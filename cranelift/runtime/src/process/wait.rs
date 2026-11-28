//! Non-blocking and pidfd-backed process wait/reap loops.

use super::{exit_code, process_handles};
use crate::blocking;
use crate::fdio;
use crate::handle_registry::{self, HandleKind};
use crate::netpoll;
use std::os::unix::io::RawFd;
#[cfg(test)]
use std::sync::atomic::Ordering;

/// what a first, non-blocking look at a child found.
enum ExitState {
    /// it is already over; this is its exit code.
    Finished(i64),
    /// still running, and this fd becomes readable when that changes.
    Watch(RawFd),
    /// still running with nothing to watch it by: the caller has to poll.
    Unwatchable,
    /// the handle is not ours (closed under us, or never existed).
    Unknown,
}

/// an fd that becomes readable when the process `pid` exits, or `None` when we
/// cannot get one.
///
/// `pidfd_open` is linux 5.3 and newer. an older kernel answers ENOSYS and we
/// fall back to `poll_until_exit`.
#[cfg(target_os = "linux")]
fn watch_pid(pid: u32) -> Option<RawFd> {
    #[cfg(test)]
    if NO_PIDFD_FOR_TESTS.load(Ordering::Relaxed) {
        return None;
    }
    // SAFETY: pidfd_open takes a pid and a flags word and returns an fd or -1.
    // the pid is live: the caller holds the registry lock and has just seen the
    // child unreaped, so the number cannot have been recycled.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0 as libc::c_uint) };
    if fd < 0 {
        return None;
    }
    Some(fd as RawFd)
}

/// makes `watch_pid` answer as a kernel without pidfds would, so tests can
/// take the path macOS and linux before 5.3 take.
#[cfg(test)]
pub(crate) static NO_PIDFD_FOR_TESTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(not(target_os = "linux"))]
fn watch_pid(_pid: u32) -> Option<RawFd> {
    // no pidfd off linux, and no epoll reactor either (see `netpoll_fallback`),
    // so there would be nothing to hand the wait to even if there were.
    None
}

/// look at a child without blocking, and set up a way to be told when it ends.
///
/// this is one lock hold on purpose: the pidfd has to be opened while the child
/// is known to be unreaped, or the pid it names could already belong to someone
/// else. the lock is released before the wait itself, which is the part that
/// must not hold it: a task parked with the registry locked would wedge every
/// other process call on that worker, and at one worker that is a deadlock.
fn exit_state(handle: i64) -> ExitState {
    let mut handles = process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return ExitState::Unknown;
    };
    match entry.child.try_wait() {
        // already exited, or already waited for once: `try_wait` remembers the
        // status, so a second wait on the same handle costs nothing.
        Ok(Some(status)) => ExitState::Finished(exit_code(status)),
        Ok(None) => match watch_pid(entry.child.id()) {
            Some(fd) => ExitState::Watch(fd),
            None => ExitState::Unwatchable,
        },
        // a child that cannot be polled cannot be waited for either, so this
        // is the -1 a blocking wait would have come back with.
        Err(_) => ExitState::Finished(-1),
    }
}

/// the wait off a green task. it blocks the thread, as an os thread may, but
/// never while holding the registry lock: a reader on another thread takes
/// that lock to drain the child's pipes, and a child stuck on a full pipe
/// never exits, so a wait that kept the lock would never return (and a kill
/// from a deadline could not get in either).
fn wait_off_green(handle: i64) -> i64 {
    loop {
        match exit_state(handle) {
            ExitState::Finished(code) => return code,
            ExitState::Unknown => return -1,
            ExitState::Watch(fd) => {
                let mut pollfd = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = {
                    let _native = crate::cycle::native_bracket();
                    // SAFETY: one valid pollfd naming the pidfd opened above.
                    unsafe { libc::poll(&mut pollfd, 1, -1) }
                };
                let interrupted = ready < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
                // SAFETY: closing the pidfd we opened and have not shared.
                unsafe { libc::close(fd) };
                if interrupted {
                    continue;
                }
                return reap(handle);
            }
            // no pidfd (an old kernel, or not linux)
            ExitState::Unwatchable => return poll_until_exit(handle),
        }
    }
}

/// collect the exit status of a child that has already ended. never holds the
/// global handle lock across a blocking wait: if the child has not exited yet
/// (for instance after a spurious/failed pidfd wait), falls back to
/// `poll_until_exit` which releases the lock between checks.
fn reap(handle: i64) -> i64 {
    {
        let mut handles = process_handles().lock();
        let Some(entry) = handles.get_mut(&handle) else {
            return -1;
        };
        match entry.child.try_wait() {
            Ok(Some(status)) => return exit_code(status),
            Ok(None) => {}
            Err(_) => return -1,
        }
    }
    poll_until_exit(handle)
}

/// wait for a child there is no pidfd for: look under the registry lock, and
/// sleep between looks with the lock released. a wait that held the lock
/// across the child's life would shut out a reader draining the child's
/// pipes, and a child stuck on a full pipe never exits. the sleep parks a
/// green task on the reactor's timer and blocks any other thread.
pub(super) fn poll_until_exit(handle: i64) -> i64 {
    loop {
        {
            let mut handles = process_handles().lock();
            let Some(entry) = handles.get_mut(&handle) else {
                return -1;
            };
            match entry.child.try_wait() {
                Ok(Some(status)) => return exit_code(status),
                Ok(None) => {}
                Err(_) => return -1,
            }
        }
        crate::platform::pith_sleep(2);
    }
}

/// Wait for a spawned process to finish, returns exit code
///
/// Off a green task this is a plain blocking wait. On one, the
/// task parks on a pidfd in the epoll reactor and its worker runs everything
/// else until the child exits.
#[no_mangle]
pub extern "C" fn pith_process_wait(handle: i64) -> i64 {
    if !handle_registry::is_valid_id(handle, HandleKind::Process) {
        return -1;
    }
    // off a green task there is no worker to keep free and blocking is what an
    // OS thread is for, but not with the registry locked; see `wait_off_green`.
    if !blocking::offloads() {
        return wait_off_green(handle);
    }
    let watch = match exit_state(handle) {
        ExitState::Finished(code) => return code,
        ExitState::Unknown => return -1,
        ExitState::Unwatchable => return poll_until_exit(handle),
        ExitState::Watch(fd) => fd,
    };

    // suspend the coroutine until the child exits. a wait that ends any other
    // way (an epoll error) just falls through to a blocking reap, which is
    // still correct, only slower than we wanted.
    fdio::wait_ready(watch as i64, true, -1);
    // drop the registration before the fd goes away, so the reactor is not left
    // holding a number the kernel can hand to the next socket we open.
    netpoll::on_close(watch);
    // SAFETY: closing the pidfd we opened above and have not shared.
    unsafe { libc::close(watch) };

    reap(handle)
}
