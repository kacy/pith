//! a test probe for resuming a parked task on another thread.

use super::*;

// ---------------------------------------------------------------------------
// the cross-thread resume probe.
//
// a task parked on one os thread and resumed on another must see the resuming
// thread's identity — its worker index — and must still find its own task id
// and tls map, which the resuming thread's `run_task` installs. that is the
// resume a migration performs, and this probe forces it by hand: the calling
// thread poses as worker 0 and runs a task until it parks; a second os thread,
// posing as the last worker, claims the task and resumes it; the task body
// reports what it saw. `tests/cross_thread_resume.rs` drives it.
//
// it lives in library code rather than in the test module for a reason that
// is the whole point of the check. what makes a stale read possible is the
// code shape of the *shipped* runtime — the tls model `libpith_runtime.a`
// is compiled with (see the accessor block above `current_task`). a `cargo
// test` binary is an executable in its own right, always gets the local-exec
// model whatever the workspace is configured with, and so can never observe
// the hazard however its body is written: the archive built with the
// dynamic model failed this probe (worker `none` after the resume, identity
// lost) while the identical body passed inside the test binary. the probe's
// body is therefore compiled here, and the integration test runs it a
// second time through a c harness linked against the archive, which is how
// every pith program links it.
// ---------------------------------------------------------------------------

/// pack the probe's readings: the pool's worker count (bits 24..32), the
/// worker seen before the park (bits 16..24), the worker seen after (bits
/// 8..16; 0xff for "none"), and whether the task id and tls map seen after
/// the park are still the task's own (bit 0). `-1` means the probe could not
/// run: the pool has fewer than two workers, so there is no second identity
/// to pose as.
#[doc(hidden)]
pub fn encode_probe(workers: usize, before: Option<usize>, after: Option<usize>, own: bool) -> i64 {
    let n = (workers.min(0xff)) as i64;
    let b = before.map_or(0xff, |w| w as i64);
    let a = after.map_or(0xff, |w| w as i64);
    (n << 24) | (b << 16) | (a << 8) | i64::from(own)
}

/// run the cross-thread resume scenario and report what the task body saw,
/// encoded by `encode_probe`. the expected value is
/// `encode_probe(n, Some(0), Some(n - 1), true)` for a pool of `n` workers.
///
/// exported unmangled so a harness linked against the archive can call it.
#[doc(hidden)]
#[no_mangle]
pub extern "C" fn pith_green_cross_thread_probe() -> i64 {
    // build the scheduler without starting its workers: the two identities
    // below are posed by this thread and a plain spawned thread, and a real
    // worker draining queues would race the hand-off.
    let n = scheduler().workers.len();
    if n < 2 {
        return -1;
    }
    let first = 0;
    let second = n - 1;
    set_current_worker(Some(first));

    let coro = TaskCoroutine::with_stack(
        take_stack(),
        move |yielder: &TaskYielder, _input: ()| -> i64 {
            publish_yielder(yielder as *const TaskYielder);
            let Some(id) = current_task() else {
                return encode_probe(n, None, None, false);
            };
            let tls = current_task_tls();
            let before = current_worker();
            park_current(id);
            // every read from here on is on whichever thread resumed us.
            let after = current_worker();
            let own = current_task() == Some(id) && current_task_tls() == tls;
            encode_probe(n, before, after, own)
        },
    );
    let id = alloc_id();
    let generation = scheduler().sync.ensure(id).arm(SendCoroutine(coro), 0);

    // first resume, on this thread: the body parks. claim it the way a waker
    // does (Parked -> Ready) but hand it straight to the thread below instead
    // of enqueueing it, so a real worker 0 — if the process has started the
    // pool — cannot take it first.
    run_task(id);
    set_current_worker(None);
    let Some(block) = sync_block(id) else {
        return encode_probe(n, None, None, false);
    };
    let word = block.word.load(AtomicOrdering::Acquire);
    if word & S_STATE_MASK != S_PARKED
        || block
            .word
            .compare_exchange(
                word,
                (word & S_OWNER_MASK) | S_READY,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Acquire,
            )
            .is_err()
    {
        return encode_probe(n, None, None, false);
    }

    let resumed = std::thread::Builder::new()
        .name("pith-probe-resumer".to_string())
        .spawn(move || {
            set_current_worker(Some(second));
            run_task(id);
            set_current_worker(None);
            match sync_block(id) {
                Some(block) if lock_join(&block.join).done => lock_join(&block.join).result,
                _ => encode_probe(n, None, None, false),
            }
        });
    let result = match resumed {
        Ok(handle) => handle.join().unwrap_or_else(|_| encode_probe(n, None, None, false)),
        Err(_) => encode_probe(n, None, None, false),
    };
    // the task is done and nobody awaits it: give its block back.
    let _ = unsafe { green_await(make_handle(id, generation)) };
    result
}

