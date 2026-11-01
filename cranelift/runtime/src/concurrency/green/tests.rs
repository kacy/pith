use super::*;

// a pair of tasks handing off to each other on one worker refills the
// affinity slot on every exchange, which is exactly the shape that used to
// leapfrog the fifo forever: `pop` empties the slot before `push_next` puts
// the partner back, so the displace-on-occupied rule never fires. the fifo
// has to get a turn anyway.
#[test]
fn a_ping_ponging_pair_cannot_starve_the_fifo() {
    let mut q = PinnedQueue::default();
    q.push(99); // a third task, waiting behind the pair
    let mut partner = 1;
    for exchange in 0..(MAX_AFFINITY_STREAK as usize * 2) {
        q.push_next(partner);
        if q.pop() == Some(99) {
            // the fifo got its turn; the pair is still queued and running.
            assert!(exchange > 0, "the slot should win at least once first");
            return;
        }
        partner = if partner == 1 { 2 } else { 1 };
    }
    panic!("the fifo never ran in {} exchanges", MAX_AFFINITY_STREAK * 2);
}

// the streak only costs a turn when someone is actually waiting: with an
// empty fifo the slot keeps running, however long the pair goes on.
#[test]
fn the_streak_costs_nothing_with_an_empty_fifo() {
    let mut q = PinnedQueue::default();
    for _ in 0..(MAX_AFFINITY_STREAK * 4) {
        q.push_next(7);
        assert_eq!(q.pop(), Some(7));
    }
    assert!(q.is_empty());
}

// a second same-worker wake while the slot is still full displaces the first
// to the back rather than dropping it.
#[test]
fn a_displaced_slot_task_goes_to_the_back_of_the_fifo() {
    let mut q = PinnedQueue::default();
    q.push(50);
    q.push_next(10);
    q.push_next(20);
    assert_eq!(q.pop(), Some(20));
    assert_eq!(q.pop(), Some(50));
    assert_eq!(q.pop(), Some(10));
    assert!(q.is_empty());
}

// a coroutine that returns a constant, driven straight through: no pith,
// no FFI. exercises the corosensei integration in isolation.
#[test]
fn coroutine_runs_to_completion() {
    let mut coro: Coroutine<(), (), i64> = Coroutine::new(|_y, _in| 42);
    match coro.resume(()) {
        CoroutineResult::Return(v) => assert_eq!(v, 42),
        CoroutineResult::Yield(()) => panic!("unexpected yield"),
    }
}

// the preemption tests share the process-global epoch/flag and the task slab,
// so they must not run concurrently. this lock serializes them; the rest of
// the suite never touches those globals.
static PREEMPT_TEST_LOCK: Mutex<()> = Mutex::new(());

// the overrun predicate is the single definition of "this task has run too
// long" shared by the monitor and the safe-point. check the boundary: a task
// stamped at the current epoch has not overrun, nor one stamped one epoch
// ago, since a single tick says nothing about how long it ran; a full
// quantum later it has.
#[test]
fn task_overrun_triggers_after_the_quantum() {
    assert!(!task_overrun(5, 5));
    assert!(!task_overrun(5, 6));
    assert!(task_overrun(5, 5 + QUANTUM_EPOCHS));
    assert!(task_overrun(5, 100));
    // saturating: a stale `now` behind `running_since` never reports overrun.
    assert!(!task_overrun(10, 3));
}

// off a green task (`current_task()` is None: the os-thread backend, main,
// or between tasks), the safe-point slow path must do nothing even with the
// flag forced on. this is the invariant that makes emitting the check into
// all code harmless off-green.
#[test]
fn maybe_yield_is_noop_without_running_task() {
    let _guard = PREEMPT_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    assert!(current_task().is_none());
    PITH_PREEMPT_REQUESTED.store(1, AtomicOrdering::Relaxed);
    // must return promptly without parking; if it tried to park with no
    // running coroutine the debug_assert in park_current would fire.
    pith_green_maybe_yield();
    PITH_PREEMPT_REQUESTED.store(0, AtomicOrdering::Relaxed);
}

// full path: a green task that overruns its quantum hits a safe-point, parks,
// and is re-enqueued onto its owner worker's pinned queue, then resumes there
// and runs to completion. drives the real run_task/maybe_yield/park machinery
// on the test thread (no worker threads started), pinning the task to worker 0.
#[test]
fn maybe_yield_parks_and_requeues_on_owner() {
    let _guard = PREEMPT_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // act as worker 0 for the duration of this test.
    set_current_worker(Some(0));

    // a task body that, on its first resume, simulates the monitor observing
    // time pass (bumps the epoch past the quantum) and then hits a safe-point;
    // after it is resumed the second time it runs to completion.
    let coro = TaskCoroutine::with_stack(
        corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
        move |yielder: &TaskYielder, _input: ()| -> i64 {
            let _finish = publish_yielder(yielder as *const TaskYielder);
            PREEMPT_EPOCH.fetch_add(QUANTUM_EPOCHS + 1, AtomicOrdering::Relaxed);
            pith_green_maybe_yield();
            99
        },
    );

    let id = push_ready_task(coro);

    // force the flag on so the safe-point takes its slow path.
    PITH_PREEMPT_REQUESTED.store(1, AtomicOrdering::Relaxed);

    // first resume: the body overruns and parks at the safe-point. run_task's
    // yield path must see the preempt flag and re-enqueue onto owner 0's
    // lowest-priority deferred queue.
    run_task(id);
    {
        let word = sync_block(id).unwrap().word.load(AtomicOrdering::Acquire);
        assert_eq!(word_owner(word), Some(0), "task should pin to worker 0");
        assert_eq!(word & S_STATE_MASK, S_READY, "should be re-enqueued, not parked");
    }
    {
        let mut deferred = scheduler().workers[0]
            .deferred
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        assert_eq!(
            deferred.pop_front(),
            Some(id),
            "preempted task should be on owner's deferred queue"
        );
    }

    // second resume: runs to completion, records the result.
    run_task(id);
    assert_eq!(completion(id), Some(99));

    PITH_PREEMPT_REQUESTED.store(0, AtomicOrdering::Relaxed);
    set_current_worker(None);
}

// arm a Ready task wrapping `coro` the way `green_spawn` does, without
// enqueueing it, and return its id. shared by the tests that drive
// `run_task` by hand.
fn push_ready_task(coro: TaskCoroutine) -> TaskId {
    spawn_unqueued(coro).0
}

fn spawn_unqueued(coro: TaskCoroutine) -> (TaskId, i64) {
    let id = alloc_id();
    let generation = scheduler().sync.ensure(id).arm(SendCoroutine(coro), StackBounds::UNKNOWN, 0);
    (id, make_handle(id, generation))
}

// the result a finished task recorded, or `None` while it has not finished.
fn completion(id: TaskId) -> Option<i64> {
    let join = lock_join(&sync_block(id).unwrap().join);
    join.done.then_some(join.result)
}

// a task whose body returns `value` straight away.
fn returning(value: i64) -> TaskCoroutine {
    TaskCoroutine::with_stack(
        corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
        move |yielder: &TaskYielder, _input: ()| -> i64 {
            let _finish = publish_yielder(yielder as *const TaskYielder);
            value
        },
    )
}

// the panic boundary: a green task whose body panics (a pith trap or an FFI
// panic surfaces exactly as a Rust panic unwinding out of the coroutine) must
// NOT take the worker down with it. run_task's `catch_unwind` catches the
// panic, marks the task done (so its awaiters unblock instead of hanging),
// drops the dead coroutine, and returns normally; the same worker then goes on
// to run the next task. drives run_task directly on the test thread, standing
// in for a worker, so a missing boundary would unwind this test and fail it.
#[test]
fn panicking_task_marks_done_and_keeps_worker_alive() {
    let _guard = PREEMPT_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    set_current_worker(Some(0));

    let coro = TaskCoroutine::with_stack(
        corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
        move |yielder: &TaskYielder, _input: ()| -> i64 {
            let _finish = publish_yielder(yielder as *const TaskYielder);
            panic!("boom from a green task");
        },
    );
    let id = push_ready_task(coro);

    // must return, not unwind. without the boundary this call propagates the
    // panic and the test aborts.
    run_task(id);

    // the task is recorded done with a zero result, so an awaiter unblocks
    // with a defined outcome rather than hanging, and its coroutine is
    // dropped (never re-enqueued).
    assert_eq!(completion(id), Some(0), "a failed task reports a zero result");
    let block = sync_block(id).unwrap();
    assert_eq!(
        block.word.load(AtomicOrdering::Acquire) & S_STATE_MASK,
        S_DONE,
        "the scheduling word must retire so late wakes no-op"
    );
    // SAFETY: the task is Done, so nothing else touches its cells.
    assert!(
        unsafe { (*block.coro.get()).is_none() },
        "dead coroutine must not be put back"
    );
    // the worker (this thread) survived: a fresh task still runs to completion.
    let id2 = push_ready_task(returning(7));
    run_task(id2);
    assert_eq!(completion(id2), Some(7), "worker still runs tasks after a panic");

    set_current_worker(None);
}

// awaiting a finished task returns its result and retires it: the handle
// stops resolving, and the id comes back at a new generation, so the old
// handle can never name the task that reuses the block.
#[test]
fn await_retires_the_task_and_its_handle() {
    let (id, handle) = spawn_unqueued(returning(5));
    run_task(id);
    assert_eq!(green_is_done(handle), 1);
    assert_eq!(unsafe { green_await(handle) }, 5);
    assert_eq!(green_is_done(handle), 0, "a retired handle names no task");
    assert_eq!(unsafe { green_await(handle) }, 0, "await consumes the handle");

    let join = lock_join(&sync_block(id).unwrap().join);
    assert!(!join.live);
    assert_ne!(Some((id, join.generation)), split_handle(handle));
}

// a handle of another generation finds nothing and never blocks.
#[test]
fn a_stale_generation_resolves_to_no_task() {
    let (id, handle) = spawn_unqueued(returning(1));
    let (_, generation) = split_handle(handle).unwrap();
    let stale = make_handle(id, generation.wrapping_add(1) & GEN_MASK);
    assert_eq!(green_is_done(stale), 0);
    assert_eq!(unsafe { green_await(stale) }, 0);
    green_detach(stale);
    run_task(id);
    assert_eq!(unsafe { green_await(handle) }, 1, "the real handle still works");
}

// detaching a running task retires it when it finishes.
#[test]
fn a_detached_task_retires_when_it_finishes() {
    let (id, handle) = spawn_unqueued(returning(3));
    green_detach(handle);
    assert!(lock_join(&sync_block(id).unwrap().join).live, "still running");
    run_task(id);
    assert!(!lock_join(&sync_block(id).unwrap().join).live);
    assert_eq!(unsafe { green_await(handle) }, 0);
}

// detaching a finished task retires it at once.
#[test]
fn detaching_a_finished_task_retires_it_at_once() {
    let (id, handle) = spawn_unqueued(returning(3));
    run_task(id);
    green_detach(handle);
    assert!(!lock_join(&sync_block(id).unwrap().join).live);
    green_detach(handle);
}

// the scheduling-word owner packing: 0 means unpinned, worker indices
// round-trip through the owner bits, and the owner bits never collide with
// the state or flag bits.
#[test]
fn scheduling_word_owner_round_trips() {
    assert_eq!(word_owner(S_READY), None);
    assert_eq!(word_owner(S_PARKED | owner_bits(0)), Some(0));
    assert_eq!(word_owner(S_RUNNING | S_WAKE_PENDING | owner_bits(7)), Some(7));
    assert_eq!(
        owner_bits(3) & (S_STATE_MASK | S_WAKE_PENDING | S_PREEMPT_PENDING),
        0
    );
}

// a wake against an id no task has ever had must be a harmless no-op: the
// reactor can probe ids far beyond any real task (see netpoll.rs).
#[test]
fn wake_on_never_used_id_is_noop() {
    wake(ARENA_CHUNK * ARENA_SPINE + 5); // beyond the spine entirely
    wake(ARENA_CHUNK * (ARENA_SPINE - 1)); // in range, chunk never allocated
}

/// set in the child half of the arena-bound case below.
const ARENA_BOUND_CHILD: &str = "PITH_GREEN_ARENA_BOUND";

// arming a sync block past the end of the spine has to stop the process with
// a diagnostic. it used to `panic!`, which on this path is far worse than a
// crash: `ensure` runs on the spawn path, inside whatever task called spawn,
// and `run_task` catches every panic a task raises, so the spawning task
// would die with `join.done` never set and its awaiters would block forever,
// with nothing on stderr to say why.
//
// `ensure` exits, so the case runs this test binary again as a child and the
// parent asserts on how the child died.
#[test]
fn arming_past_the_arena_spine_stops_the_process() {
    if std::env::var(ARENA_BOUND_CHILD).is_ok() {
        scheduler().sync.ensure(ARENA_CHUNK * ARENA_SPINE);
        unreachable!("ensure past the spine returned instead of stopping the process");
    }

    let exe = std::env::current_exe().expect("test binary path");
    let output = std::process::Command::new(exe)
        .args([
            "concurrency::green::tests::arming_past_the_arena_spine_stops_the_process",
            "--exact",
            "--nocapture",
        ])
        .env(ARENA_BOUND_CHILD, "1")
        .output()
        .expect("re-run the test binary");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    assert_eq!(
        output.status.code(),
        Some(1),
        "expected a clean exit(1), got {:?}. stderr:\n{stderr}",
        output.status
    );
    assert!(
        stderr.contains("exceeds the sync arena bound"),
        "the diagnostic should name the bound. stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("panicked"),
        "the trap must not surface as a rust panic. stderr:\n{stderr}"
    );
}

// the full park -> wake -> resume cycle on the lock-free scheduling word: a
// task that parks with no pending flag reads Parked and sits in no queue;
// wake() claims it (Parked -> Ready) and enqueues it on its owner's pinned
// queue exactly once; a second wake while Ready is a no-op; the resumed
// task runs to completion and retires to Done.
#[test]
fn parked_task_wakes_once_onto_owner_pinned_queue() {
    let _guard = PREEMPT_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    set_current_worker(Some(0));
    // start from an empty pinned queue so the assertions below see only
    // this test's enqueues.
    while scheduler().workers[0]
        .pinned
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .pop()
        .is_some()
    {}

    let coro = TaskCoroutine::with_stack(
        corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
        move |yielder: &TaskYielder, _input: ()| -> i64 {
            let _finish = publish_yielder(yielder as *const TaskYielder);
            park_current(current_task().expect("test body runs inside a task"));
            21
        },
    );
    let id = push_ready_task(coro);

    // first resume: the body parks with no pending flag.
    run_task(id);
    let block = sync_block(id).unwrap();
    assert_eq!(
        block.word.load(AtomicOrdering::Acquire) & S_STATE_MASK,
        S_PARKED
    );
    assert!(scheduler().workers[0]
        .pinned
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_empty());

    // wake claims Parked -> Ready and enqueues on the owner's pinned queue.
    wake(id);
    let word = block.word.load(AtomicOrdering::Acquire);
    assert_eq!(word & S_STATE_MASK, S_READY);
    assert_eq!(word_owner(word), Some(0), "task stays pinned to its owner");
    // a second wake while Ready must not enqueue again.
    wake(id);
    {
        let mut pinned = scheduler().workers[0]
            .pinned
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        assert_eq!(pinned.pop(), Some(id));
        assert!(pinned.is_empty(), "a Ready task must not be double-enqueued");
    }

    run_task(id);
    assert_eq!(completion(id), Some(21));
    assert_eq!(
        block.word.load(AtomicOrdering::Acquire) & S_STATE_MASK,
        S_DONE
    );

    set_current_worker(None);
}

// a task woken onto a busy owner's pinned fifo can be taken by an idle
// peer, which becomes its owner. an owner that is not running anything is
// left alone, since it is about to drain the queue itself.
#[test]
fn idle_peer_steals_from_a_busy_owners_pinned_queue() {
    let _guard = PREEMPT_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if scheduler().workers.len() < 2 {
        return;
    }
    set_current_worker(Some(0));
    let owner = &scheduler().workers[0];
    while owner.pinned.lock().unwrap_or_else(|p| p.into_inner()).pop().is_some() {}

    let coro = TaskCoroutine::with_stack(
        corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
        move |yielder: &TaskYielder, _input: ()| -> i64 {
            let _finish = publish_yielder(yielder as *const TaskYielder);
            park_current(current_task().expect("test body runs inside a task"));
            34
        },
    );
    let id = push_ready_task(coro);
    run_task(id);

    // a wake from outside the pool keeps the owner and goes on the fifo.
    set_current_worker(None);
    wake(id);
    let block = sync_block(id).unwrap();
    assert_eq!(word_owner(block.word.load(AtomicOrdering::Acquire)), Some(0));
    assert_eq!(owner.pinned_fifo.load(AtomicOrdering::Relaxed), 1);

    let mut watch = PinWatch::new();
    owner.running_task.store(0, AtomicOrdering::Relaxed);
    assert_eq!(
        steal_pinned(0, 1, &mut watch),
        None,
        "an idle owner drains its own queue"
    );

    // the first look at a busy owner only records its resume count; a later
    // look that finds the count unchanged after `PIN_STEAL_WAIT` takes the task.
    owner.running_task.store(usize::MAX, AtomicOrdering::Relaxed);
    assert_eq!(
        steal_pinned(0, 1, &mut watch),
        None,
        "a busy owner is watched before it is robbed"
    );
    std::thread::sleep(PIN_STEAL_WAIT * 2);
    assert_eq!(steal_pinned(0, 1, &mut watch), Some(id));
    owner.running_task.store(0, AtomicOrdering::Relaxed);
    assert_eq!(owner.pinned_fifo.load(AtomicOrdering::Relaxed), 0);
    let word = block.word.load(AtomicOrdering::Acquire);
    assert_eq!(word & S_STATE_MASK, S_READY);
    assert_eq!(word_owner(word), Some(1), "the thief owns the task now");

    set_current_worker(Some(1));
    run_task(id);
    assert_eq!(completion(id), Some(34));
    set_current_worker(None);
}

// the run queue entry a wake left on worker 0's pinned queue, which the tests
// below take off by hand before they resume the task.
fn take_pinned_on_0() -> Option<TaskId> {
    scheduler().workers[0]
        .pinned
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .pop()
}

fn state_of(id: TaskId) -> u64 {
    sync_block(id).unwrap().word.load(AtomicOrdering::Acquire) & S_STATE_MASK
}

// a task body that runs `body` and returns `value`.
fn task_running(value: i64, body: impl FnOnce() + 'static) -> TaskCoroutine {
    TaskCoroutine::with_stack(
        corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
        move |yielder: &TaskYielder, _input: ()| -> i64 {
            let _finish = publish_yielder(yielder as *const TaskYielder);
            body();
            value
        },
    )
}

// a wake from elsewhere reaches a task parked in a semaphore acquire. the
// task re-checks, parks again, and must not stay listed twice: the stale
// entry would take the next release's wake from a task that is really
// waiting, which then sits parked with a permit free.
#[test]
fn a_stray_wake_leaves_no_stale_semaphore_waiter() {
    use crate::concurrency::semaphore::*;
    let _guard = PREEMPT_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    set_current_worker(Some(0));
    while take_pinned_on_0().is_some() {}

    let sem = pith_semaphore_new(1);
    unsafe { pith_semaphore_acquire(sem) }; // no permits left
    let sem_addr = sem as usize;
    let a = push_ready_task(task_running(1, move || {
        unsafe { pith_semaphore_acquire(sem_addr as *mut PithSemaphoreHandle) };
        // then wait somewhere else, as a task does after its acquire.
        park_current(current_task().expect("inside a task"));
    }));
    run_task(a);
    assert_eq!(state_of(a), S_PARKED, "a waits for a permit");

    // the stray wake: a resumes, finds no permit, parks on the semaphore again.
    wake(a);
    assert_eq!(take_pinned_on_0(), Some(a));
    run_task(a);
    assert_eq!(state_of(a), S_PARKED);

    // a release for a: it takes the permit and parks elsewhere.
    unsafe { pith_semaphore_release(sem) };
    assert_eq!(take_pinned_on_0(), Some(a));
    run_task(a);
    assert_eq!(state_of(a), S_PARKED, "a parked past its acquire");

    let b = push_ready_task(task_running(2, move || {
        unsafe { pith_semaphore_acquire(sem_addr as *mut PithSemaphoreHandle) };
    }));
    run_task(b);
    assert_eq!(state_of(b), S_PARKED, "b waits for a permit");

    unsafe { pith_semaphore_release(sem) };
    assert_eq!(state_of(b), S_READY, "the release must wake b, the real waiter");
    assert_eq!(state_of(a), S_PARKED, "and leave a where it waits");
    assert_eq!(take_pinned_on_0(), Some(b));
    run_task(b);
    assert_eq!(completion(b), Some(2));

    wake(a);
    assert_eq!(take_pinned_on_0(), Some(a));
    run_task(a);
    assert_eq!(completion(a), Some(1));
    set_current_worker(None);
}

// the same stray wake on a wait group or mutex waiter. both wake every listed
// waiter, so a duplicate entry is a second wake for one task, which can land
// after it resumed and cut its next park short.
#[test]
fn a_stray_wake_leaves_no_duplicate_waitgroup_or_mutex_waiter() {
    use crate::concurrency::mutex::*;
    use crate::concurrency::waitgroup::*;
    let _guard = PREEMPT_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    set_current_worker(Some(0));
    while take_pinned_on_0().is_some() {}

    let wg = pith_waitgroup_new();
    unsafe { pith_waitgroup_add(wg, 1) };
    let wg_addr = wg as usize;
    let a = push_ready_task(task_running(3, move || unsafe {
        pith_waitgroup_wait(wg_addr as *mut PithWaitGroupHandle)
    }));
    run_task(a);
    wake(a);
    assert_eq!(take_pinned_on_0(), Some(a));
    run_task(a);
    assert_eq!(state_of(a), S_PARKED);
    assert_eq!(unsafe { crate::concurrency::waitgroup::green_waiters_for_tests(wg) }, 1);
    unsafe { pith_waitgroup_done(wg) };
    assert_eq!(take_pinned_on_0(), Some(a));
    run_task(a);
    assert_eq!(completion(a), Some(3));

    let m = pith_mutex_new();
    unsafe { pith_mutex_lock(m) }; // held by this thread
    let m_addr = m as usize;
    let b = push_ready_task(task_running(4, move || unsafe {
        pith_mutex_lock(m_addr as *mut PithMutexHandle);
        pith_mutex_unlock(m_addr as *mut PithMutexHandle);
    }));
    run_task(b);
    wake(b);
    assert_eq!(take_pinned_on_0(), Some(b));
    run_task(b);
    assert_eq!(state_of(b), S_PARKED);
    assert_eq!(unsafe { crate::concurrency::mutex::green_waiters_for_tests(m) }, 1);
    unsafe { pith_mutex_unlock(m) };
    assert_eq!(take_pinned_on_0(), Some(b));
    run_task(b);
    assert_eq!(completion(b), Some(4));
    set_current_worker(None);
}

// a green task waits on a child with no pidfd to watch it by (macOS, linux
// before 5.3). the wait must not hold the process registry across the
// child's life: a reader draining the child's pipes, or a deadline's kill,
// takes that lock, and a child stuck on a full pipe never exits.
#[test]
fn a_green_wait_without_a_pidfd_keeps_the_process_lock_free() {
    use crate::process::*;
    let _guard = PREEMPT_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // the last worker, so the timers this sweeps are not worker 0's, which
    // the netpoll tests drive by hand.
    let worker = scheduler().workers.len() - 1;
    if worker == 0 {
        return;
    }
    let take_pinned = || {
        scheduler().workers[worker]
            .pinned
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop()
    };
    set_current_worker(Some(worker));
    while take_pinned().is_some() {}
    NO_PIDFD_FOR_TESTS.store(true, AtomicOrdering::Relaxed);

    let cmd = std::ffi::CString::new("sleep 3").unwrap();
    let handle = unsafe { pith_process_spawn(cmd.as_ptr()) };
    assert_ne!(handle, 0, "sh should spawn");
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        let start = Instant::now();
        let killed = pith_process_kill(handle);
        (killed, start.elapsed())
    });
    let code = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let task_code = Arc::clone(&code);
    let a = push_ready_task(task_running(0, move || {
        task_code.store(pith_process_wait(handle), AtomicOrdering::Relaxed);
    }));
    run_task(a);
    let deadline = Instant::now() + Duration::from_secs(10);
    while completion(a).is_none() {
        match take_pinned() {
            Some(id) => run_task(id),
            None => {
                assert!(Instant::now() < deadline, "the wait never finished");
                // as that worker between tasks: sweep its timers.
                crate::netpoll::idle_wait(worker, Duration::from_millis(5));
            }
        }
    }
    let (killed, kill_took) = killer.join().expect("killer panicked");
    NO_PIDFD_FOR_TESTS.store(false, AtomicOrdering::Relaxed);
    pith_process_close(handle);
    set_current_worker(None);
    assert!(
        kill_took < Duration::from_secs(1),
        "the kill waited {kill_took:?} on the registry lock"
    );
    assert_eq!(killed, 1, "the kill reached the running child");
    assert_ne!(code.load(AtomicOrdering::Relaxed), 0, "a killed child is no clean exit");
}
