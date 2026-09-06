//! fd-backed handles that carry liveness.
//!
//! a socket or a child's pipe used to reach the language as its raw descriptor
//! number, and the number is the whole problem. the kernel hands a closed fd's
//! number to the very next `open`, `accept` or `pipe` anywhere in the process,
//! so a task still holding the old number talks to whatever took it: it reads
//! another connection's bytes, or writes over them, with nothing to object.
//! a close racing a read on the same connection is the ordinary way to get
//! there, and every std type that wraps a socket coordinated around it by hand
//! (`shutdown` first, wait for the reader, then close).
//!
//! the handle the language holds is now `(generation << 32) | fd`: the number
//! plus a per-number generation that changes every time the number is opened,
//! the same shape task handles have. a stale handle names a generation that is
//! gone, so a call on it fails with an ordinary error instead of reaching the
//! syscall. that alone leaves a window between the check and the syscall, so
//! the handle also counts its users: a call holds a `Guard` for the whole of
//! its syscall, including any park in the reactor, and a close that finds
//! users still inside marks the handle dead, wakes them, and leaves the
//! `close(2)` itself to the last one out. the number therefore cannot be
//! recycled while any syscall is in flight on it, and a syscall never lands on
//! an fd the handle did not name.
//!
//! ## the slot
//!
//! one `AtomicU64` per fd number, allocated in chunks on first use, holds the
//! number's whole state:
//!
//! ```text
//!   bits 62..32  generation of the most recent open on this number
//!   bit  31      live: the handle for that generation has not been closed
//!   bit  30      kind: 0 a socket, 1 a pipe
//!   bit  29      opened: the number has had a handle at some point
//!   bits 28..0   users: calls currently holding a guard
//! ```
//!
//! every transition is one compare-exchange on that word, which is what keeps
//! a close and a release racing on the same number to exactly one `close(2)`.
//! the check on the hot path is one load and one compare-exchange to take the
//! guard, and one compare-exchange to drop it.
//!
//! a handle's generation starts at zero, so the first handle on a number is
//! the number itself — the encoding task handles use for the same reason —
//! except on fd 0, whose handle would be the "no handle" value and so starts
//! at one. generations wrap at 2^31 and never collide in practice: a number
//! would have to be opened two billion times while one stale handle was kept.
//!
//! regular files are not here. `host_fs` keys them by a counter that never
//! recycles and holds the `File` itself, so a stale file handle already fails
//! to resolve; and fd 0, 1 and 2 belong to the process for its whole life and
//! are never closed, so the terminal reads them raw.

use crate::netpoll;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

/// what an fd is, which is to say which syscalls are allowed on it. a handle
/// of one kind handed to the other kind's entry point is refused as stale
/// rather than reaching a syscall that would report `ENOTSOCK` or worse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FdKind {
    Socket,
    Pipe,
}

const GEN_SHIFT: u32 = 32;
/// 31 bits of generation keep the handle's sign bit clear: every handle is a
/// positive `i64`, and 0 stays "no handle" as it always was.
const GEN_MASK: u64 = 0x7fff_ffff;
const LIVE: u64 = 1 << 31;
const PIPE: u64 = 1 << 30;
/// set by the first open on a number and never cleared, so a closed
/// generation-zero slot is not mistaken for a number that was never opened
/// (both would otherwise be all zeros) and the next open bumps the generation.
const OPENED: u64 = 1 << 29;
const USERS_MASK: u64 = (1 << 29) - 1;
/// the fd half of a handle. a descriptor is a non-negative `int`.
const FD_MASK: u64 = 0x7fff_ffff;

/// slots are allocated in chunks of this many fd numbers, on the first open of
/// a number in the chunk.
const CHUNK: usize = 4096;
/// the table covers this many chunks: 16 million fd numbers, past any
/// `RLIMIT_NOFILE` a process here will see. an fd beyond it is refused at open.
const CHUNKS: usize = 4096;

static TABLE: [AtomicPtr<AtomicU64>; CHUNKS] = [const { AtomicPtr::new(std::ptr::null_mut()) }; CHUNKS];

/// the slot for `fd`, or `None` when it was never opened (a lookup does not
/// allocate: a handle on a number that never had a chunk is stale by
/// definition).
fn slot(fd: usize) -> Option<&'static AtomicU64> {
    let chunk = TABLE.get(fd / CHUNK)?.load(Ordering::Acquire);
    if chunk.is_null() {
        return None;
    }
    // SAFETY: a chunk pointer, once published, is a leaked `Box<[AtomicU64; CHUNK]>`
    // that lives for the rest of the process; `fd % CHUNK` is in bounds.
    Some(unsafe { &*chunk.add(fd % CHUNK) })
}

/// the slot for `fd`, allocating its chunk if this is the first open there.
/// `None` only for a number past the table.
fn slot_or_allocate(fd: usize) -> Option<&'static AtomicU64> {
    let head = TABLE.get(fd / CHUNK)?;
    let mut chunk = head.load(Ordering::Acquire);
    if chunk.is_null() {
        let fresh: Box<[AtomicU64]> = (0..CHUNK).map(|_| AtomicU64::new(0)).collect();
        let fresh = Box::into_raw(fresh) as *mut AtomicU64;
        match head.compare_exchange(
            std::ptr::null_mut(),
            fresh,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => chunk = fresh,
            Err(published) => {
                // another opener published a chunk first; ours is not needed.
                // SAFETY: `fresh` came from `Box::into_raw` above and was never shared.
                drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(fresh, CHUNK)) });
                chunk = published;
            }
        }
    }
    // SAFETY: as in `slot`.
    Some(unsafe { &*chunk.add(fd % CHUNK) })
}

fn generation(word: u64) -> u64 {
    (word >> GEN_SHIFT) & GEN_MASK
}

fn users(word: u64) -> u64 {
    word & USERS_MASK
}

fn kind_bit(kind: FdKind) -> u64 {
    match kind {
        FdKind::Socket => 0,
        FdKind::Pipe => PIPE,
    }
}

/// split a handle into its fd and generation. `None` for zero, a negative
/// value, or anything with the sign bit set, none of which name a descriptor.
fn split(handle: i64) -> Option<(usize, u64)> {
    if handle <= 0 {
        return None;
    }
    let bits = handle as u64;
    Some(((bits & FD_MASK) as usize, (bits >> GEN_SHIFT) & GEN_MASK))
}

fn make_handle(fd: usize, generation: u64) -> i64 {
    ((generation << GEN_SHIFT) | fd as u64) as i64
}

/// register a descriptor pith just opened and return its handle: the fd
/// stamped with the next generation for that number. `0` — no handle — when the
/// fd is out of range, in which case the caller still owns it and closes it.
///
/// the number's previous handle, if it had one, is necessarily dead by now:
/// the kernel only reuses a number after its `close(2)`, and the only
/// `close(2)` on a registered number is the one this module runs when its
/// handle retires. an os-thread reader still blocked on the old description
/// keeps that description alive in the kernel, not the number.
pub(crate) fn open(fd: RawFd, kind: FdKind) -> i64 {
    if fd < 0 {
        return 0;
    }
    let fd = fd as usize;
    let Some(slot) = slot_or_allocate(fd) else {
        return 0;
    };
    let mut word = slot.load(Ordering::Acquire);
    loop {
        let mut generation = if word & OPENED == 0 {
            0
        } else {
            (generation(word) + 1) & GEN_MASK
        };
        if make_handle(fd, generation) == 0 {
            // fd 0 at generation 0 would be the "no handle" value: a process
            // that closed its stdin and then opened a socket gets it. skip to
            // the first generation that names it.
            generation = 1;
        }
        let fresh = (generation << GEN_SHIFT) | LIVE | OPENED | kind_bit(kind);
        match slot.compare_exchange_weak(word, fresh, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return make_handle(fd, generation),
            Err(current) => word = current,
        }
    }
}

/// a call's hold on a live handle: the fd it resolved to, valid for exactly as
/// long as the guard lives. dropping it releases the hold and, if the handle
/// was closed meanwhile and this was the last user, runs the `close(2)` the
/// closer deferred.
pub(crate) struct Guard {
    fd: RawFd,
    handle: i64,
}

impl Guard {
    /// the descriptor. the guard being alive is what makes it the right one.
    pub(crate) fn fd(&self) -> RawFd {
        self.fd
    }

    /// has the handle been closed since the guard was taken? a call that wakes
    /// from a park asks this before retrying its syscall, so a close that
    /// landed while it slept ends the call with an error rather than a read
    /// of a socket that has been shut down.
    pub(crate) fn is_live(&self) -> bool {
        is_live(self.handle)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        release(self.fd, self.handle);
    }
}

/// take a hold on `handle` for a syscall of `kind`. `None` when the handle is
/// stale — closed, never opened, of the other kind, or not a handle at all —
/// which the caller reports as an ordinary failed call.
pub(crate) fn acquire(handle: i64, kind: FdKind) -> Option<Guard> {
    let (fd, generation) = split(handle)?;
    let slot = slot(fd)?;
    let mut word = slot.load(Ordering::Acquire);
    loop {
        if generation != self::generation(word)
            || word & LIVE == 0
            || word & PIPE != kind_bit(kind)
            || users(word) == USERS_MASK
        {
            return None;
        }
        match slot.compare_exchange_weak(word, word + 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                return Some(Guard {
                    fd: fd as RawFd,
                    handle,
                })
            }
            Err(current) => word = current,
        }
    }
}

/// is `handle` open? true between its `open` and its `close`, whatever calls
/// are in flight on it.
pub(crate) fn is_live(handle: i64) -> bool {
    let Some((fd, generation)) = split(handle) else {
        return false;
    };
    let Some(slot) = slot(fd) else {
        return false;
    };
    let word = slot.load(Ordering::Acquire);
    generation == self::generation(word) && word & LIVE != 0
}

/// drop one hold. the user count reaching zero on a handle that has been
/// closed is the deferred close: this user was the one keeping the number
/// reserved, so it runs the `close(2)`.
fn release(fd: RawFd, handle: i64) {
    let Some((index, generation)) = split(handle) else {
        return;
    };
    let Some(slot) = slot(index) else {
        return;
    };
    let mut word = slot.load(Ordering::Acquire);
    loop {
        if generation != self::generation(word) || users(word) == 0 {
            // the number has moved on to another generation, or the count is
            // already zero: a guard whose slot the bookkeeping lost. there is
            // nothing safe to do with it, so it does nothing.
            return;
        }
        let next = word - 1;
        match slot.compare_exchange_weak(word, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                if next & LIVE == 0 && users(next) == 0 {
                    finish_close(fd);
                }
                return;
            }
            Err(current) => word = current,
        }
    }
}

/// the `close(2)` itself, run exactly once per handle, by whichever of the
/// closer and the last user is the one to find the count at zero with the
/// handle dead. the reactor registration goes first, so a parked waiter is
/// never left on a number the kernel is about to hand out again.
fn finish_close(fd: RawFd) {
    netpoll::on_close(fd);
    // SAFETY: closing the fd this handle owned; no guard is alive on it, so no
    // syscall is in flight, and its slot is dead, so none can start.
    unsafe {
        libc::close(fd);
    }
}

/// close `handle`. returns true when this call retired it; false for a handle
/// that is stale — already closed, or never a handle — which makes a second
/// close a no-op rather than a `close(2)` on whatever now holds the number.
///
/// with no call in flight the fd is closed here. with users inside, the handle
/// is marked dead — no new call can take a hold — and the users are woken: a
/// socket is shut down in both directions, which returns a blocked `recv`,
/// `send`, `accept` or `poll` on it with nothing, and the reactor resolves
/// every parked waiter with a closed outcome. each of those calls then sees the
/// handle is dead, fails, and drops its guard; the last one runs the close.
/// a pipe has no shutdown: an os-thread reader blocked on one keeps the number
/// reserved until its writer speaks or exits, the same time it would have
/// stayed blocked with the fd closed underneath it.
pub(crate) fn close(handle: i64) -> bool {
    let Some((fd, generation)) = split(handle) else {
        return false;
    };
    let Some(slot) = slot(fd) else {
        return false;
    };
    let mut word = slot.load(Ordering::Acquire);
    loop {
        if generation != self::generation(word) || word & LIVE == 0 {
            return false;
        }
        let next = word & !LIVE;
        match slot.compare_exchange_weak(word, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                let fd = fd as RawFd;
                if users(next) == 0 {
                    finish_close(fd);
                } else {
                    if next & PIPE == 0 {
                        // SAFETY: shutdown on an fd a live guard still holds
                        // open; it frees nothing.
                        unsafe {
                            libc::shutdown(fd, libc::SHUT_RDWR);
                        }
                    }
                    netpoll::on_close(fd);
                }
                return true;
            }
            Err(current) => word = current,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a fresh descriptor whose number the kernel will reuse: one end of a
    /// socket pair, the other end closed at once.
    fn socket_fd() -> RawFd {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: socketpair(2) fills the two-element array we pass it.
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(rc, 0);
        // SAFETY: closing an fd this test owns.
        unsafe { libc::close(fds[1]) };
        fds[0]
    }

    /// is `fd` open in the kernel?
    fn fd_is_open(fd: RawFd) -> bool {
        // SAFETY: fcntl F_GETFD on any int is safe; a closed fd answers EBADF.
        unsafe { libc::fcntl(fd, libc::F_GETFD) >= 0 }
    }

    /// pin a socket onto a high fd number that no parallel test will take,
    /// so the number can be watched across a close and a reopen.
    fn socket_on(number: RawFd) -> RawFd {
        let fd = socket_fd();
        // SAFETY: dup2 onto a number this test reserves for itself.
        assert_eq!(unsafe { libc::dup2(fd, number) }, number);
        // SAFETY: closing the original; the dup keeps the socket open.
        unsafe { libc::close(fd) };
        number
    }

    #[test]
    fn the_first_handle_on_a_number_is_the_number_itself() {
        let fd = socket_on(1201);
        let handle = open(fd, FdKind::Socket);
        assert_eq!(handle, fd as i64);
        assert!(is_live(handle));
        assert!(close(handle));
        assert!(!fd_is_open(fd));
    }

    #[test]
    fn a_live_handle_is_accepted_and_a_closed_one_refused() {
        let fd = socket_on(1202);
        let handle = open(fd, FdKind::Socket);
        assert_eq!(acquire(handle, FdKind::Socket).map(|g| g.fd()), Some(fd));
        assert!(close(handle));
        assert!(acquire(handle, FdKind::Socket).is_none());
        assert!(!is_live(handle));
        // a second close is a no-op, not a second close(2).
        assert!(!close(handle));
    }

    #[test]
    fn a_recycled_number_gets_a_new_generation_and_the_old_handle_is_refused() {
        let fd = socket_on(1203);
        let old = open(fd, FdKind::Socket);
        assert!(close(old));
        // the kernel hands the number out again; the new handle differs.
        let again = socket_on(1203);
        assert_eq!(again, fd);
        let new = open(again, FdKind::Socket);
        assert_ne!(new, old);
        assert_eq!(new & FD_MASK as i64, fd as i64);
        assert!(acquire(old, FdKind::Socket).is_none());
        assert!(!is_live(old));
        assert!(!close(old), "a stale close must not touch the new handle");
        assert!(is_live(new));
        assert_eq!(acquire(new, FdKind::Socket).map(|g| g.fd()), Some(fd));
        assert!(close(new));
    }

    #[test]
    fn a_close_with_a_user_inside_is_deferred_to_that_user() {
        let fd = socket_on(1204);
        let handle = open(fd, FdKind::Socket);
        let guard = acquire(handle, FdKind::Socket).expect("live");
        assert!(close(handle));
        // dead to new callers, and the guard sees it, but the number is still
        // reserved: the kernel has not been told.
        assert!(acquire(handle, FdKind::Socket).is_none());
        assert!(!guard.is_live());
        assert!(fd_is_open(fd));
        drop(guard);
        assert!(!fd_is_open(fd));
        // and the number, once reopened, is a fresh generation.
        let again = socket_on(1204);
        let new = open(again, FdKind::Socket);
        assert_ne!(new, handle);
        assert!(close(new));
    }

    #[test]
    fn fd_zero_gets_a_handle_that_is_not_the_no_handle_value() {
        // stdin is closed in this test binary's children only, so stand in
        // for it with a dup onto 0 here — nothing in cargo's test harness
        // reads it.
        let fd = socket_fd();
        // SAFETY: dup2 onto stdin, which this test process does not read.
        assert_eq!(unsafe { libc::dup2(fd, 0) }, 0);
        // SAFETY: closing the original; the dup keeps the socket open.
        unsafe { libc::close(fd) };
        let handle = open(0, FdKind::Socket);
        assert_ne!(handle, 0);
        assert_eq!(handle & FD_MASK as i64, 0);
        assert_eq!(acquire(handle, FdKind::Socket).map(|g| g.fd()), Some(0));
        assert!(is_live(handle));
        // the socket stays on fd 0 for the rest of the process rather than
        // being closed: a freed 0 would be handed to another test's socket
        // pair, and `fdio::wait_ready` treats 0 as "no fd".
    }

    #[test]
    fn a_handle_of_one_kind_is_refused_by_the_other() {
        let fd = socket_on(1205);
        let handle = open(fd, FdKind::Pipe);
        assert!(acquire(handle, FdKind::Socket).is_none());
        assert!(acquire(handle, FdKind::Pipe).is_some());
        assert!(close(handle));
    }

    #[test]
    fn values_that_are_not_handles_are_refused() {
        assert!(acquire(0, FdKind::Socket).is_none());
        assert!(acquire(-1, FdKind::Socket).is_none());
        assert!(acquire(i64::MAX, FdKind::Socket).is_none());
        assert!(!is_live(0));
        assert!(!close(0));
        assert!(!close(-1));
        // a number nothing was ever opened on has no slot, and no chunk is
        // allocated to find out.
        assert!(acquire((CHUNKS * CHUNK - 1) as i64, FdKind::Socket).is_none());
        assert_eq!(open(-1, FdKind::Socket), 0);
    }
}
