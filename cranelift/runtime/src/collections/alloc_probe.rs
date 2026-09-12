//! A counting allocator, for the collection tests only.
//!
//! The claim the map and set key paths have to hold is a count, not a time: a
//! lookup, a membership test, a removal, or an overwrite of a key the container
//! already holds must allocate nothing at all, and only a miss that inserts may
//! allocate. No other instrument here can see that. Instruction counts move for
//! a dozen unrelated reasons and a leak gate only sees what is never freed, so
//! the test build installs a `#[global_allocator]` that forwards to the system
//! allocator and counts the calls on the way through.
//!
//! The counter is per thread. Cargo runs tests in parallel, so a process-wide
//! counter would report whatever the other tests on the machine happened to be
//! doing. The thread-local's initialiser is `const`, which is what keeps its
//! first touch from allocating and recursing into the allocator doing the
//! touching.
//!
//! `#[cfg(test)]` means this is compiled only into this crate's own test
//! harness. Programs that link the runtime get the ordinary system allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

pub struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        System.alloc(layout)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record();
        System.alloc_zeroed(layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record();
        System.realloc(ptr, layout, new_size)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
}

/// A thread whose locals have already been torn down still frees, and can
/// still allocate; a count lost there belongs to no test, so the failure to
/// reach the counter is ignored rather than reported.
fn record() {
    let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// How many allocations `body` makes on this thread.
///
/// Deallocations are deliberately not counted: what the key paths promise is
/// that nothing is allocated, and a count of zero allocations is also a count
/// of zero matching frees.
pub fn allocations_during(body: impl FnOnce()) -> u64 {
    let before = ALLOCATIONS.with(Cell::get);
    body();
    ALLOCATIONS.with(Cell::get) - before
}
