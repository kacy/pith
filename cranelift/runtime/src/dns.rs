//! hostname resolution that does not park a green worker.
//!
//! `getaddrinfo` is synchronous and there is no portable way to poll it, so a
//! lookup made from a green task holds the *worker* OS thread for its whole
//! duration — every other task pinned to that worker stalls with it. sockets do
//! not have this problem (a would-block yields to the epoll reactor in
//! `netpoll`), which leaves dns as the one blocking call on the common client
//! path: every grpc, http, and database dial starts with one.
//!
//! the fix is the usual one for a syscall that cannot be polled: keep the
//! blocking call, but make some other thread do it. a small pool of ordinary OS
//! threads owns `to_socket_addrs`; the green task hands over a job, parks, and a
//! pool thread wakes it when the answer is ready. the worker is free the whole
//! time.
//!
//! the park/wake handshake is the same one `netpoll` uses for socket readiness,
//! for the same reason: a wake that lands before the task finishes suspending
//! must not be lost. green records it as pending and the yield arm re-enqueues,
//! so the block site only has to re-check its condition and re-park.
//!
//! under the os-thread backend none of this applies — blocking is what a thread
//! is for, and the pool would only add a handoff — so `resolve` calls
//! `to_socket_addrs` inline there, exactly as the callers used to.

use crate::concurrency::green;
use crate::concurrency::scheduler::{backend, Backend};
use std::collections::VecDeque;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

/// the most OS threads the resolver pool will ever run. a thread per lookup
/// would let a burst of dials (or a hostile one) spawn threads without bound,
/// so lookups past this many in flight queue up instead.
const MAX_THREADS: usize = 4;

/// the addresses one lookup produced, in resolution order.
///
/// an empty list means "no address", and covers both a resolver error and a
/// lookup that succeeded with nothing in it. collapsing the two loses nothing:
/// every caller in this runtime already treats them the same (a failed connect,
/// a null resolve result), and it keeps the value that crosses between threads a
/// plain `Vec`.
type Answer = Vec<SocketAddr>;

/// resolve `host:port` to its addresses, returning an empty list on failure.
///
/// this is the only entry point; it picks the offloaded or the inline path
/// itself so callers do not have to care which backend they are on.
pub(crate) fn resolve(host: &str, port: i64) -> Answer {
    let target = format!("{}:{}", host, port);
    // a numeric address never reaches a resolver: `to_socket_addrs` parses it
    // in place, with no syscall to block in, so offloading would buy nothing
    // and cost a thread handoff. dialing a literal ip is common enough — tests,
    // benchmarks, a sidecar on 127.0.0.1 — to check for first.
    if let Ok(addr) = target.parse::<SocketAddr>() {
        return vec![addr];
    }
    match offload_target() {
        Some(task) => resolve_offloaded(target, task),
        None => resolve_inline(&target),
    }
}

/// the green task that should park for this lookup, or `None` when the caller
/// should just block: the os-thread backend, or a green program calling from a
/// thread that is not running a task (main, or an os-thread spawn).
fn offload_target() -> Option<usize> {
    if backend() != Backend::Green {
        return None;
    }
    green::current_task()
}

/// the blocking lookup itself, byte-for-byte what the call sites used to do
/// inline. runs on the calling thread under the os-thread backend and on a pool
/// thread under green.
fn resolve_inline(target: &str) -> Answer {
    match target.to_socket_addrs() {
        Ok(addrs) => addrs.collect(),
        Err(_) => Vec::new(),
    }
}

/// hand the lookup to the pool and park the green task until the answer lands.
fn resolve_offloaded(target: String, task: usize) -> Answer {
    let slot = Arc::new(Slot {
        task,
        answer: Mutex::new(None),
    });
    let job = Job {
        target,
        slot: Arc::clone(&slot),
    };
    if let Err(job) = pool().submit(job) {
        // no pool thread could be started at all. blocking the worker is worse
        // than not blocking it, but much better than failing a lookup the
        // system can answer.
        return resolve_inline(&job.target);
    }

    // park until a pool thread fills the slot. a wake that lands before we
    // finish suspending is not lost — green flags it as pending and the yield
    // arm re-enqueues us instead of parking — and a spurious wake just sends us
    // round the loop to park again, the tolerance every green block site has.
    loop {
        if let Some(answer) = slot.take() {
            return answer;
        }
        green::park_current();
    }
}

/// where a pool thread leaves the answer for the task that asked for it.
struct Slot {
    /// green slab id of the parked task, passed straight to `green::wake`.
    task: usize,
    /// `None` until the lookup finishes, `Some` (possibly empty) after.
    answer: Mutex<Option<Answer>>,
}

impl Slot {
    /// publish the answer and wake the waiting task. the store happens first so
    /// the task cannot resume and find the slot still empty.
    fn complete(&self, answer: Answer) {
        *lock(&self.answer) = Some(answer);
        green::wake(self.task);
    }

    /// take the answer if it has arrived.
    fn take(&self) -> Option<Answer> {
        lock(&self.answer).take()
    }
}

/// one queued lookup: the `host:port` string to resolve and where to put the
/// answer.
struct Job {
    target: String,
    slot: Arc<Slot>,
}

/// the queue the pool threads share, plus the bookkeeping `submit` needs to
/// decide whether to start another thread.
struct Queue {
    jobs: VecDeque<Job>,
    /// threads started so far. pool threads never retire, so this only grows,
    /// and never past `MAX_THREADS`.
    threads: usize,
    /// threads currently waiting on `ready` for work.
    idle: usize,
}

struct Pool {
    queue: Mutex<Queue>,
    ready: Condvar,
}

/// built on the first offloaded lookup, so a program that never resolves a
/// hostname under green never starts a thread.
static POOL: OnceLock<Pool> = OnceLock::new();

fn pool() -> &'static Pool {
    POOL.get_or_init(|| Pool {
        queue: Mutex::new(Queue {
            jobs: VecDeque::new(),
            threads: 0,
            idle: 0,
        }),
        ready: Condvar::new(),
    })
}

impl Pool {
    /// queue a lookup, growing the pool if nothing is free to pick it up.
    ///
    /// hands the job back only when the pool has no thread at all and the OS
    /// refused to give us one — the caller then resolves inline rather than
    /// leaving a task parked on an answer that will never come.
    fn submit(&'static self, job: Job) -> Result<(), Job> {
        let mut queue = lock(&self.queue);

        // grow lazily: one thread is enough for a program that resolves one
        // name at a time, and a burst gets up to `MAX_THREADS` of them.
        if queue.idle == 0 && queue.threads < MAX_THREADS {
            match std::thread::Builder::new()
                .name("pith-dns".to_string())
                .spawn(|| self.worker())
            {
                Ok(_) => queue.threads += 1,
                Err(_) => {
                    if queue.threads == 0 {
                        return Err(job);
                    }
                    // some other thread is already running; it will get to this
                    // job when it finishes the one it has.
                }
            }
        }

        queue.jobs.push_back(job);
        drop(queue);
        self.ready.notify_one();
        Ok(())
    }

    /// a pool thread: resolve queued names forever, waking each waiting task.
    ///
    /// threads are never retired. the cap is four, an idle one costs a parked
    /// futex wait and its stack, and a program that resolved once is very likely
    /// to resolve again — a client dials more than one host.
    fn worker(&self) {
        loop {
            let job = self.next_job();
            // a panic here would strand the parked task forever, and unwinding
            // out of a pool thread would cross no FFI boundary but would still
            // silently shrink the pool. contain it and report the lookup as a
            // failure, which is how a resolver error already reads.
            let answer =
                std::panic::catch_unwind(|| resolve_inline(&job.target)).unwrap_or_default();
            job.slot.complete(answer);
        }
    }

    /// take the next queued lookup, waiting for one if the queue is empty.
    fn next_job(&self) -> Job {
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
/// later lookup, and none of this state has an invariant a panic could break.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// a hostname whose lookup fails without touching the network: the port is
    /// out of range, so `to_socket_addrs` rejects the target before it would
    /// query a resolver. keeps the failure tests fast and offline.
    const UNPARSEABLE_PORT: i64 = 99999;

    /// run one job through the pool from a plain test thread and wait for the
    /// answer. the slot names a task id no green task will ever have; `wake`
    /// against an unknown id is a documented no-op, so this exercises the queue,
    /// the threads, and the slot without needing a live green task.
    fn resolve_via_pool(target: &str) -> Answer {
        let slot = Arc::new(Slot {
            task: usize::MAX,
            answer: Mutex::new(None),
        });
        assert!(pool()
            .submit(Job {
                target: target.to_string(),
                slot: Arc::clone(&slot),
            })
            .is_ok());
        await_slot(&slot)
    }

    /// spin until a slot fills, with a generous ceiling so a hung lookup fails
    /// the test instead of hanging the suite.
    fn await_slot(slot: &Slot) -> Answer {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(answer) = slot.take() {
                return answer;
            }
            assert!(Instant::now() < deadline, "pool never answered");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn inline_resolve_finds_loopback() {
        let addrs = resolve_inline("localhost:80");
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(|a| a.ip().is_loopback() && a.port() == 80));
    }

    #[test]
    fn inline_resolve_reports_failure_as_no_addresses() {
        assert!(resolve_inline(&format!("localhost:{}", UNPARSEABLE_PORT)).is_empty());
    }

    #[test]
    fn pool_answers_a_lookup() {
        let addrs = resolve_via_pool("localhost:8080");
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(|a| a.ip().is_loopback()));
    }

    #[test]
    fn pool_reports_failure_as_no_addresses() {
        assert!(resolve_via_pool(&format!("localhost:{}", UNPARSEABLE_PORT)).is_empty());
    }

    #[test]
    fn pool_never_exceeds_its_thread_cap() {
        // more jobs at once than the cap, so the growth check is exercised past
        // the point where it must stop starting threads.
        let slots: Vec<Arc<Slot>> = (0..MAX_THREADS * 4)
            .map(|_| {
                let slot = Arc::new(Slot {
                    task: usize::MAX,
                    answer: Mutex::new(None),
                });
                assert!(pool()
                    .submit(Job {
                        target: "localhost:80".to_string(),
                        slot: Arc::clone(&slot),
                    })
                    .is_ok());
                slot
            })
            .collect();

        for slot in &slots {
            assert!(!await_slot(slot).is_empty());
        }
        assert!(lock(&pool().queue).threads <= MAX_THREADS);
    }

    #[test]
    fn resolve_outside_a_green_task_stays_inline() {
        // nothing here runs on a green worker, so `resolve` must take the
        // blocking path and still produce the same answer.
        assert!(offload_target().is_none());
        assert!(!resolve("localhost", 443).is_empty());
    }

    #[test]
    fn literal_addresses_skip_the_resolver() {
        // both families, and the answer must match what `to_socket_addrs`
        // produces for the same string — the fast path is a shortcut, not a
        // second implementation.
        for (host, port) in [("127.0.0.1", 8080), ("[::1]", 443), ("::1", 443)] {
            let target = format!("{}:{}", host, port);
            assert_eq!(resolve(host, port), resolve_inline(&target), "{}", target);
        }
    }

    #[test]
    fn out_of_range_ports_still_fail() {
        // the literal fast path must not accept what `to_socket_addrs` rejects.
        assert!(resolve("127.0.0.1", UNPARSEABLE_PORT).is_empty());
    }
}
