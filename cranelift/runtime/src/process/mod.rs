//! spawning, waiting on, and killing child processes.
//!
//! ## why none of this parks a green worker
//!
//! a green worker OS thread runs many tasks, so a syscall it sleeps in stalls
//! all of them. waiting on a child is the worst case of that: it is unbounded,
//! since the child decides when it ends.
//!
//! child processes have a nice property here that files do not: everything
//! worth waiting for is pollable. the pipes are pipes, and linux will hand out
//! a `pidfd` that becomes readable exactly when the child exits. so both waits
//! go to the epoll reactor that already serves sockets (`netpoll`), and no
//! green worker is parked on a spawned child anywhere in the runtime.
//!
//! the run-to-completion calls (`process.output`, `exec`) cannot use the
//! reactor: `Command::output` and `Command::status` spawn, drain, and wait in
//! one opaque std call. those go to a small thread pool instead, the same
//! shape dns and file i/o use: see `command_output`/`command_status` and the
//! rule in `blocking` about what may cross to a pool thread. the `Command`
//! going in and the `Output` coming back are plain owned data; every handle,
//! `Bytes`, and C string is still built on the calling task's thread.

use crate::blocking::{self, Pool};
use crate::concurrency::scheduler::{backend, Backend};
use crate::fd_handle::{self, FdKind};
use crate::fdio;
use crate::handle_registry::{self, HandleKind};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::os::unix::io::{IntoRawFd, RawFd};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;

/// the threads that run whole-child commands (`Command::output`/`status`) for
/// green tasks. a child decides for itself when it exits, so one of these can
/// be held for an unbounded time: its own small pool keeps that from queueing
/// behind (or in front of) file i/o or dns.
static POOL: Pool = Pool::new("pith-proc", 4);

/// run `command.output()` (spawn, drain both pipes, wait) without holding a
/// green worker for the child's lifetime. off a green task it runs inline.
/// `None` is any spawn/read failure.
pub(crate) fn command_output(mut command: Command) -> Option<std::process::Output> {
    blocking::run(&POOL, move || command.output().ok())
}

/// run `command.status()` (spawn and wait, stdio inherited) with the same
/// offload-or-inline split as `command_output`.
pub(crate) fn command_status(mut command: Command) -> Option<std::process::ExitStatus> {
    blocking::run(&POOL, move || command.status().ok())
}

/// a child's exit status as one number: its exit code, or 128 plus the signal
/// number when a signal ended it, which is what a shell reports. a child that
/// was killed (sigkill from the oom killer, a segfault) must not read as a
/// clean exit, and every path that reports a status goes through this so they
/// agree.
pub(crate) fn exit_code(status: std::process::ExitStatus) -> i64 {
    use std::os::unix::process::ExitStatusExt;

    match (status.code(), status.signal()) {
        (Some(code), _) => code as i64,
        (None, Some(signal)) => 128 + signal as i64,
        (None, None) => -1,
    }
}

/// which of a child's three pipes a caller wants. the discriminant indexes
/// `ProcessHandle::pipes`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pipe {
    Stdin = 0,
    Stdout = 1,
    Stderr = 2,
}

/// every pipe, for the paths that have to touch all three.
const PIPES: [Pipe; 3] = [Pipe::Stdin, Pipe::Stdout, Pipe::Stderr];

pub(crate) struct ProcessHandle {
    pub(crate) child: Child,
    /// our end of each captured pipe, as an `fd_handle` handle rather than the
    /// `ChildStdin`/`ChildStdout` std hands out: `process_io` takes a hold on
    /// the handle for each read or write, outside the registry lock, and a
    /// close from another task retires the handle rather than closing a number
    /// a read is still inside on. `None` for a pipe that was not captured.
    pub(super) pipes: [Option<i64>; 3],
}

impl ProcessHandle {
    /// the handle of one of the child's pipes, or `None` if it was not
    /// captured. the caller takes its own hold on it (`fd_handle::acquire`),
    /// which is what keeps the descriptor alive across its syscall.
    pub(crate) fn pipe(&self, pipe: Pipe) -> Option<i64> {
        self.pipes[pipe as usize]
    }
}

/// register our end of a captured pipe as a handle, non-blocking under green.
///
/// under green the pipes have to be non-blocking, or a read with nothing on
/// the other end sleeps the worker instead of yielding to the reactor. this
/// is our end of each pipe only: the child's end is a separate open file
/// description and keeps the blocking semantics it expects. the os-thread
/// backend leaves them alone: there is no worker to protect and a blocking
/// read is exactly what it wants.
pub(super) fn register_pipe(fd: Option<RawFd>) -> Option<i64> {
    let fd = fd?;
    if backend() == Backend::Green {
        fdio::set_nonblocking(fd);
    }
    let handle = fd_handle::open(fd, FdKind::Pipe);
    if handle == 0 {
        // SAFETY: closing a pipe end we own that never became a handle.
        unsafe { libc::close(fd) };
        return None;
    }
    Some(handle)
}

pub(super) struct ProcessOutputHandle {
    pub(super) status: i64,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

static PROCESS_HANDLES: OnceLock<Mutex<HashMap<i64, ProcessHandle>>> = OnceLock::new();
pub(super) static NEXT_PROCESS_HANDLE: AtomicI64 = AtomicI64::new(1);
static PROCESS_OUTPUT_HANDLES: OnceLock<Mutex<HashMap<i64, ProcessOutputHandle>>> = OnceLock::new();
pub(super) static NEXT_PROCESS_OUTPUT_HANDLE: AtomicI64 = AtomicI64::new(1);

/// children whose handle was closed while they still ran. nobody can wait for
/// them any more, and an exited child nobody waits for stays a zombie for the
/// life of this process, so each later spawn and close reaps the ones that
/// have exited since.
pub(super) static UNREAPED: Mutex<Vec<Child>> = Mutex::new(Vec::new());

pub(super) fn reap_unreaped() {
    // an error (no such child, say) means there is nothing left to reap
    UNREAPED.lock().retain_mut(|child| matches!(child.try_wait(), Ok(None)));
}

pub(crate) fn process_handles() -> &'static Mutex<HashMap<i64, ProcessHandle>> {
    PROCESS_HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn process_output_handles() -> &'static Mutex<HashMap<i64, ProcessOutputHandle>> {
    PROCESS_OUTPUT_HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}
pub(super) fn pith_store_process_handle(mut child: Child) -> i64 {
    // a write to the stdin of a child that has exited raises SIGPIPE, which
    // would kill this process where `process_write` should report `EPIPE`.
    // see signals::ignore_sigpipe. the child itself starts with the default
    // disposition: std resets SIGPIPE before it execs.
    crate::signals::ignore_sigpipe();
    reap_unreaped();
    let handle = NEXT_PROCESS_HANDLE.fetch_add(1, Ordering::Relaxed);
    // the pipes are taken out of the `Child` here and owned as handles from
    // now on: `Child` never closes them again, and neither does `wait`.
    let entry = ProcessHandle {
        pipes: [
            register_pipe(child.stdin.take().map(IntoRawFd::into_raw_fd)),
            register_pipe(child.stdout.take().map(IntoRawFd::into_raw_fd)),
            register_pipe(child.stderr.take().map(IntoRawFd::into_raw_fd)),
        ],
        child,
    };
    process_handles().lock().insert(handle, entry);
    handle_registry::register_id(handle, HandleKind::Process);
    handle
}
/// Send a kill signal to a process
#[no_mangle]
pub extern "C" fn pith_process_kill(handle: i64) -> i64 {
    if !handle_registry::is_valid_id(handle, HandleKind::Process) {
        return 0;
    }
    let mut handles = process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return 0;
    };
    match entry.child.kill() {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

/// close our end of a child's stdin pipe, so a child that reads to the end of
/// its input (`sort`, `wc`) sees end of file. returns 1 when this call closed
/// it, 0 when it was already closed, never captured, or the handle is unknown,
/// so a second close is harmless. a later write fails as for a child with no
/// stdin pipe.
#[no_mangle]
pub extern "C" fn pith_process_close_stdin(handle: i64) -> i64 {
    if !handle_registry::is_valid_id(handle, HandleKind::Process) {
        return 0;
    }
    // take the handle out under the lock and retire it after, as
    // `pith_process_close` does, so a parked writer is woken without the lock.
    let pipe_handle = match process_handles().lock().get_mut(&handle) {
        Some(entry) => entry.pipes[Pipe::Stdin as usize].take(),
        None => None,
    };
    match pipe_handle {
        Some(pipe_handle) => i64::from(fd_handle::close(pipe_handle)),
        None => 0,
    }
}

/// Close and forget a process handle
#[no_mangle]
pub extern "C" fn pith_process_close(handle: i64) {
    // take the entry out under the lock but close its pipes after, so the
    // closes run outside the lock and a task parked on one of them can be
    // woken without contending for it.
    let entry = process_handles().lock().remove(&handle);
    handle_registry::unregister_id(handle, HandleKind::Process);

    // retiring a pipe's handle is the same teardown a socket gets in
    // `pith_tcp_close`: a task parked on it in the reactor is woken with a
    // closed outcome and its read reports an error, a read blocked in the
    // kernel keeps the number reserved until it returns, and the descriptor
    // is closed once no call is inside, never while one is.
    if let Some(entry) = &entry {
        for pipe in PIPES {
            if let Some(pipe_handle) = entry.pipe(pipe) {
                fd_handle::close(pipe_handle);
            }
        }
    }
    // a close does not stop the child, but once it exits someone has to reap
    // it. one already reaped (`wait` ran, or `try_wait` saw it exit) is done.
    if let Some(mut entry) = entry {
        if let Ok(None) = entry.child.try_wait() {
            UNREAPED.lock().push(entry.child);
        }
    }
    reap_unreaped();
}

pub mod io;
mod spawn;
mod wait;
#[cfg(test)]
mod tests;

pub use io::*;
pub use spawn::*;
pub use wait::*;
