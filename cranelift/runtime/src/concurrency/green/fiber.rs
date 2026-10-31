//! sanitizer annotations for the coroutine stack switches.
//!
//! a resume and a suspend each move the cpu to another stack. asan and tsan
//! track one stack per thread unless told otherwise: asan then reads a task's
//! frames as a wild stack, and tsan folds every task a worker runs into that
//! worker's own history. each task's `Fiber` holds what the two need, and the
//! hooks below run on either side of every switch:
//!
//! - worker side, around `resume`: `enter` before it and `back` after it.
//! - task side: `started` at the top of the body, `suspend` and `resumed`
//!   around the yielder's suspend, and `finish` once the body is done. the
//!   last runs from the guard `started` returns, so a panic unwinding the body
//!   runs it too.
//!
//! tsan gets one fiber per task, created when the block is armed and
//! destroyed by `retire` once the body has returned. only the worker side
//! switches it: `enter` makes the task's fiber current and `back` restores the
//! worker's. tsan keeps a shadow call stack per fiber, and the instrumented
//! corosensei frames between a hook and the real switch push onto whichever
//! fiber is current. with both switches on the worker side, those frames push
//! and pop on the task's fiber on every resume, so both shadow stacks stay
//! balanced. switching in `suspend` too would leave the worker's stack short
//! by the difference in depth on every round until it underflowed. the switches
//! synchronize (flag 0): control passing between a worker and its task is
//! sequential, and the cross-thread order comes from the scheduling word's
//! atomics, which tsan sees.
//!
//! a build without a sanitizer has no fields here, and every hook is empty.

#[cfg(any(pith_asan, pith_tsan))]
use std::cell::Cell;
#[cfg(any(pith_asan, pith_tsan))]
use std::ffi::c_void;

#[cfg(pith_asan)]
extern "C" {
    fn __sanitizer_start_switch_fiber(fake_stack_save: *mut *mut c_void, bottom: *const c_void, size: usize);
    fn __sanitizer_finish_switch_fiber(
        fake_stack_save: *mut c_void,
        bottom_old: *mut *const c_void,
        size_old: *mut usize,
    );
}

#[cfg(pith_tsan)]
extern "C" {
    fn __tsan_get_current_fiber() -> *mut c_void;
    fn __tsan_create_fiber(flags: u32) -> *mut c_void;
    fn __tsan_destroy_fiber(fiber: *mut c_void);
    fn __tsan_switch_to_fiber(fiber: *mut c_void, flags: u32);
}

/// the extent of a task's stack, which asan needs before switching onto it.
/// `UNKNOWN` (a test's hand-built coroutine) skips the asan calls.
#[derive(Clone, Copy)]
pub(super) struct StackBounds {
    #[cfg(pith_asan)]
    bottom: usize,
    #[cfg(pith_asan)]
    size: usize,
}

impl StackBounds {
    pub(super) const UNKNOWN: StackBounds = StackBounds {
        #[cfg(pith_asan)]
        bottom: 0,
        #[cfg(pith_asan)]
        size: 0,
    };

    #[inline(always)]
    pub(super) fn of(stack: &corosensei::stack::DefaultStack) -> StackBounds {
        #[cfg(pith_asan)]
        {
            use corosensei::stack::Stack;
            let bottom = stack.limit().get();
            StackBounds { bottom, size: stack.base().get() - bottom }
        }
        #[cfg(not(pith_asan))]
        {
            let _ = stack;
            StackBounds::UNKNOWN
        }
    }
}

/// a task's sanitizer state. the cells follow the block's claim protocol:
/// only the worker resuming the task, or the task itself, touches them.
pub(super) struct Fiber {
    #[cfg(pith_asan)]
    stack: Cell<StackBounds>,
    /// the stack of the worker that resumed the task, learned on each switch
    /// in and used to switch back out.
    #[cfg(pith_asan)]
    worker_bottom: Cell<*const c_void>,
    #[cfg(pith_asan)]
    worker_size: Cell<usize>,
    /// the task's fake stack, saved across a suspend.
    #[cfg(pith_asan)]
    fake_stack: Cell<*mut c_void>,
    #[cfg(pith_tsan)]
    fiber: Cell<*mut c_void>,
}

/// what the worker keeps across one resume.
pub(super) struct Resume {
    #[cfg(pith_asan)]
    fake_stack: *mut c_void,
    #[cfg(pith_tsan)]
    worker_fiber: *mut c_void,
}

/// runs `Fiber::finish` when the task body ends, by return or by unwind.
/// empty for a body that is not a task's.
pub(super) struct FinishGuard(pub(super) Option<&'static Fiber>);

impl Drop for FinishGuard {
    #[inline(always)]
    fn drop(&mut self) {
        if let Some(fiber) = self.0 {
            fiber.finish();
        }
    }
}

impl Fiber {
    pub(super) fn new() -> Fiber {
        Fiber {
            #[cfg(pith_asan)]
            stack: Cell::new(StackBounds::UNKNOWN),
            #[cfg(pith_asan)]
            worker_bottom: Cell::new(std::ptr::null()),
            #[cfg(pith_asan)]
            worker_size: Cell::new(0),
            #[cfg(pith_asan)]
            fake_stack: Cell::new(std::ptr::null_mut()),
            #[cfg(pith_tsan)]
            fiber: Cell::new(std::ptr::null_mut()),
        }
    }

    /// set up for a freshly armed task running on `stack`.
    #[inline(always)]
    pub(super) fn arm(&self, stack: StackBounds) {
        #[cfg(pith_asan)]
        {
            self.stack.set(stack);
            self.fake_stack.set(std::ptr::null_mut());
        }
        #[cfg(not(pith_asan))]
        let _ = stack;
        #[cfg(pith_tsan)]
        // SAFETY: plain tsan bookkeeping; the fiber is destroyed by `retire`.
        unsafe {
            if self.fiber.get().is_null() {
                self.fiber.set(__tsan_create_fiber(0));
            }
        }
    }

    /// worker side, right before resuming the task.
    #[inline(always)]
    pub(super) fn enter(&self) -> Resume {
        #[allow(unused_mut)]
        let mut resume = Resume {
            #[cfg(pith_asan)]
            fake_stack: std::ptr::null_mut(),
            // SAFETY: plain tsan bookkeeping.
            #[cfg(pith_tsan)]
            worker_fiber: unsafe { __tsan_get_current_fiber() },
        };
        #[cfg(pith_tsan)]
        // SAFETY: the task's fiber, which `arm` created.
        unsafe {
            __tsan_switch_to_fiber(self.fiber.get(), 0)
        };
        #[cfg(pith_asan)]
        {
            let stack = self.stack.get();
            if stack.size != 0 {
                // SAFETY: the bounds name the task's own stack.
                unsafe {
                    __sanitizer_start_switch_fiber(&mut resume.fake_stack, stack.bottom as *const c_void, stack.size)
                };
            }
        }
        resume
    }

    /// worker side, once the resume has returned to the worker's stack.
    #[inline(always)]
    pub(super) fn back(&self, resume: Resume) {
        #[cfg(pith_asan)]
        if self.stack.get().size != 0 {
            // SAFETY: pairs with the start in `enter`.
            unsafe { __sanitizer_finish_switch_fiber(resume.fake_stack, std::ptr::null_mut(), std::ptr::null_mut()) };
        }
        #[cfg(pith_tsan)]
        // SAFETY: the fiber `enter` found current.
        unsafe {
            __tsan_switch_to_fiber(resume.worker_fiber, 0)
        };
        let _ = resume;
    }

    /// task side, at the top of the body on its first resume. the guard runs
    /// `finish` when the body ends.
    #[inline(always)]
    pub(super) fn started(&'static self) -> FinishGuard {
        #[cfg(pith_asan)]
        if self.stack.get().size != 0 {
            self.learn_worker_stack(std::ptr::null_mut());
        }
        FinishGuard(Some(self))
    }

    /// task side, right before suspending back to the worker.
    #[inline(always)]
    pub(super) fn suspend(&self) {
        #[cfg(pith_asan)]
        if self.stack.get().size != 0 {
            // SAFETY: `learn_worker_stack` recorded the worker's stack.
            unsafe {
                __sanitizer_start_switch_fiber(
                    self.fake_stack.as_ptr(),
                    self.worker_bottom.get(),
                    self.worker_size.get(),
                )
            };
        }
    }

    /// task side, once a later resume has brought the task back, possibly on
    /// another worker.
    #[inline(always)]
    pub(super) fn resumed(&self) {
        #[cfg(pith_asan)]
        if self.stack.get().size != 0 {
            self.learn_worker_stack(self.fake_stack.get());
        }
    }

    /// task side, the body is done: the last switch back to the worker.
    #[inline(always)]
    fn finish(&self) {
        #[cfg(pith_asan)]
        if self.stack.get().size != 0 {
            // SAFETY: a null save tells asan this stack is being left for good.
            unsafe {
                __sanitizer_start_switch_fiber(
                    std::ptr::null_mut(),
                    self.worker_bottom.get(),
                    self.worker_size.get(),
                )
            };
        }
    }

    /// worker side, after the body has returned: drop the task's tsan fiber.
    #[inline(always)]
    pub(super) fn retire(&self) {
        #[cfg(pith_tsan)]
        // SAFETY: the task's body has finished and `back` has restored the
        // worker's fiber, so nothing runs on this one again.
        unsafe {
            let fiber = self.fiber.replace(std::ptr::null_mut());
            if !fiber.is_null() {
                __tsan_destroy_fiber(fiber);
            }
        }
    }

    #[cfg(pith_asan)]
    #[inline(always)]
    fn learn_worker_stack(&self, fake_stack: *mut c_void) {
        let mut bottom = std::ptr::null();
        let mut size = 0;
        // SAFETY: completes the switch onto this task's stack.
        unsafe { __sanitizer_finish_switch_fiber(fake_stack, &mut bottom, &mut size) };
        self.worker_bottom.set(bottom);
        self.worker_size.set(size);
    }
}
