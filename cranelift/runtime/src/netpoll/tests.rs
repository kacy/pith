use super::epoll::*;
use super::registry::*;
use super::*;
use std::collections::BinaryHeap;

#[test]
fn deadline_ordering_is_soonest_first() {
    let now = Instant::now();
    let mut heap: BinaryHeap<std::cmp::Reverse<Deadline>> = BinaryHeap::new();
    heap.push(std::cmp::Reverse(Deadline {
        at: now + Duration::from_millis(50),
        seq: 1,
        target: DeadlineTarget::Io {
            fd: 3,
            interest: Interest::Read,
        },
    }));
    heap.push(std::cmp::Reverse(Deadline {
        at: now + Duration::from_millis(10),
        seq: 2,
        target: DeadlineTarget::Timer,
    }));
    // the nearer deadline (10ms) must come out first.
    assert_eq!(heap.pop().unwrap().0.seq, 2);
    assert_eq!(heap.pop().unwrap().0.seq, 1);
}

// close-during-wait: a task blocked reading an fd that another task closes
// must be woken with a CLOSED outcome (so its wait_io returns an error and the
// task unwinds and reclaims its stack/slot), not silently dropped and parked
// forever. we register a waiter by hand and drive on_close directly, since
// wait_io itself needs a live coroutine to suspend into. the fd is a bogus
// value that no real socket test uses, so the epoll DEL just fails harmlessly.
#[test]
fn on_close_wakes_parked_waiter_with_closed_outcome() {
    let poller = poller(0);
    let fd: RawFd = 424_242;
    let outcome = Arc::new(AtomicI32::new(PENDING));
    {
        let mut inner = lock_inner(poller);
        let seq = inner.next_seq;
        inner.next_seq += 1;
        inner.push_waiter(
            fd,
            Interest::Read,
            Waiter {
                // a task id far beyond any real task: green::wake finds no
                // such block and no-ops, which is all we need to exercise
                // on_close.
                task: usize::MAX,
                seq,
                outcome: outcome.clone(),
            },
        );
        inner.fds.insert(fd, FdReg { ready: 0 });
    }

    on_close(fd);

    // the waiter's outcome was flipped to CLOSED (wait_io maps that to -1), and
    // the registry no longer holds the waiter or the fd.
    assert_eq!(outcome.load(AtomicOrdering::Acquire), CLOSED);
    let inner = lock_inner(poller);
    assert!(inner.waiters.get(&(fd, Interest::Read)).is_none());
    assert!(inner.fds.get(&fd).is_none());
}

// the two timer tests share the one process-global registry, so they must
// not interleave with each other (cargo runs tests concurrently).
static TIMER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// a timer registration must land in the timers map and the deadline heap,
// and, once the sweep runs past it, resolve to TIMED_OUT with the entry
// gone and no fd touched. driven by hand (register_timer + collect_expired)
// because a real sleep_task needs a live coroutine to suspend into.
#[test]
fn timer_registers_expires_and_wakes() {
    let _guard = TIMER_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let poller = poller(0);
    let outcome = Arc::new(AtomicI32::new(PENDING));
    let at = Instant::now() + Duration::from_millis(5);
    register_timer(poller, at, usize::MAX, &outcome);

    // identify our entry by its shared outcome cell, not by map emptiness:
    // the registry is global and other tests may hold entries of their own.
    let ours = |inner: &Inner| {
        inner
            .timers
            .values()
            .any(|w| Arc::ptr_eq(&w.outcome, &outcome))
    };

    let mut to_wake = Vec::new();
    {
        let mut inner = lock_inner(poller);
        assert!(ours(&inner));

        // not due yet: the sweep leaves it alone.
        collect_expired(&mut inner, at - Duration::from_millis(1), &mut to_wake);
        assert!(to_wake.is_empty());
        assert_eq!(outcome.load(AtomicOrdering::Acquire), PENDING);

        // due: it resolves to TIMED_OUT, queues the task, and cleans up.
        collect_expired(&mut inner, at, &mut to_wake);
        assert!(!ours(&inner));
    }
    assert_eq!(to_wake, vec![usize::MAX]);
    assert_eq!(outcome.load(AtomicOrdering::Acquire), TIMED_OUT);
}

// on_close must only close out a *pending* wait: a waiter a readiness or
// timeout already claimed keeps its terminal outcome, so a close racing a
// fire never double-resolves the wait.
#[test]
fn on_close_leaves_already_resolved_waiter() {
    let poller = poller(0);
    let fd: RawFd = 424_243;
    let outcome = Arc::new(AtomicI32::new(READY));
    {
        let mut inner = lock_inner(poller);
        let seq = inner.next_seq;
        inner.next_seq += 1;
        inner.push_waiter(
            fd,
            Interest::Write,
            Waiter {
                task: usize::MAX,
                seq,
                outcome: outcome.clone(),
            },
        );
    }

    on_close(fd);

    // still READY: on_close's compare_exchange only fires on a PENDING waiter.
    assert_eq!(outcome.load(AtomicOrdering::Acquire), READY);
}

fn socket_pair() -> (RawFd, RawFd) {
    let mut fds = [0; 2];
    // SAFETY: socketpair writes two fds into the array we pass.
    let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    assert_eq!(rc, 0);
    (fds[0], fds[1])
}

#[test]
fn deadlines_of_finished_waits_do_not_pile_up() {
    let mut inner = Inner::new();
    let far = Instant::now() + Duration::from_secs(60);
    let fd: RawFd = 424_250;
    // one wait stays pending the whole time, so its deadline must survive
    // every compaction.
    let kept = inner.next_seq;
    inner.next_seq += 1;
    inner.push_waiter(
        fd + 1,
        Interest::Read,
        Waiter {
            task: usize::MAX,
            seq: kept,
            outcome: Arc::new(AtomicI32::new(PENDING)),
        },
    );
    inner.push_deadline(Deadline {
        at: far,
        seq: kept,
        target: DeadlineTarget::Io { fd: fd + 1, interest: Interest::Read },
    });
    for _ in 0..20_000 {
        // a wait that registers a deadline and then fires ready.
        add_read_waiter(&mut inner, fd);
        let seq = inner.next_seq - 1;
        inner.push_deadline(Deadline {
            at: far,
            seq,
            target: DeadlineTarget::Io { fd, interest: Interest::Read },
        });
        inner.waiters.remove(&(fd, Interest::Read));
    }
    assert!(inner.deadlines.len() <= DEADLINE_COMPACT_MIN, "{} entries", inner.deadlines.len());
    assert!(inner.deadlines.iter().any(|entry| entry.0.seq == kept));
}

fn add_read_waiter(inner: &mut Inner, fd: RawFd) {
    let seq = inner.next_seq;
    inner.next_seq += 1;
    inner.push_waiter(
        fd,
        Interest::Read,
        Waiter {
            task: usize::MAX,
            seq,
            outcome: Arc::new(AtomicI32::new(PENDING)),
        },
    );
}

/// what a poll does, minus the wait: collect the events queued on
/// `poller` and hand each fd's to `take_event`, returning the tasks to wake.
fn harvest(poller: &Poller, inner: &mut Inner) -> Vec<usize> {
    let mut ev = [libc::epoll_event { events: 0, u64: 0 }; 8];
    // SAFETY: a zero-timeout wait on our own epoll fd into a local buffer.
    let n = unsafe { libc::epoll_wait(poller.epfd, ev.as_mut_ptr(), 8, 0) };
    let mut to_wake = Vec::new();
    for e in &ev[..n.max(0) as usize] {
        if e.u64 != EVENTFD_TOKEN {
            take_event(inner, e.u64 as RawFd, e.events, &mut to_wake);
        }
    }
    to_wake
}

/// register a wait by task id; returns whether the task would park.
fn wait(poller: &Poller, inner: &mut Inner, fd: RawFd, interest: Interest, task: usize) -> bool {
    matches!(
        register_io(poller, inner, fd, interest, None, task),
        RegisterOutcome::Parked(_, _)
    )
}

fn send_byte(fd: RawFd) {
    // SAFETY: writing one byte to a socket the test owns.
    assert_eq!(unsafe { libc::write(fd, b"x".as_ptr().cast(), 1) }, 1);
}

fn close_fds(fds: &[RawFd]) {
    for &fd in fds {
        // SAFETY: closing fds the test opened.
        unsafe { libc::close(fd) };
    }
}

// an edge harvested while no task waits must not be lost: it leaves a
// readiness bit, the next wait consumes it instead of parking, and the
// wait after that parks again. the fd is added to epoll once, on its
// first wait, and never re-armed.
#[test]
fn an_edge_before_the_wait_is_not_lost() {
    let poller = Poller::new();
    let (a, b) = socket_pair();
    let mut inner = lock_inner(&poller);
    assert!(wait(&poller, &mut inner, a, Interest::Read, 1));
    send_byte(b);
    assert_eq!(harvest(&poller, &mut inner), vec![1]);
    assert_eq!(inner.waiting, 0);

    // data arrives while the task is running, not waiting.
    send_byte(b);
    assert!(harvest(&poller, &mut inner).is_empty());
    assert!(!wait(&poller, &mut inner, a, Interest::Read, 1), "the bit ends the wait");
    assert_eq!(inner.waiting, 0);
    assert!(wait(&poller, &mut inner, a, Interest::Read, 1), "the bit is consumed");
    assert_eq!(inner.waiting, 1);

    // the fd stays registered: a new edge still reaches the waiter.
    send_byte(b);
    assert_eq!(harvest(&poller, &mut inner), vec![1]);
    drop(inner);
    close_fds(&[a, b]);
}

// a reader and a writer on one fd: each edge wakes only the direction it
// reports, and a direction nobody waits on keeps its bit for later.
#[test]
fn a_reader_and_a_writer_share_an_fd() {
    let poller = Poller::new();
    let (a, b) = socket_pair();
    let mut inner = lock_inner(&poller);
    assert!(wait(&poller, &mut inner, a, Interest::Read, 1));
    assert!(wait(&poller, &mut inner, a, Interest::Write, 2));
    // the add reports the socket as already writable, which wakes the
    // writer and leaves the reader parked.
    assert_eq!(harvest(&poller, &mut inner), vec![2]);
    assert_eq!(inner.waiting, 1);

    send_byte(b);
    assert_eq!(harvest(&poller, &mut inner), vec![1]);
    // that edge also reported the socket writable, with no writer waiting.
    let write_bit = Interest::Write.epoll_bit();
    assert_eq!(inner.fds.get(&a).map(|reg| reg.ready & write_bit), Some(write_bit));
    assert!(!wait(&poller, &mut inner, a, Interest::Write, 2));
    drop(inner);
    close_fds(&[a, b]);
}

// a close wakes the fd's waiters and takes it out of the set, with any
// readiness bits it had, so a new fd that reuses the number is added again
// on its first wait and reported.
#[test]
fn a_closed_fd_number_is_registered_again() {
    let poller = Poller::new();
    let (a, b) = socket_pair();
    let mut inner = lock_inner(&poller);
    let RegisterOutcome::Parked(_, outcome) =
        register_io(&poller, &mut inner, a, Interest::Read, None, 1)
    else {
        panic!("expected wait to park");
    };
    // the add reports the socket writable: a write bit with no writer.
    assert!(harvest(&poller, &mut inner).is_empty());
    drop(inner);

    let mut to_wake = Vec::new();
    close_in(&poller, a, &mut to_wake);
    assert_eq!(to_wake, vec![1]);
    assert_eq!(outcome.load(AtomicOrdering::Acquire), CLOSED);
    close_fds(&[a, b]);

    let (c, d) = socket_pair();
    let mut inner = lock_inner(&poller);
    assert!(!inner.fds.contains_key(&c));
    assert_eq!(inner.waiting, 0);
    assert!(wait(&poller, &mut inner, c, Interest::Write, 3), "no bit survives the close");
    assert_eq!(harvest(&poller, &mut inner), vec![3]);
    assert!(wait(&poller, &mut inner, c, Interest::Read, 4));
    send_byte(d);
    assert_eq!(harvest(&poller, &mut inner), vec![4]);
    drop(inner);
    close_fds(&[c, d]);
}

#[test]
fn failed_epoll_add_returns_failed_without_parking_waiter() {
    let poller = Poller::new();
    let mut inner = lock_inner(&poller);
    let reg = register_io(&poller, &mut inner, -1, Interest::Read, None, 99);
    assert!(matches!(reg, RegisterOutcome::Failed));
    assert_eq!(inner.waiting, 0);
    assert!(!inner.fds.contains_key(&-1));
}
