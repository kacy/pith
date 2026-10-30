//! coroutine stacks, pooled so a spawn rarely maps a new one.

use super::*;

/// coroutine stack size. pith programs default to 8 MiB OS-thread stacks; a
/// green task gets 1 MiB, which is generous for typical task bodies and keeps
/// a large fan-out affordable. a deeply recursive task could overflow it, and
/// corosensei's guard page makes that fault loudly rather than corrupt
/// memory.
pub(super) const STACK_SIZE: usize = 1024 * 1024;

/// finished coroutines donate their stacks here and green_spawn reuses them.
/// a fresh stack is an mmap + a guard-page mprotect, and freeing one is an
/// munmap that costs a TLB shootdown across every core. per task, that made
/// spawn dominated by the kernel's address-space bookkeeping (a 20k spawn/join
/// probe spent 22k page faults where go spends 1.7k). reuse skips all of it,
/// and the already-faulted pages come back warm.
///
/// the pool is capped so a burst cannot hoard address space forever: beyond
/// STACK_POOL_MAX a returned stack just drops (one munmap, as before). stacks
/// keep whatever pages the previous task dirtied: same-process memory, and the
/// coroutine's setup overwrites what it uses, so stale bytes are inert.
pub(super) const STACK_POOL_MAX: usize = 64;
pub(super) static STACK_POOL: CacheLine<Mutex<Vec<corosensei::stack::DefaultStack>>> =
    CacheLine(Mutex::new(Vec::new()));

/// how many stacks each worker keeps for itself before returning them to the
/// shared `STACK_POOL`. a spawn takes a stack and a finish returns one, so
/// with only the shared pool every spawn and finish on every worker took its
/// lock.
pub(super) const WORKER_STACK_MAX: usize = 16;

/// a pooled stack if one is free (this worker's own first, then the shared
/// pool), otherwise a fresh allocation.
pub(super) fn take_stack() -> corosensei::stack::DefaultStack {
    if let Some(Some(stack)) = with_own_cache(|cache| cache.stacks.pop()) {
        return stack;
    }
    if let Some(stack) = STACK_POOL.lock().unwrap_or_else(|p| p.into_inner()).pop() {
        return stack;
    }
    match corosensei::stack::DefaultStack::new(STACK_SIZE) {
        Ok(stack) => stack,
        Err(err) => {
            runtime_fatal!("could not allocate a {STACK_SIZE}-byte green task stack: {err}")
        }
    }
}

/// recycle a finished coroutine's stack. `into_stack` requires the coroutine to
/// be complete, which both a clean return and an unwound panic guarantee; the
/// defensive `done()` check drops a hypothetical incomplete coroutine instead
/// of aborting.
pub(super) fn recycle_stack(coro: SendCoroutine) {
    if !coro.0.done() {
        return;
    }
    let mut stack = Some(coro.0.into_stack());
    with_own_cache(|cache| {
        if cache.stacks.len() < WORKER_STACK_MAX {
            cache.stacks.extend(stack.take());
        }
    });
    let Some(stack) = stack else {
        return;
    };
    let mut pool = STACK_POOL.lock().unwrap_or_else(|p| p.into_inner());
    if pool.len() < STACK_POOL_MAX {
        pool.push(stack);
    }
}

