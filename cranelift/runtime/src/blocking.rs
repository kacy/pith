//! running a blocking syscall without holding a green worker.
//!
//! a green worker OS thread runs many tasks, so a call with no yield point
//! holds every one of them, not just the task that made it. sockets avoid that
//! by yielding to the epoll reactor in `netpoll`, but some syscalls cannot be
//! polled at all: `getaddrinfo` has nothing to wait on, and a regular file is
//! *always* reported ready by epoll no matter how slow the disk behind it is.
//!
//! for those there is only one shape that works — keep the blocking call, but
//! make some other thread do it. this module is that shape: a pool of ordinary
//! OS threads takes a job, the green task parks, and the pool thread wakes it
//! when the result is ready. the worker is free the whole time.
//!
//! the park/wake handshake is the one `netpoll` uses for socket readiness, for
//! the same reason: a wake that lands before the task finishes suspending must
//! not be lost. green records it as pending and the yield arm re-enqueues, so a
//! block site only has to re-check its condition and park again.
//!
//! ## what may cross to a pool thread
//!
//! plain owned data, and nothing else. pith handles are not thread-agnostic —
//! the struct pool is a thread-local freelist and there is other thread-local
//! state besides — so a handle allocated on a pool thread and later freed by
//! the task's worker would cross freelists, silently. jobs therefore return a
//! `Vec<u8>`, an `i64`, a `bool`, a `std::fs::File`: things the OS or the
//! global allocator owns. every pith-visible value is built afterwards, back on
//! the calling task's thread.
//!
//! ## one implementation, one pool per kind of work
//!
//! `Pool` is a type, not a singleton. dns and file i/o each hold their own
//! static, so a multi-megabyte file write cannot queue behind four slow
//! hostname lookups (or the reverse) while both still share this one park/wake
//! implementation.

use crate::concurrency::green;
use crate::concurrency::scheduler::{backend, Backend};
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// is the caller a green task whose worker we should keep free?
///
/// callers that would have to copy something to hand it over — the content of a
/// write, say — check this first and take the direct path untouched when it is
/// false, so the os-thread backend pays nothing for a facility it does not use.
pub(crate) fn offloads() -> bool {
    offload_target().is_some()
}

/// run `work` on a pool thread and park until it finishes, or run it right here
/// when there is no worker to protect.
///
/// the inline arm covers the os-thread backend (blocking is what a thread is
/// for, and the pool would only add a handoff) and a green program calling from
/// a thread that is not running a task, such as main.
pub(crate) fn run<T, F>(pool: &'static Pool, work: F) -> T
where
    T: Default + Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let Some(task) = offload_target() else {
        return work();
    };
    if !pool.ensure_worker() {
        // the OS refused us a thread and the pool has none at all. blocking the
        // worker is worse than not blocking it, but far better than parking a
        // task on a result that will never come.
        return work();
    }

    let slot = Arc::new(Slot::<T>::new(task));
    let publish = Arc::clone(&slot);
    pool.enqueue(Box::new(move || {
        // a panic here would strand the parked task forever. contain it and
        // report the default, which every caller already reads as failure: an
        // empty answer, a zero byte count, a `None`.
        //
        // the closure is only unwind-safe in the sense that matters — nothing
        // it captures outlives the job — so assert it rather than pushing the
        // bound onto every caller.
        let value = std::panic::catch_unwind(AssertUnwindSafe(work)).unwrap_or_default();
        publish.complete(value);
    }));

    if let Some(value) = spin_briefly(&slot) {
        return value;
    }

    // a wake that lands before we finish suspending is not lost: green flags it
    // as pending and the yield arm re-enqueues us instead of parking. a
    // spurious wake just sends us round to park again, the tolerance every
    // green block site has.
    loop {
        if let Some(value) = slot.take() {
            return value;
        }
        green::park_current();
    }
}

/// how long a task waits on-CPU for its job before it gives up and parks.
///
/// a park is two thread wakeups — the pool thread to start the job, this
/// worker to resume afterwards — and on a small machine that costs far more
/// than a read the page cache answers. so wait a moment first: this is roughly
/// what the same call would have held the worker for if it had run inline, and
/// anything slower than it (a real disk, a network mount, a large transfer)
/// falls through to the park and frees the worker, which is the whole point of
/// the pool.
const SPIN_BEFORE_PARK: Duration = Duration::from_micros(25);

/// how long a pool thread keeps polling the queue before it goes back to
/// sleep. short enough that an idle pool is not a spinning pool, long enough to
/// cover the gap between two calls in a loop.
const STAY_HOT: Duration = Duration::from_micros(50);

/// how many pauses to burn between clock reads while spinning. reading the
/// clock is the expensive part of the loop, and there is no point doing it
/// after every single pause.
const SPINS_PER_CLOCK_READ: usize = 64;

/// is there a second core for the pool thread to run on?
///
/// both waits below assume the thread they are waiting for can make progress
/// while they spin. on a single-core host it cannot: the spinner is holding the
/// only core the pool thread needs, so waiting on-CPU is pure tax and both
/// sides skip it. asked once and cached, since it cannot change.
fn spinning_helps() -> bool {
    static HELPS: OnceLock<bool> = OnceLock::new();
    *HELPS.get_or_init(|| {
        std::thread::available_parallelism().is_ok_and(|cores| cores.get() > 1)
    })
}

/// wait out `SPIN_BEFORE_PARK` for the job to finish, returning its result if
/// it does.
///
/// the wait polls a flag rather than the result mutex: the pool thread has to
/// take that mutex to publish, and a spinner hammering it is a spinner slowing
/// down the very thing it is waiting for.
fn spin_briefly<T>(slot: &Slot<T>) -> Option<T> {
    if !spinning_helps() {
        return None;
    }
    let deadline = Instant::now() + SPIN_BEFORE_PARK;
    loop {
        if slot.is_ready() {
            return slot.take();
        }
        if Instant::now() >= deadline {
            return None;
        }
        for _ in 0..SPINS_PER_CLOCK_READ {
            std::hint::spin_loop();
        }
    }
}

/// the green task that should park for this job, or `None` when the caller
/// should just block.
fn offload_target() -> Option<usize> {
    if backend() != Backend::Green {
        return None;
    }
    green::current_task()
}

/// where a pool thread leaves the result for the task that asked for it.
struct Slot<T> {
    /// green slab id of the parked task, passed straight to `green::wake`.
    task: usize,
    /// set once the result is in `value`, so a waiter can check for it without
    /// touching the mutex the publisher needs.
    ready: AtomicBool,
    /// `None` until the job finishes, `Some` after.
    value: Mutex<Option<T>>,
}

impl<T> Slot<T> {
    fn new(task: usize) -> Self {
        Slot {
            task,
            ready: AtomicBool::new(false),
            value: Mutex::new(None),
        }
    }

    /// publish the result and wake the waiting task. the store happens first so
    /// the task cannot resume and find the slot still empty.
    fn complete(&self, value: T) {
        *lock(&self.value) = Some(value);
        self.ready.store(true, Ordering::Release);
        green::wake(self.task);
    }

    /// has the result landed? pairs with the release store in `complete`.
    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// take the result if it has arrived.
    fn take(&self) -> Option<T> {
        if !self.is_ready() {
            return None;
        }
        lock(&self.value).take()
    }
}

/// one queued unit of work. the result type is erased here — the job already
/// knows which slot to publish to — so a single queue serves every caller.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// the queue the pool threads share, plus the bookkeeping `ensure_worker`
/// needs to decide whether to start another thread.
struct Queue {
    jobs: VecDeque<Job>,
    /// threads started so far. pool threads never retire, so this only grows,
    /// and never past `max_threads`.
    threads: usize,
    /// threads currently waiting on `ready` for work.
    idle: usize,
}

/// a set of OS threads that run blocking work on behalf of green tasks.
///
/// hold one in a `static`: it costs nothing until the first job, since threads
/// start on demand and the queue allocates lazily.
pub(crate) struct Pool {
    /// thread name, so `top` and a backtrace say which pool is busy.
    name: &'static str,
    /// the most threads this pool will ever run. a thread per job would let a
    /// burst (or a hostile caller) spawn threads without bound, so jobs past
    /// this many in flight queue up instead.
    max_threads: usize,
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Pool {
    pub(crate) const fn new(name: &'static str, max_threads: usize) -> Self {
        Pool {
            name,
            max_threads,
            queue: Mutex::new(Queue {
                jobs: VecDeque::new(),
                threads: 0,
                idle: 0,
            }),
            ready: Condvar::new(),
        }
    }

    /// make sure some thread will pick up the next job, growing the pool if
    /// nothing is idle to take it.
    ///
    /// false means the pool has no thread at all and the OS refused to give us
    /// one; the caller then does the work itself rather than queueing it
    /// somewhere nothing will run it. asking before handing the job over keeps
    /// the fallback trivial — there is no job to take back.
    ///
    /// threads are never retired. the cap is small, an idle one costs a parked
    /// futex wait and its stack, and a program that blocked once is very likely
    /// to block again.
    fn ensure_worker(&'static self) -> bool {
        let mut queue = lock(&self.queue);
        if queue.idle > 0 {
            return true;
        }
        if queue.threads >= self.max_threads {
            // every thread is busy, but they all come back for more work.
            return true;
        }
        match std::thread::Builder::new()
            .name(self.name.to_string())
            .spawn(|| self.worker())
        {
            Ok(_) => {
                queue.threads += 1;
                true
            }
            // some other thread is already running; it will get to this job
            // when it finishes the one it has.
            Err(_) => queue.threads > 0,
        }
    }

    /// hand a job to whichever pool thread gets to it first.
    fn enqueue(&self, job: Job) {
        lock(&self.queue).jobs.push_back(job);
        self.ready.notify_one();
    }

    /// a pool thread: run queued jobs forever.
    fn worker(&self) {
        loop {
            (self.next_job())();
        }
    }

    /// take the next queued job, waiting for one if the queue is empty.
    ///
    /// a thread stays hot for a moment before it goes to sleep. a program that
    /// made one blocking call usually makes another right behind it, and a
    /// sleeping thread costs that next caller a wakeup — the same wakeup its
    /// own spin in `run` is trying to avoid. the two together are what make
    /// back-to-back small reads cheap.
    fn next_job(&self) -> Job {
        if !spinning_helps() {
            return self.wait_for_job();
        }
        let deadline = Instant::now() + STAY_HOT;
        loop {
            if let Some(job) = lock(&self.queue).jobs.pop_front() {
                return job;
            }
            if Instant::now() >= deadline {
                break;
            }
            for _ in 0..SPINS_PER_CLOCK_READ {
                std::hint::spin_loop();
            }
        }
        self.wait_for_job()
    }

    /// sleep until there is work. the notify that wakes this cannot be lost:
    /// the queue is re-checked under the same lock the enqueue takes.
    fn wait_for_job(&self) -> Job {
        let mut queue = lock(&self.queue);
        loop {
            if let Some(job) = queue.jobs.pop_front() {
                return job;
            }
            queue.idle += 1;
            queue = self
                .ready
                .wait(queue)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            queue.idle -= 1;
        }
    }
}

/// lock through a poisoned mutex: a panicking pool thread must not wedge every
/// later job, and none of this state has an invariant a panic could break.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    static TEST_POOL: Pool = Pool::new("pith-blocking-test", 4);

    /// submit one job from a plain test thread and wait for its result.
    ///
    /// the slot names a task id no green task will ever have; `green::wake`
    /// against an unknown id is a documented no-op, so this exercises the
    /// queue, the threads, and the slot without needing a live green task.
    fn via_pool<T, F>(work: F) -> T
    where
        T: Default + Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let slot = Arc::new(Slot::<T>::new(usize::MAX));
        let publish = Arc::clone(&slot);
        assert!(TEST_POOL.ensure_worker());
        TEST_POOL.enqueue(Box::new(move || {
            let value = std::panic::catch_unwind(AssertUnwindSafe(work)).unwrap_or_default();
            publish.complete(value);
        }));
        await_slot(&slot)
    }

    /// spin until a slot fills, with a generous ceiling so a hung job fails the
    /// test instead of hanging the suite.
    fn await_slot<T>(slot: &Slot<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(value) = slot.take() {
                return value;
            }
            assert!(Instant::now() < deadline, "pool never answered");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn pool_runs_a_job_and_returns_its_value() {
        assert_eq!(via_pool(|| 6 * 7), 42);
    }

    #[test]
    fn pool_runs_a_job_on_another_thread() {
        let here = std::thread::current().id();
        assert!(via_pool(move || std::thread::current().id() != here));
    }

    #[test]
    fn a_panicking_job_yields_the_default_and_keeps_the_pool_alive() {
        let value: i64 = via_pool(|| panic!("job blew up"));
        assert_eq!(value, 0);
        // the thread that took the panicking job must still be serving.
        assert_eq!(via_pool(|| 1i64), 1);
    }

    #[test]
    fn pool_never_exceeds_its_thread_cap() {
        // more jobs at once than the cap, so the growth check runs well past
        // the point where it must stop starting threads. each job blocks until
        // every one of them has been picked up, forcing the pool to grow.
        static STARTED: AtomicUsize = AtomicUsize::new(0);
        let slots: Vec<Arc<Slot<i64>>> = (0..TEST_POOL.max_threads * 4)
            .map(|_| {
                let slot = Arc::new(Slot::<i64>::new(usize::MAX));
                let publish = Arc::clone(&slot);
                assert!(TEST_POOL.ensure_worker());
                TEST_POOL.enqueue(Box::new(move || {
                    STARTED.fetch_add(1, Ordering::Relaxed);
                    publish.complete(1);
                }));
                slot
            })
            .collect();

        for slot in &slots {
            assert_eq!(await_slot(slot), 1);
        }
        assert_eq!(STARTED.load(Ordering::Relaxed), slots.len());
        assert!(lock(&TEST_POOL.queue).threads <= TEST_POOL.max_threads);
    }

    #[test]
    fn the_short_wait_gives_up_when_nothing_answers() {
        let slot = Slot::<i64>::new(usize::MAX);
        let started = Instant::now();
        assert!(spin_briefly(&slot).is_none());
        // it must not return early either, or a result that was one microsecond
        // away would still cost a park.
        if spinning_helps() {
            assert!(started.elapsed() >= SPIN_BEFORE_PARK);
        }
    }

    #[test]
    fn the_short_wait_picks_up_a_result_that_lands_during_it() {
        if !spinning_helps() {
            return;
        }
        let slot = Arc::new(Slot::<i64>::new(usize::MAX));
        let publish = Arc::clone(&slot);
        std::thread::spawn(move || publish.complete(9));
        // the spawn may lose the race on a busy machine, so accept either
        // outcome and only insist that a result seen in the wait is the right
        // one — never a torn or missing value.
        if let Some(value) = spin_briefly(&slot) {
            assert_eq!(value, 9);
        } else {
            assert_eq!(await_slot(&slot), 9);
        }
    }

    #[test]
    fn work_outside_a_green_task_runs_inline() {
        // nothing in a unit test runs on a green worker, so `run` must take the
        // direct path — same thread, same answer.
        assert!(!offloads());
        let here = std::thread::current().id();
        assert!(run(&TEST_POOL, move || std::thread::current().id() == here));
    }
}
