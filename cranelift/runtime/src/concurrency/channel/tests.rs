use super::handle::*;
use super::park::*;
use super::ring::*;
use super::*;

// a received optional as a pointer to its two slots: a Some is its own
// shell, and a none (the null pointer) reads as a shell of zeros.
fn shell(opt: i64) -> *const i64 {
    static NONE: [i64; 2] = [0, 0];
    if opt == 0 {
        NONE.as_ptr()
    } else {
        opt as *const i64
    }
}

// the creator drops its only count while three receivers are parked on
// the channel. the channel must survive them: a parked operation holds a
// count of its own, so the release frees nothing yet. a fourth holder,
// retained before the release, then feeds them; the channel is freed
// exactly once, when that last holder releases.
#[test]
fn parked_receivers_then_last_release_elsewhere() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let ch = pith_channel_new(4, 0);
    unsafe { pith_channel_retain(ch) }; // the sender's own count
    let receivers: Vec<_> = (0..3)
        // recv returns an optional: slot 0 of shell() is the presence flag, slot 1 the value
        .map(|_| {
            std::thread::spawn(move || unsafe {
                *(shell(pith_channel_recv(ch)).add(1))
            })
        })
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(60));
    unsafe { pith_channel_release(ch) }; // the creator is gone; receivers still parked
    for v in 1..=3 {
        assert_eq!(unsafe { pith_channel_send(ch, v) }, 1, "still in service");
    }
    let mut got: Vec<i64> = receivers.into_iter().map(|t| t.join().unwrap()).collect();
    got.sort();
    assert_eq!(got, vec![1, 2, 3]);
    assert_eq!(unsafe { pith_channel_send(ch, 4) }, 1, "one holder remains");
    let before = frees();
    unsafe { pith_channel_release(ch) };
    assert_eq!(frees(), before + 1, "the last owner frees exactly once");
}

// a channel whose payloads are channels: releasing the outer one to zero
// drains its ring by the payload tag, which releases the inner channels'
// only counts through this same path: four frees for one release.
#[test]
fn zero_drains_ring_by_tag() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let outer = pith_channel_new(8, 8);
    let inners: Vec<i64> = (0..3).map(|_| pith_channel_new(1, 0)).collect();
    for &inner in &inners {
        assert_eq!(unsafe { pith_channel_send(outer, inner) }, 1);
    }
    assert_eq!(unsafe { pith_channel_len(outer) }, 3);
    for &inner in &inners {
        assert_eq!(
            unsafe { pith_channel_send(inner, 1) },
            1,
            "alive while queued"
        );
        assert_eq!(
            unsafe { *shell(pith_channel_try_recv(inner)) },
            1,
            "present"
        );
    }
    let before = frees();
    unsafe { pith_channel_release(outer) };
    assert_eq!(frees(), before + 4, "the outer and its three queued inners");
}

#[test]
fn extra_owners_delay_the_free_and_garbage_is_ignored() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let ch = pith_channel_new(1, 0);
    let before = frees();
    unsafe {
        pith_channel_retain(ch);
        pith_channel_release(ch);
        assert_eq!(frees(), before, "one owner remains");
        pith_channel_release(ch);
        assert_eq!(frees(), before + 1);
        pith_channel_retain(12345); // garbage: no-op
        pith_channel_release(12345);
    }
}

// an ALIGNED but bogus handle: the magic check is what has to reject this,
// since alignment alone lets the pointer be dereferenced. the older
// registry lookup rejected every unknown address; the tag scheme rejects
// anything whose first word is not CHANNEL_MAGIC, which is what this pins.
// (a pointer into unmapped memory is out of contract either way: the same
// practical guard strings, closures, and structs already use.)
#[test]
fn aligned_but_untagged_handle_is_rejected() {
    let scratch: Box<[u64; 4]> = Box::new([0; 4]);
    let handle = Box::into_raw(scratch) as i64;
    assert_eq!(handle % 8, 0, "test needs an aligned allocation");
    unsafe {
        assert_eq!(pith_channel_send(handle, 7), 0);
        assert_eq!(pith_channel_try_send(handle, 7), 0);
        assert_eq!(pith_channel_close(handle), 0);
        assert_eq!(pith_channel_len(handle), 0);
        assert_eq!(pith_channel_cap(handle), 0);
        assert_eq!(pith_channel_is_closed(handle), 1);
        let recv = shell(pith_channel_try_recv(handle));
        assert_eq!(*recv, 0);
        drop(Box::from_raw(handle as *mut [u64; 4]));
    }
}

// the retained-bytes accounting has to track the allocation it describes:
// a fixed part that every channel pays and a ring part that scales with the
// rounded-up capacity. an unbuffered channel allocates no ring at all.
#[test]
fn channel_bytes_tracks_the_ring() {
    let fixed = channel_bytes(0);
    assert!(fixed > 0);
    let slot = std::mem::size_of::<Slot>();
    // capacity 1 still rounds to two slots (see `Ring::new`).
    assert_eq!(channel_bytes(1), fixed + 2 * slot);
    assert_eq!(channel_bytes(256), fixed + 256 * slot);
    // a non-power-of-two capacity pays for the slots it rounds up to.
    assert_eq!(channel_bytes(200), fixed + 256 * slot);
}

// a buffered channel's capacity is clamped rather than allocated blindly, so
// an absurd request degrades to a smaller buffer instead of aborting the
// process on a 16 GiB allocation.
#[test]
fn absurd_capacity_is_clamped() {
    unsafe {
        let ch = pith_channel_new(i64::MAX, 0);
        assert_eq!(pith_channel_cap(ch), MAX_CHANNEL_CAPACITY);
        assert_eq!(pith_channel_try_send(ch, 1), 1);
        let t = shell(pith_channel_try_recv(ch));
        assert_eq!((*t, *t.add(1)), (1, 1));
    }
}

// an unbuffered send whose value a receiver took must report success even
// if the channel closes before the sender wakes up. close() clears the
// pending slot the same way a receiver taking it does, so a sender that
// looked only at `closed` reported failure for a value that had already
// been delivered, and a caller that retries on failure sends it twice.
#[test]
fn unbuffered_send_reports_delivery_not_closure() {
    unsafe {
        let ch = pith_channel_new(0, 0);
        let sender = std::thread::spawn(move || { pith_channel_send(ch, 42) });

        // take the value, then close while the sender is still parked
        let t = shell(pith_channel_recv(ch));
        assert_eq!((*t, *t.add(1)), (1, 42));
        pith_channel_close(ch);

        assert_eq!(
            sender.join().unwrap(),
            1,
            "delivered value reported as failed"
        );
    }
}

// a receiver that polls with try_recv, as a select does, meets a sender
// that parked first. the sender used to offer its value only to a parked
// receiver, so the poll never saw it.
#[test]
fn unbuffered_try_recv_meets_a_parked_sender() {
    unsafe {
        let ch = pith_channel_new(0, 0);
        let sender = std::thread::spawn(move || pith_channel_send(ch, 42));
        std::thread::sleep(std::time::Duration::from_millis(30));
        let mut got = None;
        for _ in 0..2000 {
            let opt = pith_channel_try_recv(ch);
            if opt != 0 {
                let t = shell(opt);
                got = Some(*t.add(1));
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(got, Some(42));
        assert_eq!(sender.join().unwrap(), 1);
        pith_channel_close(ch);
    }
}

// the other side of the same race: a sender parked with nobody to take its
// value gets a failure when the channel closes, so the caller knows the
// value never landed.
#[test]
fn unbuffered_send_fails_when_closed_undelivered() {
    unsafe {
        let ch = pith_channel_new(0, 0);
        let sender = std::thread::spawn(move || { pith_channel_send(ch, 7) });
        std::thread::sleep(std::time::Duration::from_millis(50));
        pith_channel_close(ch);
        assert_eq!(sender.join().unwrap(), 0);
    }
}

static SERIAL: Mutex<()> = Mutex::new(());
fn frees() -> usize {
    TEST_FREES.load(Ordering::Relaxed)
}

// receivers parked on an empty ring when close() wakes them: every one
// sees the close, the channel then answers like any closed, drained one
// while its owner still holds it, and the owner's release frees it once.
#[test]
fn close_with_parked_receivers_then_release_frees() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let ch = pith_channel_new(4, 0);
        let receivers: Vec<_> = (0..3)
            .map(|_| {
                std::thread::spawn(move || {
                    let t = shell(pith_channel_recv(ch));
                    *t
                })
            })
            .collect();
        std::thread::sleep(std::time::Duration::from_millis(50));
        pith_channel_close(ch);
        for h in receivers {
            assert_eq!(h.join().unwrap(), 0, "parked recv must see the close");
        }
        assert_eq!(pith_channel_is_closed(ch), 1);
        assert_eq!(pith_channel_len(ch), 0);
        assert_eq!(pith_channel_send(ch, 1), 0);
        assert_eq!(pith_channel_cap(ch), 4);
        let t = shell(pith_channel_recv(ch));
        assert_eq!(*t, 0);
        let before = frees();
        pith_channel_release(ch);
        assert_eq!(
            frees(),
            before + 1,
            "the owner's release frees the closed channel"
        );
    }
}

// senders racing a concurrent close-and-drain, many rounds, each thread
// holding its own count and releasing it at its end so the last one out
// frees the channel under contention: every interleaving must end with the
// channel freed exactly once and nobody faulting.
#[test]
fn a_close_race_never_hands_back_more_than_was_sent() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // what a close race guarantees, and what it does not.
    //
    // it does NOT guarantee that every accepted send is drainable. a
    // sender can pass its closed check and enqueue after the drain loop
    // has already seen the ring empty and stopped, and that value is then
    // unobservable. this is a property of close, not of reclamation: the
    // same interleaving strands the value in the ring without it. an
    // earlier version of this test asserted the equality and was flaky.
    //
    // what must hold is the other direction. a drain may never produce
    // more than was accepted, because that would mean a value delivered
    // twice or a slot read after it was freed.
    let before = frees();
    for _ in 0..1500 {
        unsafe {
            let ch = pith_channel_new(8, 0);
            pith_channel_retain(ch); // the sender's count
            pith_channel_retain(ch); // the closer's count
            let sender = std::thread::spawn(move || {
                let mut accepted = 0;
                for v in 0..8 {
                    if pith_channel_send(ch, v + 1) != 0 {
                        accepted += 1;
                    }
                }
                pith_channel_release(ch);
                accepted
            });
            let closer = std::thread::spawn(move || {
                pith_channel_close(ch);
                let mut drained = 0;
                loop {
                    let t = shell(pith_channel_recv(ch));
                    if *t == 0 {
                        break;
                    }
                    drained += 1;
                }
                pith_channel_release(ch);
                drained
            });
            let accepted = sender.join().unwrap();
            let drained = closer.join().unwrap();
            assert!(
                drained <= accepted,
                "a drain produced {drained} values from {accepted} accepted sends"
            );
            pith_channel_release(ch); // the creator's count, last or not
        }
    }
    assert_eq!(
        frees(),
        before + 1500,
        "every round freed its channel exactly once"
    );
}

#[test]
fn close_drain_racing_senders_never_faults() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let before = frees();
    for _ in 0..1500 {
        unsafe {
            let ch = pith_channel_new(2, 0);
            pith_channel_retain(ch);
            let sender = std::thread::spawn(move || {
                for v in 0..8 {
                    if pith_channel_send(ch, v) == 0 {
                        break;
                    }
                }
                pith_channel_release(ch);
            });
            let closer = std::thread::spawn(move || {
                let _ = pith_channel_try_recv(ch);
                pith_channel_close(ch);
                loop {
                    let t = shell(pith_channel_recv(ch));
                    if *t == 0 {
                        break;
                    }
                }
            });
            sender.join().unwrap();
            closer.join().unwrap();
            assert_eq!(pith_channel_is_closed(ch), 1);
            assert_eq!(pith_channel_send(ch, 99), 0);
            pith_channel_release(ch);
        }
    }
    assert_eq!(frees(), before + 1500);
}

// a consumer draining after close must still get every buffered value:
// close is not a free, and nothing is freed until the last owner is gone.
#[test]
fn close_then_drain_keeps_values() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let ch = pith_channel_new(8, 0);
        for v in 1..=5 {
            assert_eq!(pith_channel_send(ch, v), 1);
        }
        pith_channel_close(ch);
        for v in 1..=5 {
            let t = shell(pith_channel_recv(ch));
            assert_eq!((*t, *t.add(1)), (1, v));
        }
        let t = shell(pith_channel_recv(ch));
        assert_eq!(*t, 0);
        let before = frees();
        pith_channel_release(ch);
        assert_eq!(frees(), before + 1);
    }
}

#[test]
fn invalid_channel_handles_return_safe_defaults() {
    unsafe {
        assert_eq!(pith_channel_send(12345, 7), 0);
        assert_eq!(pith_channel_try_send(12345, 7), 0);
        assert_eq!(pith_channel_close(12345), 0);
        assert_eq!(pith_channel_len(12345), 0);
        assert_eq!(pith_channel_cap(12345), 0);
        assert_eq!(pith_channel_is_closed(12345), 1);

        let recv = shell(pith_channel_try_recv(12345));
        assert!(!recv.is_null());
        assert_eq!(*recv, 0);
    }
}

// the barrier shape: several senders park on one unbuffered (rendezvous)
// channel at the same time, and a single receiver drains them. a recv that
// takes a value must wake the *specific* depositing sender; a role-gated
// notify_one that woke the wrong same-role sender would leave the depositor
// parked forever, so a regression here hangs rather than fails. the counted
// total proves every send was matched by exactly one recv.
#[test]
fn unbuffered_rendezvous_many_senders_one_receiver() {
    unsafe {
        let ch = pith_channel_new(0, 0);
        let n = 8i64;
        let senders: Vec<_> = (1..=n)
            .map(|v| std::thread::spawn(move || assert_eq!(pith_channel_send(ch, v), 1)))
            .collect();

        let mut total = 0i64;
        for _ in 0..n {
            let t = shell(pith_channel_recv(ch));
            assert_eq!(*t, 1, "expected a value, not an empty optional");
            total += *t.add(1);
        }
        for h in senders {
            h.join().unwrap();
        }
        assert_eq!(total, n * (n + 1) / 2);
    }
}

// capacity 1 is the vyukov edge: a single-slot ring cannot distinguish full
// from free (the sequence after an enqueue equals the next enqueue
// position), so the ring holds two slots with the explicit capacity check
// enforcing the bound of one. the second try_send must refuse.
#[test]
fn buffered_capacity_one_refuses_second_send() {
    unsafe {
        let ch = pith_channel_new(1, 0);
        assert_eq!(pith_channel_try_send(ch, 4), 1);
        assert_eq!(pith_channel_try_send(ch, 5), 0, "capacity 1 is full");
        assert_eq!(pith_channel_len(ch), 1);
        let t = shell(pith_channel_try_recv(ch));
        assert_eq!((*t, *t.add(1)), (1, 4));
        assert_eq!(pith_channel_try_send(ch, 6), 1, "room again after recv");
    }
}

// a non-power-of-two capacity leaves spare ring slots, so fullness is
// enforced by the explicit capacity check rather than the slot sequences:
// try_send must refuse the (cap+1)th value, len must report the exact
// capacity, and draining must return every value in order.
#[test]
fn buffered_ring_honors_non_power_of_two_capacity() {
    unsafe {
        let ch = pith_channel_new(3, 0);
        assert_eq!(pith_channel_cap(ch), 3);
        for v in 1..=3 {
            assert_eq!(pith_channel_try_send(ch, v), 1);
        }
        assert_eq!(pith_channel_try_send(ch, 99), 0, "over capacity");
        assert_eq!(pith_channel_len(ch), 3);
        for v in 1..=3 {
            let t = shell(pith_channel_try_recv(ch));
            assert_eq!(*t, 1);
            assert_eq!(*t.add(1), v);
        }
        let t = shell(pith_channel_try_recv(ch));
        assert_eq!(*t, 0, "drained");
    }
}

// values enqueued before close must still drain; recvs after the drain see
// the empty optional, and sends after close are refused.
#[test]
fn buffered_close_drains_then_reports_closed() {
    unsafe {
        let ch = pith_channel_new(4, 0);
        assert_eq!(pith_channel_send(ch, 41), 1);
        assert_eq!(pith_channel_send(ch, 42), 1);
        pith_channel_close(ch);
        assert_eq!(pith_channel_send(ch, 43), 0, "send after close");
        let a = shell(pith_channel_recv(ch));
        assert_eq!((*a, *a.add(1)), (1, 41));
        let b = shell(pith_channel_recv(ch));
        assert_eq!((*b, *b.add(1)), (1, 42));
        let c = shell(pith_channel_recv(ch));
        assert_eq!(*c, 0, "closed and drained");
        assert_eq!(pith_channel_is_closed(ch), 1);
    }
}

// the fanout shape: several producers and consumers over one bounded channel,
// exactly what `bench/chan_fanout` measures. guards the buffered notify_one
// hand-off: a producer that enqueues a value wakes one consumer, and close
// wakes every blocked consumer so each observes the drained-and-closed state.
#[test]
fn buffered_fanout_many_producers_consumers() {
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;
    unsafe {
        let ch = pith_channel_new(4, 0);
        let producers = 4i64;
        let per = 500i64;
        let total_msgs = producers * per;
        let sum = Arc::new(AtomicI64::new(0));
        let count = Arc::new(AtomicI64::new(0));

        let consumers: Vec<_> = (0..4)
            .map(|_| {
                let sum = Arc::clone(&sum);
                let count = Arc::clone(&count);
                std::thread::spawn(move || loop {
                    let t = shell(pith_channel_recv(ch));
                    if *t == 0 {
                        break; // empty optional: channel closed and drained
                    }
                    sum.fetch_add(*t.add(1), Ordering::Relaxed);
                    count.fetch_add(1, Ordering::Relaxed);
                })
            })
            .collect();

        let prod: Vec<_> = (0..producers)
            .map(|p| {
                std::thread::spawn(move || {
                    for i in 0..per {
                        assert_eq!(pith_channel_send(ch, p * per + i), 1);
                    }
                })
            })
            .collect();

        for h in prod {
            h.join().unwrap();
        }
        pith_channel_close(ch);
        for h in consumers {
            h.join().unwrap();
        }

        assert_eq!(count.load(Ordering::Relaxed), total_msgs);
        // every id in 0..total_msgs was seen exactly once
        assert_eq!(
            sum.load(Ordering::Relaxed),
            total_msgs * (total_msgs - 1) / 2
        );
    }
}

#[test]
fn ring_close_rejects_subsequent_enqueues_atomically() {
    let ring = Ring::new(4);
    assert!(matches!(ring.try_enqueue(10, 4), EnqueueStatus::Enqueued));
    assert!(matches!(ring.try_enqueue(20, 4), EnqueueStatus::Enqueued));
    ring.close();
    assert!(matches!(ring.try_enqueue(30, 4), EnqueueStatus::Closed));
    assert_eq!(ring.len(), 2);
    assert_eq!(ring.try_dequeue(), Some(10));
    assert_eq!(ring.try_dequeue(), Some(20));
    assert_eq!(ring.try_dequeue(), None);
    assert_eq!(ring.len(), 0);
}

#[test]
fn unbuffered_detached_try_send_survives_close_and_drains_cleanly() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let ch = pith_channel_new(0, 0);
        // Simulate a waiting receiver so try_send succeeds and detaches ownership.
        with_channel(ch, |t| {
            lock_state(&t.inner.state).receiver_waiting = 1;
        });
        assert_eq!(pith_channel_try_send(ch, 42), 1);
        assert_eq!(pith_channel_close(ch), 1);
        // The detached value is still drainable by a receiver after close.
        let got = channel_try_recv(ch);
        assert_eq!(got, Some(42));
        // Subsequent recv observes closed & drained.
        assert_eq!(channel_recv(ch), None);
        pith_channel_release(ch);
    }
}

#[test]
fn unbuffered_multi_sender_rendezvous_does_not_deadlock() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let ch = pith_channel_new(0, 0);
        let senders = 4i64;
        let per = 50i64;
        let handles: Vec<_> = (0..senders)
            .map(|s| {
                std::thread::spawn(move || {
                    for i in 0..per {
                        assert_eq!(pith_channel_send(ch, s * per + i), 1);
                    }
                })
            })
            .collect();
        let mut total = 0i64;
        for _ in 0..(senders * per) {
            total += channel_recv(ch).expect("unbuffered value");
        }
        for h in handles {
            h.join().unwrap();
        }
        let n = senders * per;
        assert_eq!(total, n * (n - 1) / 2);
        pith_channel_release(ch);
    }
}
