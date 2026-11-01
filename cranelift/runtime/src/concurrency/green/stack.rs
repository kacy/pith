//! coroutine stacks, pooled so a spawn rarely maps a new one.

use super::*;

/// the default coroutine stack size. a green task gets 1 MiB, which is
/// generous for typical task bodies and keeps a large fan-out affordable: the
/// size is reserved address space, and only the pages a task touches are
/// faulted in. a deeply recursive task can still overflow it. the guard page
/// below the stack then faults, and `stack_guard` reports a stack overflow in
/// the task. `PITH_GREEN_STACK_KB` changes the size (see `stack_size`).
pub(super) const DEFAULT_STACK_SIZE: usize = 1024 * 1024;

/// the bounds `PITH_GREEN_STACK_KB` is clamped to: 64 KiB, below which the
/// runtime's own frames leave a task little room, and 1 GiB, past which a
/// few thousand tasks exhaust the address space.
const MIN_STACK_KB: usize = 64;
const MAX_STACK_KB: usize = 1024 * 1024;

/// the coroutine stack size for this process: `PITH_GREEN_STACK_KB` in KiB,
/// clamped to the bounds above, or `DEFAULT_STACK_SIZE` when it is unset or
/// not a number. read once, so every stack the pools hold has this size.
pub(super) fn stack_size() -> usize {
    static SIZE: OnceLock<usize> = OnceLock::new();
    *SIZE.get_or_init(|| {
        std::env::var("PITH_GREEN_STACK_KB")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .map(|kb| kb.clamp(MIN_STACK_KB, MAX_STACK_KB) * 1024)
            .unwrap_or(DEFAULT_STACK_SIZE)
    })
}

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
    let size = stack_size();
    match corosensei::stack::DefaultStack::new(size) {
        Ok(stack) => stack,
        Err(err) => {
            runtime_fatal!("could not allocate a {size}-byte green task stack: {err}")
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

