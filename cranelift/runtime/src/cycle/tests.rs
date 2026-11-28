use super::collect::*;
use super::stw::*;
use super::suspects::*;
use super::thread::*;
use super::tracers::*;
use super::*;
use crate::child_kind::STRUCT as CHILD_KIND_STRUCT;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar};
use std::time::{Duration, Instant};

use crate::collections::list::{
    pith_list_len, pith_list_push_value, pith_list_release, pith_list_retain_handle,
};
use crate::runtime_core::{
    closure_buffered_for_tests, pith_closure_get_fn, pith_closure_new, pith_closure_release,
    pith_closure_retain, pith_closure_set_env_rc, pith_struct_alloc, pith_struct_release,
    pith_struct_retain, pith_struct_set_dtor, pith_struct_weak_release, pith_struct_weak_upgrade,
    pith_struct_weak_retain, struct_dead_word_for_tests, struct_weak_count_for_tests,
};

fn locked() -> std::sync::MutexGuard<'static, ()> {
    crate::sync::lock(&CYCLE_TEST_LOCK)
}

#[test]
fn exited_mutator_slots_are_reused_not_accumulated() {
    let _guard = locked();
    // each spawned thread registers a slot on first use and marks it
    // exited at teardown; a later registration must adopt an exited
    // slot instead of growing the registry. sequential joins make the
    // exited state visible to the next spawn, so the registry may only
    // grow by a bounded handful over many short-lived threads, the
    // os-thread backend's task-per-thread shape in miniature.
    let before = lock_mutators().len();
    for _ in 0..64 {
        std::thread::spawn(|| {
            let _ = mutator_slot();
        })
        .join()
        .expect("mutator thread");
    }
    let after = lock_mutators().len();
    // the sequential answer is before + 1: the first thread pushes at
    // most one fresh slot, marks it exited at join, and every later
    // thread's CAS adopts it. the extra unit of slack is for a thread
    // OUTSIDE this test: the registry is process-global and cargo
    // runs other tests' threads concurrently, so one of them gating
    // mid-loop legitimately pushes its own slot.
    assert!(
        after <= before + 2,
        "registry grew from {before} to {after} over 64 sequential threads"
    );
}

#[test]
fn concurrent_mutators_hold_distinct_slots() {
    let _guard = locked();
    // the property the resurrection CAS actually guarantees, which the
    // sequential test above cannot see: an exited slot is handed to at
    // most ONE adopter, so two simultaneously live mutators never share
    // a state word (a shared word would let one thread's park report a
    // different thread as stopped). eight threads register, rendezvous
    // on a barrier while every one of them still holds its slot, and
    // report the slot's address; all eight must be pairwise distinct.
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let (tx, rx) = std::sync::mpsc::channel();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let tx = tx.clone();
            std::thread::spawn(move || {
                let slot = mutator_slot().expect("thread-local storage is live");
                // no thread passes the barrier until all eight hold a
                // slot, so no slot can be marked exited and recycled
                // while another registration is still to come
                barrier.wait();
                tx.send(Arc::as_ptr(&slot) as usize)
                    .expect("receiver outlives the senders");
            })
        })
        .collect();
    drop(tx);
    let mut ptrs: Vec<usize> = rx.iter().collect();
    for thread in threads {
        thread.join().expect("mutator thread");
    }
    assert_eq!(ptrs.len(), 8);
    ptrs.sort_unstable();
    ptrs.dedup();
    assert_eq!(ptrs.len(), 8, "two live mutators shared a slot");
}

#[test]
fn flag_off_release_paths_stay_untouched() {
    let _guard = locked();
    force_enabled_for_tests(false);
    reset_for_tests();

    let s = unsafe { pith_struct_alloc(2) };
    unsafe { pith_struct_retain(s) };
    unsafe { pith_struct_release(s) }; // 2 -> 1: would buffer with the flag on
    assert_eq!(unsafe { struct_dead_word_for_tests(s) }, 0);
    assert_eq!(unsafe { struct_weak_count_for_tests(s) }, 1);
    assert_eq!(suspect_count_for_tests(s as usize), 0);
    unsafe { pith_struct_release(s) }; // frees normally

    let list = unsafe { crate::collections::list::pith_list_new(8, 0) };
    unsafe { pith_list_retain_handle(list.ptr as i64) };
    unsafe { pith_list_release(list) };
    assert_eq!(
        suspect_count_for_tests(list.ptr as usize),
        0,
        "flag off must keep the buffer empty"
    );
    unsafe { pith_list_release(list) }; // frees normally, no graveyard
    assert_eq!(graveyard_count_for_tests(list.ptr as usize), 0);
    assert_eq!(suspects_len_for_tests(), 0);
}

#[test]
#[ignore = "enables the collector flag; run serially via make test-cycle-gc"]
fn struct_decremented_to_nonzero_is_buffered_exactly_once() {
    let _guard = locked();
    force_enabled_for_tests(true);
    reset_for_tests();

    let s = unsafe { pith_struct_alloc(2) };
    unsafe { pith_struct_retain(s) };
    unsafe { pith_struct_retain(s) }; // strong 3
    unsafe { pith_struct_release(s) }; // 3 -> 2: buffered
    assert_eq!(suspect_count_for_tests(s as usize), 1);
    assert_eq!(
        unsafe { struct_dead_word_for_tests(s) },
        2,
        "buffered bit set, not dead"
    );
    assert_eq!(unsafe { struct_weak_count_for_tests(s) }, 2, "buffer holds one weak");
    unsafe { pith_struct_release(s) }; // 2 -> 1: already buffered, no second entry
    assert_eq!(suspect_count_for_tests(s as usize), 1);
    assert_eq!(unsafe { struct_weak_count_for_tests(s) }, 2);

    force_enabled_for_tests(false);
    reset_for_tests(); // clears the bit, drops the buffer's weak
    assert_eq!(unsafe { struct_dead_word_for_tests(s) }, 0);
    assert_eq!(unsafe { struct_weak_count_for_tests(s) }, 1);
    unsafe { pith_struct_release(s) };
}

#[test]
#[ignore = "enables the collector flag; run serially via make test-cycle-gc"]
fn buffered_struct_survives_the_death_of_its_value() {
    let _guard = locked();
    force_enabled_for_tests(true);
    reset_for_tests();

    let s = unsafe { pith_struct_alloc(1) };
    unsafe { pith_struct_retain(s) };
    unsafe { pith_struct_release(s) }; // 2 -> 1: buffered, weak now 2
    unsafe { pith_struct_release(s) }; // 1 -> 0: dies while buffered
                                       // dead bit and buffered bit both set; the buffer's weak count
                                       // is the only thing keeping the header alive.
    assert_eq!(unsafe { struct_dead_word_for_tests(s) }, 3);
    assert_eq!(unsafe { struct_weak_count_for_tests(s) }, 1);
    assert_eq!(unsafe { pith_struct_weak_upgrade(s) }, 0, "dead reads as none");
    assert_eq!(suspect_count_for_tests(s as usize), 1);

    force_enabled_for_tests(false);
    reset_for_tests(); // drops the buffer's weak: header freed here
}

#[test]
#[ignore = "enables the collector flag; run serially via make test-cycle-gc"]
fn weak_upgrade_ignores_the_buffered_bit() {
    let _guard = locked();
    force_enabled_for_tests(true);
    reset_for_tests();

    let s = unsafe { pith_struct_alloc(1) };
    unsafe { pith_struct_weak_retain(s) };
    unsafe { pith_struct_retain(s) };
    unsafe { pith_struct_release(s) }; // buffered, still alive
    assert_eq!(unsafe { struct_dead_word_for_tests(s) }, 2);
    assert_eq!(unsafe { pith_struct_weak_upgrade(s) }, s, "buffered but alive");
    unsafe { pith_struct_release(s) }; // the upgrade's count
    unsafe { pith_struct_release(s) }; // dies buffered
    assert_eq!(unsafe { pith_struct_weak_upgrade(s) }, 0, "buffered and dead");
    unsafe { pith_struct_weak_release(s) };

    force_enabled_for_tests(false);
    reset_for_tests();
}

#[test]
#[ignore = "enables the collector flag; run serially via make test-cycle-gc"]
fn buffered_list_dies_into_the_graveyard() {
    let _guard = locked();
    force_enabled_for_tests(true);
    reset_for_tests();

    // a struct element observed through a weak reference proves the
    // list's death still cascades into its elements.
    let elem = unsafe { pith_struct_alloc(1) };
    unsafe { pith_struct_weak_retain(elem) };

    let list = unsafe { crate::collections::list::pith_list_new(8, 4) }; // struct elements
    unsafe { pith_list_push_value(list, elem) }; // list retains
    unsafe { pith_struct_release(elem) }; // list holds the only strong count
    assert_eq!(unsafe { pith_struct_weak_upgrade(elem) }, elem);
    unsafe { pith_struct_release(elem) }; // the upgrade's count

    unsafe { pith_list_retain_handle(list.ptr as i64) };
    unsafe { pith_list_release(list) }; // 2 -> 1: buffered
    assert_eq!(suspect_count_for_tests(list.ptr as usize), 1);
    unsafe { pith_list_release(list) }; // 1 -> 0: dies buffered

    assert_eq!(graveyard_count_for_tests(list.ptr as usize), 1);
    assert_eq!(unsafe { pith_struct_weak_upgrade(elem) }, 0, "element was released");
    assert_eq!(
        pith_list_len(list),
        0,
        "magic scrubbed: handle no longer validates"
    );
    unsafe { pith_list_retain_handle(list.ptr as i64) }; // must be a no-op, not a revival

    unsafe { pith_struct_weak_release(elem) };
    force_enabled_for_tests(false);
    reset_for_tests();
}

#[test]
#[ignore = "enables the collector flag; run serially via make test-cycle-gc"]
fn buffered_closure_dies_into_the_graveyard() {
    let _guard = locked();
    force_enabled_for_tests(true);
    reset_for_tests();

    let captured = unsafe { pith_struct_alloc(1) };
    unsafe { pith_struct_weak_retain(captured) };

    let closure = pith_closure_new(0x1000);
    // tag 6 = struct, per the closure env tag codes; ownership of the
    // strong count transfers into the closure.
    unsafe { pith_closure_set_env_rc(closure, 0, captured, 6) };

    unsafe { pith_closure_retain(closure) };
    unsafe { pith_closure_release(closure) }; // 2 -> 1: buffered
    assert!(unsafe { closure_buffered_for_tests(closure) });
    assert_eq!(suspect_count_for_tests(closure as usize), 1);
    unsafe { pith_closure_release(closure) }; // 1 -> 0: dies buffered

    assert_eq!(graveyard_count_for_tests(closure as usize), 1);
    assert_eq!(unsafe { pith_struct_weak_upgrade(captured) }, 0, "capture was released");
    assert_eq!(unsafe { pith_closure_get_fn(closure) }, 0, "magic scrubbed");

    unsafe { pith_struct_weak_release(captured) };
    force_enabled_for_tests(false);
    reset_for_tests();
}

#[test]
#[ignore = "enables the collector flag; run serially via make test-cycle-gc"]
fn overflow_degrades_to_a_leak_never_a_dangle() {
    let _guard = locked();
    force_enabled_for_tests(true);
    reset_for_tests();

    let mut kept = Vec::new();
    while suspects_len_for_tests() < CYCLE_SUSPECT_CAP {
        let s = unsafe { pith_struct_alloc(1) };
        unsafe { pith_struct_retain(s) };
        unsafe { pith_struct_release(s) }; // buffered
        kept.push(s);
    }

    let overflowed_before = CYCLE_SUSPECTS_OVERFLOWED.load(Ordering::Relaxed);
    let victim = unsafe { pith_struct_alloc(1) };
    unsafe { pith_struct_retain(victim) };
    unsafe { pith_struct_release(victim) }; // buffer full: turned away
    assert_eq!(
        suspect_count_for_tests(victim as usize),
        0,
        "no push at cap"
    );
    assert_eq!(unsafe { struct_dead_word_for_tests(victim) }, 0, "bit cleared again");
    assert_eq!(
        unsafe { struct_weak_count_for_tests(victim) },
        1,
        "weak released again"
    );
    assert!(CYCLE_SUSPECTS_OVERFLOWED.load(Ordering::Relaxed) > overflowed_before);

    force_enabled_for_tests(false);
    reset_for_tests();
    unsafe { pith_struct_release(victim) };
    for s in kept {
        unsafe { pith_struct_release(s) };
    }
}

#[test]
fn registered_tracer_pairs_are_looked_up_by_dtor_address() {
    let _guard = locked();
    // addresses only need to be distinct keys; nothing dereferences them
    // until a collector walks the table.
    pith_cc_register_tracer(0x1000, 0x2000);
    pith_cc_register_tracer(0x3000, 0x4000);
    assert_eq!(tracer_for_dtor(0x1000), Some(0x2000));
    assert_eq!(tracer_for_dtor(0x3000), Some(0x4000));
    assert_eq!(tracer_for_dtor(0x5000), None, "unregistered dtor is a leaf");
    assert!(tracer_count_for_tests() >= 2);

    // a re-registration (a module reloaded in-process) replaces, never
    // duplicates
    let before = tracer_count_for_tests();
    pith_cc_register_tracer(0x1000, 0x6000);
    assert_eq!(tracer_for_dtor(0x1000), Some(0x6000));
    assert_eq!(tracer_count_for_tests(), before);

    // null halves are ignored: no entry, no panic
    pith_cc_register_tracer(0, 0x7000);
    pith_cc_register_tracer(0x8000, 0);
    assert_eq!(tracer_for_dtor(0), None);
    assert_eq!(tracer_for_dtor(0x8000), None);
    assert_eq!(tracer_count_for_tests(), before);
}

#[test]
#[ignore = "enables the collector flag; run serially via make test-cycle-gc"]
fn visit_with_no_hook_is_inert_and_counts_only_when_enabled() {
    let _guard = locked();
    force_enabled_for_tests(false);
    let before = CYCLE_VISITS_UNHOOKED.load(Ordering::Relaxed);
    pith_cc_visit(0x1234, 6);
    assert_eq!(
        CYCLE_VISITS_UNHOOKED.load(Ordering::Relaxed),
        before,
        "flag off: not even the counter moves"
    );

    force_enabled_for_tests(true);
    pith_cc_visit(0x1234, 6);
    pith_cc_visit(0x5678, 2);
    assert_eq!(CYCLE_VISITS_UNHOOKED.load(Ordering::Relaxed), before + 2);
    pith_cc_visit(0, 6); // a none payload is not a child
    assert_eq!(CYCLE_VISITS_UNHOOKED.load(Ordering::Relaxed), before + 2);
    force_enabled_for_tests(false);
}

#[test]
#[ignore = "enables the collector flag; run serially via make test-cycle-gc"]
fn visit_dispatches_to_an_installed_hook() {
    static SEEN_CHILD: AtomicU64 = AtomicU64::new(0);
    static SEEN_KIND: AtomicU64 = AtomicU64::new(0);
    extern "C" fn recording_hook(child: i64, kind: i64) {
        SEEN_CHILD.store(child as u64, Ordering::Relaxed);
        SEEN_KIND.store(kind as u64, Ordering::Relaxed);
    }

    let _guard = locked();
    force_enabled_for_tests(true);
    install_visit_hook(recording_hook as *const () as usize);
    let unhooked_before = CYCLE_VISITS_UNHOOKED.load(Ordering::Relaxed);
    pith_cc_visit(0xabcd, 7);
    assert_eq!(SEEN_CHILD.load(Ordering::Relaxed), 0xabcd);
    assert_eq!(SEEN_KIND.load(Ordering::Relaxed), 7);
    assert_eq!(
        CYCLE_VISITS_UNHOOKED.load(Ordering::Relaxed),
        unhooked_before,
        "a hooked visit is delivered, not counted as unhooked"
    );

    install_visit_hook(0);
    pith_cc_visit(0x9999, 3);
    assert_eq!(SEEN_CHILD.load(Ordering::Relaxed), 0xabcd, "hook removed");
    assert_eq!(
        CYCLE_VISITS_UNHOOKED.load(Ordering::Relaxed),
        unhooked_before + 1
    );
    force_enabled_for_tests(false);
}

#[test]
#[ignore = "enables the collector flag; run serially via make test-cycle-gc"]
fn concurrent_releases_buffer_a_shared_struct_once() {
    let _guard = locked();
    force_enabled_for_tests(true);
    reset_for_tests();

    let s = unsafe { pith_struct_alloc(1) };
    for _ in 0..8 {
        unsafe { pith_struct_retain(s) }; // strong 9: one per releasing step + ours
    }
    let a = std::thread::spawn(move || {
        for _ in 0..4 {
            unsafe { pith_struct_release(s) };
        }
    });
    let b = std::thread::spawn(move || {
        for _ in 0..4 {
            unsafe { pith_struct_release(s) };
        }
    });
    a.join().expect("releasing thread panicked");
    b.join().expect("releasing thread panicked");

    assert_eq!(unsafe { pith_struct_weak_upgrade(s) }, s, "still alive: we hold a count");
    unsafe { pith_struct_release(s) }; // the upgrade's count
    assert_eq!(
        suspect_count_for_tests(s as usize),
        1,
        "one entry, many releases"
    );
    assert_eq!(unsafe { struct_dead_word_for_tests(s) }, 2);
    assert_eq!(unsafe { struct_weak_count_for_tests(s) }, 2);

    force_enabled_for_tests(false);
    reset_for_tests();
    unsafe { pith_struct_release(s) };
}

// --- stop-the-world tests ----------------------------------------------
//
// these force the flag on under CYCLE_TEST_LOCK, like the buffer tests.
// the mutator registry is process-global, and while the flag is on *any*
// test thread in this binary that crosses a bracket registers a slot that
// reads `running` until its thread exits, so tests that expect a stop to
// SUCCEED retry the attempt for a while instead of asserting the first
// one, which is exactly how the collector treats a `false` anyway.

/// spawn a thread the way the runtime spawns mutators: slot registered
/// here first, adopted by the thread. returns the slot for state asserts.
fn spawn_registered<F: FnOnce() + Send + 'static>(
    body: F,
) -> (Arc<MutatorSlot>, std::thread::JoinHandle<()>) {
    let slot = mutator_slot_for_spawn().expect("the flag is on in these tests");
    let adopted = Some(Arc::clone(&slot));
    let handle = std::thread::spawn(move || {
        adopt_mutator_slot(adopted);
        body();
    });
    (slot, handle)
}

/// retry the stop until it lands, with a ceiling so a congested process
/// skips the test instead of hanging the suite. the pause between failed
/// attempts is load-bearing: a failed attempt parks every gated thread in
/// the process for the whole stop timeout, so back-to-back retries starve
/// unrelated tests sharing the binary; the gap hands them the core back.
fn stop_eventually() -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if pith_cycle_stop_the_world() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// a congested process (unrelated tests holding running mutator slots for
/// longer than the retry ceiling) makes a stop-success assertion hang or
/// flake rather than fail honestly. the property is still pinned by the
/// solo-process leak gate, so these tests step aside loudly instead.
fn skip_congested(test: &str) {
    eprintln!("{test}: skipped, the process never quiesced under concurrent tests");
}

fn await_state(slot: &MutatorSlot, expected: u8) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while slot.state_for_tests() != expected {
        assert!(
            Instant::now() < deadline,
            "slot never reached state {expected}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// resumes the world when dropped, so an assert that fails while the
/// world is stopped unwinds into a resume instead of wedging every other
/// parked test thread in the process.
struct WorldResumer;
impl Drop for WorldResumer {
    fn drop(&mut self) {
        pith_cycle_resume_the_world();
    }
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn stop_catches_mutators_at_their_gates() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(50);

    let done = Arc::new(AtomicBool::new(false));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let done = Arc::clone(&done);
            spawn_registered(move || {
                // the shape of a worker between tasks: gate, work, gate.
                while !done.load(Ordering::Relaxed) {
                    mutator_gate();
                    std::thread::yield_now();
                }
            })
        })
        .collect();

    if !stop_eventually() {
        skip_congested("stop_catches_mutators_at_their_gates");
        done.store(true, Ordering::Relaxed);
        for (_, handle) in workers {
            handle.join().expect("gate-looping thread panicked");
        }
        force_enabled_for_tests(false);
        return;
    }
    {
        let _resume = WorldResumer;
        for (slot, _) in &workers {
            assert_eq!(
                slot.state_for_tests(),
                MUTATOR_PARKED_FOR_GC,
                "a stopped gate-looper is parked, not merely slow"
            );
        }
    }
    done.store(true, Ordering::Relaxed);
    for (slot, handle) in workers {
        handle.join().expect("gate-looping thread panicked");
        assert_eq!(
            slot.state_for_tests(),
            MUTATOR_EXITED,
            "a joined thread's slot reads exited"
        );
    }
    force_enabled_for_tests(false);
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn stop_times_out_on_a_mutator_that_never_gates() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(30);

    let done = Arc::new(AtomicBool::new(false));
    let spinner_done = Arc::clone(&done);
    let (slot, handle) = spawn_registered(move || {
        // pith code with no compiled safe-points: checks nothing, ever.
        while !spinner_done.load(Ordering::Relaxed) {
            std::hint::spin_loop();
        }
    });

    assert!(
        !pith_cycle_stop_the_world(),
        "a spinner that never gates must time the stop out"
    );
    assert!(
        !GC_STOP.load(Ordering::SeqCst),
        "an abandoned stop withdraws the request"
    );
    assert_eq!(
        slot.state_for_tests(),
        MUTATOR_RUNNING,
        "the spinner never noticed"
    );

    done.store(true, Ordering::Relaxed);
    handle.join().expect("spinning thread panicked");
    force_enabled_for_tests(false);
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn a_bracketed_wait_counts_as_stopped_and_the_exit_parks() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(50);

    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let finished = Arc::new(AtomicBool::new(false));
    let thread_release = Arc::clone(&release);
    let thread_finished = Arc::clone(&finished);
    let (slot, handle) = spawn_registered(move || {
        {
            // the shape of every runtime bracket: enter, one blocking
            // wait that reads no heap handle, exit.
            let _native = native_bracket();
            let (lock, cv) = &*thread_release;
            let mut go = crate::sync::lock(lock);
            while !*go {
                go = cv.wait(go).unwrap_or_else(|p| p.into_inner());
            }
        }
        // only reachable once the bracket exit let us back in.
        thread_finished.store(true, Ordering::Relaxed);
    });

    await_state(&slot, MUTATOR_NATIVE);
    if !stop_eventually() {
        skip_congested("a_bracketed_wait_counts_as_stopped_and_the_exit_parks");
        let (lock, cv) = &*release;
        *crate::sync::lock(lock) = true;
        cv.notify_all();
        handle.join().expect("bracketed thread panicked");
        force_enabled_for_tests(false);
        return;
    }
    {
        let _resume = WorldResumer;
        // wake the waiter while the world is stopped: it leaves its wait,
        // and the bracket's exit must park it before any code after the
        // bracket runs.
        {
            let (lock, cv) = &*release;
            *crate::sync::lock(lock) = true;
            cv.notify_all();
        }
        await_state(&slot, MUTATOR_PARKED_FOR_GC);
        assert!(
            !finished.load(Ordering::Relaxed),
            "the exit parked before code past the bracket ran"
        );
    }
    handle.join().expect("bracketed thread panicked");
    assert!(finished.load(Ordering::Relaxed), "resume let it finish");
    force_enabled_for_tests(false);
}

thread_local! {
    // run by `exit_native` between its two steps on this thread, so a test
    // can hold a mutator inside the handshake while a stop scans it.
    static EXIT_PAUSE: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn between_exit_steps() {
    EXIT_PAUSE.with(|pause| {
        if let Some(pause) = pause.borrow_mut().as_mut() {
            pause();
        }
    });
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn a_stop_never_lands_between_the_bracket_exit_steps() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(2000);

    // the mutator stops inside its bracket exit, between the state store
    // and the request load (in whichever order the exit takes them), and
    // a stop scans it there. the stop may only succeed once the exit has
    // parked: a mutator that runs past the bracket while the world reads
    // as stopped is the race.
    let (at_pause_tx, at_pause_rx) = std::sync::mpsc::channel::<()>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let finished = Arc::new(AtomicBool::new(false));
    let thread_finished = Arc::clone(&finished);
    let (slot, handle) = spawn_registered(move || {
        {
            let _native = native_bracket();
            EXIT_PAUSE.with(|pause| {
                *pause.borrow_mut() = Some(Box::new(move || {
                    let _ = at_pause_tx.send(());
                    let _ = go_rx.recv();
                }));
            });
        }
        EXIT_PAUSE.with(|pause| *pause.borrow_mut() = None);
        thread_finished.store(true, Ordering::SeqCst);
    });

    at_pause_rx.recv().expect("the mutator reached the pause");
    let stopper = std::thread::spawn(pith_cycle_stop_the_world);
    while !GC_STOP.load(Ordering::SeqCst) {
        std::thread::yield_now();
    }
    // let the stopper scan the paused mutator many times over.
    std::thread::sleep(Duration::from_millis(20));
    go_tx.send(()).expect("the mutator is waiting");
    let stopped = stopper.join().expect("stopper panicked");
    if !stopped {
        skip_congested("a_stop_never_lands_between_the_bracket_exit_steps");
        handle.join().expect("bracketed thread panicked");
        force_enabled_for_tests(false);
        return;
    }
    {
        let _resume = WorldResumer;
        let deadline = Instant::now() + Duration::from_secs(10);
        while slot.state_for_tests() != MUTATOR_PARKED_FOR_GC
            && !finished.load(Ordering::SeqCst)
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !finished.load(Ordering::SeqCst),
            "the mutator ran past its bracket while the world read as stopped"
        );
        assert_eq!(slot.state_for_tests(), MUTATOR_PARKED_FOR_GC);
    }
    handle.join().expect("bracketed thread panicked");
    assert!(finished.load(Ordering::SeqCst), "resume let it finish");
    force_enabled_for_tests(false);
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn a_stop_waits_for_main_before_its_first_gate() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(30);

    // a thread playing main: registered the way the process start
    // registers main, then mutating without ever reaching a gate.
    let (started_tx, started_rx) = std::sync::mpsc::channel::<Option<Arc<MutatorSlot>>>();
    let done = Arc::new(AtomicBool::new(false));
    let spinner_done = Arc::clone(&done);
    let handle = std::thread::spawn(move || {
        register_main_mutator();
        let _ = started_tx.send(existing_mutator_slot());
        while !spinner_done.load(Ordering::Relaxed) {
            std::hint::spin_loop();
        }
    });
    let slot = started_rx.recv().expect("main started");
    let stopped = pith_cycle_stop_the_world();
    done.store(true, Ordering::Relaxed);
    handle.join().expect("main stand-in panicked");
    if stopped {
        pith_cycle_resume_the_world();
    }
    force_enabled_for_tests(false);
    assert!(!stopped, "the stop must wait for main, which has not reached a gate");
    assert!(slot.is_some(), "main registers at start");
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn exited_slots_are_skipped_by_the_rendezvous() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(50);

    let (slot, handle) = spawn_registered(|| {});
    handle.join().expect("exiting thread panicked");
    assert_eq!(slot.state_for_tests(), MUTATOR_EXITED);

    if !stop_eventually() {
        skip_congested("exited_slots_are_skipped_by_the_rendezvous");
        force_enabled_for_tests(false);
        return;
    }
    pith_cycle_resume_the_world();
    force_enabled_for_tests(false);
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn with_world_stopped_runs_the_closure_against_a_parked_world() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(50);

    // give the calling thread a slot of its own: the stop must mark it
    // native for the duration rather than wait on it (the main-thread
    // test-caller case the seam documents).
    mutator_gate();

    let done = Arc::new(AtomicBool::new(false));
    let gate_done = Arc::clone(&done);
    let (slot, handle) = spawn_registered(move || {
        while !gate_done.load(Ordering::Relaxed) {
            mutator_gate();
            std::thread::yield_now();
        }
    });

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut observed = None;
    while observed.is_none() && Instant::now() < deadline {
        observed = with_world_stopped(|| slot.state_for_tests());
        if observed.is_none() {
            // the same starvation gap as stop_eventually.
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    if observed.is_none() {
        skip_congested("with_world_stopped_runs_the_closure_against_a_parked_world");
        done.store(true, Ordering::Relaxed);
        handle.join().expect("gate-looping thread panicked");
        force_enabled_for_tests(false);
        return;
    }
    assert_eq!(
        observed,
        Some(MUTATOR_PARKED_FOR_GC),
        "the closure ran while the other mutator was parked"
    );
    assert_eq!(
        existing_mutator_slot()
            .expect("this thread registered above")
            .state_for_tests(),
        MUTATOR_RUNNING,
        "the caller is a running mutator again after the resume"
    );

    done.store(true, Ordering::Relaxed);
    handle.join().expect("gate-looping thread panicked");
    force_enabled_for_tests(false);
}

#[test]
fn flag_off_gates_and_brackets_are_inert() {
    let _guard = locked();
    force_enabled_for_tests(false);

    // the property is thread-local: this thread's gates and brackets
    // must not register it. the global slot count is not stable here:
    // an unrelated test spawning the green workers registers theirs
    // concurrently, so counting the registry races.
    assert!(existing_mutator_slot().is_none(), "no slot before");
    mutator_gate();
    {
        let _native = native_bracket();
    }
    adopt_mutator_slot(mutator_slot_for_spawn());
    assert!(
        existing_mutator_slot().is_none(),
        "no slot registered with the flag off"
    );
    assert!(
        !pith_cycle_stop_the_world(),
        "no stop-the-world with the flag off"
    );
    assert!(!GC_STOP.load(Ordering::SeqCst), "no request left behind");
    assert_eq!(with_world_stopped(|| 1), None);
    pith_cycle_resume_the_world(); // a no-op, not a panic
}

// --- collection pass tests ---------------------------------------------
//
// rings are built from raw runtime calls with a hand-written destructor
// and tracer pair, the same shapes the emitter generates: the dtor
// releases the one field, the tracer reports it to pith_cc_visit.

/// dtor for a one-field box whose field holds another struct.
unsafe extern "C" fn ring_dtor(ptr: i64) {
    let field = unsafe { *(ptr as *const i64) };
    if field != 0 {
        unsafe { pith_struct_release(field) };
    }
}

/// tracer twin of `ring_dtor`: report the field instead of releasing it.
extern "C" fn ring_tracer(ptr: i64) {
    let field = unsafe { *(ptr as *const i64) };
    pith_cc_visit(field, 6);
}

/// dtor for a one-field box whose field holds a closure.
unsafe extern "C" fn holder_dtor(ptr: i64) {
    let closure = unsafe { *(ptr as *const i64) };
    if closure != 0 {
        unsafe { pith_closure_release(closure) };
    }
}

extern "C" fn holder_tracer(ptr: i64) {
    let closure = unsafe { *(ptr as *const i64) };
    pith_cc_visit(closure, 7);
}

/// run a collection against a provably stopped world, retrying because
/// unrelated test threads may hold running mutator slots for a while.
fn collect_eventually() -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if with_world_stopped(|| unsafe { collect() }).is_some() {
            return true;
        }
        // the same starvation gap as stop_eventually: a failed attempt
        // parked every gated thread for the whole stop timeout.
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// a garbage two-struct ring: a owns b through its field, b owns a back,
/// and no external count remains once the builder's is dropped.
unsafe fn build_garbage_ring() -> (i64, i64) {
    pith_cc_register_tracer(ring_dtor as *const () as usize as i64, ring_tracer as *const () as usize as i64);
    let a = unsafe { pith_struct_alloc(1) };
    let b = unsafe { pith_struct_alloc(1) };
    unsafe {
        pith_struct_set_dtor(a, ring_dtor as *const () as usize as i64);
        pith_struct_set_dtor(b, ring_dtor as *const () as usize as i64);
        *(a as *mut i64) = b; // our count on b transfers into a's field
        pith_struct_retain(a);
        *(b as *mut i64) = a; // b's edge holds its own count on a
        pith_struct_weak_retain(a);
        pith_struct_weak_retain(b);
    }
    (a, b)
}

#[test]
fn trial_deletion_graph_separates_garbage_cycles_from_live_roots() {
    // Pure-Rust synthetic graph test (no raw pointers):
    // - Cycle 1 <-> 2 with counts (1, 1) -> garbage (both white)
    // - Cycle 3 <-> 4 with counts (2, 1) -> externally rooted at 3 (both black)
    let graph: HashMap<usize, (i64, Vec<(usize, u8)>)> = HashMap::from([
        (1, (1, vec![(2, CHILD_KIND_STRUCT)])),
        (2, (1, vec![(1, CHILD_KIND_STRUCT)])),
        (3, (2, vec![(4, CHILD_KIND_STRUCT)])),
        (4, (1, vec![(3, CHILD_KIND_STRUCT)])),
    ]);
    let mut whites = run_trial_deletion(
        &[(1, CHILD_KIND_STRUCT), (3, CHILD_KIND_STRUCT)],
        |nodes, ptr, kind| {
            if nodes.contains_key(&ptr) {
                return true;
            }
            let Some((count, children)) = graph.get(&ptr) else {
                return false;
            };
            nodes.insert(
                ptr,
                Node {
                    kind,
                    count: *count,
                    color: Color::Fresh,
                    children: children.clone(),
                },
            );
            true
        },
        |_| 0,
    );
    whites.sort_unstable();
    assert_eq!(whites, vec![(1, CHILD_KIND_STRUCT), (2, CHILD_KIND_STRUCT)]);
}

#[test]
fn trial_deletion_keeps_non_drop_cycle_that_owns_a_user_drop_struct() {
    let _guard = locked();
    const FAKE_USER_DROP_DTOR: usize = 0xdead_beef_0001;
    crate::sync::lock(&USER_DROPS).insert(FAKE_USER_DROP_DTOR);

    // Non-Drop cycle 1 <-> 2 where 1 owns helper 3 which owns Drop struct 4;
    // plus an unrelated non-Drop cycle 10 <-> 11 that must still be collected.
    let graph: HashMap<usize, (i64, Vec<(usize, u8)>)> = HashMap::from([
        (1, (1, vec![(2, CHILD_KIND_STRUCT), (3, CHILD_KIND_STRUCT)])),
        (2, (1, vec![(1, CHILD_KIND_STRUCT)])),
        (3, (1, vec![(4, CHILD_KIND_STRUCT)])),
        (4, (1, vec![])),
        (10, (1, vec![(11, CHILD_KIND_STRUCT)])),
        (11, (1, vec![(10, CHILD_KIND_STRUCT)])),
    ]);
    let mut whites = run_trial_deletion(
        &[(1, CHILD_KIND_STRUCT), (10, CHILD_KIND_STRUCT)],
        |nodes, ptr, kind| {
            if nodes.contains_key(&ptr) {
                return true;
            }
            let Some((count, children)) = graph.get(&ptr) else {
                return false;
            };
            nodes.insert(
                ptr,
                Node {
                    kind,
                    count: *count,
                    color: Color::Fresh,
                    children: children.clone(),
                },
            );
            true
        },
        |ptr| if ptr == 4 { FAKE_USER_DROP_DTOR as u64 } else { 0 },
    );
    crate::sync::lock(&USER_DROPS).remove(&FAKE_USER_DROP_DTOR);

    whites.sort_unstable();
    assert_eq!(
        whites,
        vec![(10, CHILD_KIND_STRUCT), (11, CHILD_KIND_STRUCT)],
        "non-Drop ancestors of a Drop struct must stay black so STW never invokes user Drop"
    );
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn a_garbage_two_struct_ring_is_collected_exactly_once() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(50);
    reset_for_tests();

    let (a, b) = unsafe { build_garbage_ring() };
    unsafe { pith_struct_release(a) }; // the last external count: garbage now
    assert_eq!(suspect_count_for_tests(a as usize), 1);
    assert_eq!(
        unsafe { crate::runtime_core::cycle_struct_alive(a) },
        Some(true),
        "the ring still holds it"
    );

    let whites_before = CYCLE_WHITES_FREED.load(Ordering::Relaxed);
    if !collect_eventually() { skip_congested("collection test"); return; }
    assert_eq!(unsafe { pith_struct_weak_upgrade(a) }, 0, "ring member a reclaimed");
    assert_eq!(unsafe { pith_struct_weak_upgrade(b) }, 0, "ring member b reclaimed");
    assert_eq!(
        CYCLE_WHITES_FREED.load(Ordering::Relaxed) - whites_before,
        2,
        "exactly the two ring members were freed"
    );

    // a second pass drains the teardown's fresh suspects and finds
    // nothing more to free: collected exactly once.
    if !collect_eventually() { skip_congested("collection test"); return; }
    assert_eq!(
        CYCLE_WHITES_FREED.load(Ordering::Relaxed) - whites_before,
        2,
        "nothing was freed twice"
    );

    unsafe { pith_struct_weak_release(a) };
    unsafe { pith_struct_weak_release(b) };
    force_enabled_for_tests(false);
    reset_for_tests();
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn a_ring_with_a_live_external_owner_is_kept_until_the_owner_drops() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(50);
    reset_for_tests();

    let (a, b) = unsafe { build_garbage_ring() };
    unsafe { pith_struct_retain(a) }; // the "stack" reference keeping it alive
    unsafe { pith_struct_release(a) }; // buffers a, but a real owner remains

    let whites_before = CYCLE_WHITES_FREED.load(Ordering::Relaxed);
    if !collect_eventually() { skip_congested("collection test"); return; }
    assert_eq!(
        unsafe { crate::runtime_core::cycle_struct_alive(a) },
        Some(true),
        "externally owned: kept"
    );
    assert_eq!(
        unsafe { crate::runtime_core::cycle_struct_alive(b) },
        Some(true),
        "reached only from a: kept"
    );
    assert_eq!(
        CYCLE_WHITES_FREED.load(Ordering::Relaxed) - whites_before,
        0,
        "a reachable ring frees nothing"
    );

    // the owner lets go: the same ring is garbage on the next pass.
    unsafe { pith_struct_release(a) };
    if !collect_eventually() { skip_congested("collection test"); return; }
    assert_eq!(unsafe { pith_struct_weak_upgrade(a) }, 0);
    assert_eq!(unsafe { pith_struct_weak_upgrade(b) }, 0);
    assert_eq!(
        CYCLE_WHITES_FREED.load(Ordering::Relaxed) - whites_before,
        2
    );

    unsafe { pith_struct_weak_release(a) };
    unsafe { pith_struct_weak_release(b) };
    force_enabled_for_tests(false);
    reset_for_tests();
}

/// `ring_dtor` under another address, registered as running a user drop.
unsafe extern "C" fn user_drop_ring_dtor(ptr: i64) {
    unsafe { ring_dtor(ptr) };
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn a_garbage_ring_through_a_user_drop_is_left_alone() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(50);
    reset_for_tests();

    pith_cc_register_tracer(
        user_drop_ring_dtor as *const () as usize as i64,
        ring_tracer as *const () as usize as i64,
    );
    pith_cc_register_user_drop(user_drop_ring_dtor as *const () as usize as i64);
    let (a, b) = unsafe { build_garbage_ring() };
    unsafe { pith_struct_set_dtor(a, user_drop_ring_dtor as *const () as usize as i64) };
    unsafe { pith_struct_release(a) };

    let whites_before = CYCLE_WHITES_FREED.load(Ordering::Relaxed);
    if !collect_eventually() {
        skip_congested("collection test");
        return;
    }
    assert_eq!(
        unsafe { crate::runtime_core::cycle_struct_alive(a) },
        Some(true),
        "a runs a user drop: kept"
    );
    assert_eq!(
        unsafe { crate::runtime_core::cycle_struct_alive(b) },
        Some(true),
        "reached from a: kept"
    );
    assert_eq!(
        CYCLE_WHITES_FREED.load(Ordering::Relaxed) - whites_before,
        0
    );

    // break the ring by hand so the test leaves nothing behind
    unsafe { *(a as *mut i64) = 0 };
    unsafe { pith_struct_release(b) };
    unsafe { pith_struct_weak_release(a) };
    unsafe { pith_struct_weak_release(b) };
    force_enabled_for_tests(false);
    reset_for_tests();
}

#[test]
fn a_weak_upgrade_refuses_a_guarded_white_struct() {
    let s = unsafe { pith_struct_alloc(1) };
    unsafe { guard_white(s as usize, CHILD_KIND_STRUCT) };
    assert_eq!(unsafe { pith_struct_weak_upgrade(s) }, 0, "a white is being torn down");
    // the guard is 2^31 on a u32 count, so a second one wraps it off
    unsafe { guard_white(s as usize, CHILD_KIND_STRUCT) };
    assert_eq!(unsafe { pith_struct_weak_upgrade(s) }, s, "unguarded again");
    unsafe { pith_struct_release(s) };
    unsafe { pith_struct_release(s) };
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn a_dtor_less_box_is_a_leaf_the_collector_never_frees() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(50);
    reset_for_tests();

    // no destructor, so no tracer: the collector sees a leaf with a
    // real external count and must leave it alone.
    let s = unsafe { pith_struct_alloc(2) };
    unsafe { pith_struct_weak_retain(s) };
    unsafe { pith_struct_retain(s) };
    unsafe { pith_struct_release(s) }; // buffered, still owned by us

    let whites_before = CYCLE_WHITES_FREED.load(Ordering::Relaxed);
    if !collect_eventually() { skip_congested("collection test"); return; }
    assert_eq!(
        unsafe { crate::runtime_core::cycle_struct_alive(s) },
        Some(true),
        "leaf with an owner: kept"
    );
    assert_eq!(
        CYCLE_WHITES_FREED.load(Ordering::Relaxed) - whites_before,
        0
    );

    unsafe { pith_struct_weak_release(s) };
    unsafe { pith_struct_release(s) };
    force_enabled_for_tests(false);
    reset_for_tests();
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn a_closure_capture_ring_is_collected_through_the_env_slots() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(50);
    reset_for_tests();

    pith_cc_register_tracer(holder_dtor as *const () as usize as i64, holder_tracer as *const () as usize as i64);
    let s = unsafe { pith_struct_alloc(1) };
    unsafe { pith_struct_set_dtor(s, holder_dtor as *const () as usize as i64) };
    let c = pith_closure_new(0x1000);
    unsafe { pith_struct_retain(s) };
    unsafe { pith_closure_set_env_rc(c, 0, s, 6) }; // closure captures the struct
    unsafe { *(s as *mut i64) = c }; // the struct's field owns the closure back
    unsafe { pith_struct_weak_retain(s) };

    unsafe { pith_struct_release(s) }; // the last external count: garbage now
    assert_eq!(suspect_count_for_tests(s as usize), 1);

    let whites_before = CYCLE_WHITES_FREED.load(Ordering::Relaxed);
    if !collect_eventually() { skip_congested("collection test"); return; }
    assert_eq!(unsafe { pith_struct_weak_upgrade(s) }, 0, "struct half reclaimed");
    assert_eq!(unsafe { pith_closure_get_fn(c) }, 0, "closure half no longer validates");
    assert_eq!(
        CYCLE_WHITES_FREED.load(Ordering::Relaxed) - whites_before,
        2
    );

    unsafe { pith_struct_weak_release(s) };
    force_enabled_for_tests(false);
    reset_for_tests();
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn a_collection_drains_the_graveyard() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(50);
    reset_for_tests();

    let list = unsafe { crate::collections::list::pith_list_new(8, 0) };
    unsafe { pith_list_retain_handle(list.ptr as i64) };
    unsafe { pith_list_release(list) }; // 2 -> 1: buffered
    unsafe { pith_list_release(list) }; // 1 -> 0: dies into the graveyard
    assert_eq!(graveyard_count_for_tests(list.ptr as usize), 1);

    if !collect_eventually() { skip_congested("collection test"); return; }
    assert_eq!(
        graveyard_count_for_tests(list.ptr as usize),
        0,
        "the parked shell was freed"
    );

    force_enabled_for_tests(false);
    reset_for_tests();
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn a_forced_collect_reclaims_a_ring_and_reports_success() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(50);
    reset_for_tests();

    let (a, b) = unsafe { build_garbage_ring() };
    unsafe { pith_struct_release(a) };

    // retried because unrelated test threads may hold running
    // mutator slots for a while, exactly like collect_eventually.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut forced_ok = false;
    while !forced_ok && Instant::now() < deadline {
        forced_ok = pith_cycle_gc_collect_now() == 1;
    }
    assert!(forced_ok, "the forced collection must eventually run");
    assert_eq!(unsafe { pith_struct_weak_upgrade(a) }, 0);
    assert_eq!(unsafe { pith_struct_weak_upgrade(b) }, 0);

    unsafe { pith_struct_weak_release(a) };
    unsafe { pith_struct_weak_release(b) };
    force_enabled_for_tests(false);
    reset_for_tests();
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn a_forced_collect_reports_failure_when_a_spinner_prevents_the_stop() {
    let _guard = locked();
    force_enabled_for_tests(true);
    set_stop_timeout_for_tests(30);

    let done = Arc::new(AtomicBool::new(false));
    let spinner_done = Arc::clone(&done);
    let (_slot, handle) = spawn_registered(move || {
        while !spinner_done.load(Ordering::Relaxed) {
            std::hint::spin_loop();
        }
    });

    assert_eq!(
        pith_cycle_gc_collect_now(),
        0,
        "a mutator that never gates times the forced stop out"
    );

    done.store(true, Ordering::Relaxed);
    handle.join().expect("spinning thread panicked");
    force_enabled_for_tests(false);
}

#[test]
#[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
fn a_forced_collect_with_the_flag_off_reports_failure_immediately() {
    let _guard = locked();
    force_enabled_for_tests(false);
    assert_eq!(pith_cycle_gc_collect_now(), 0);
}
