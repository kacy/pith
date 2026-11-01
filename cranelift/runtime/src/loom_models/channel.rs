//! a buffered channel's park/wake: the wake gate, the green waiter list and
//! its stale-entry removal, and the direct handoff to a parked receiver.
//!
//! a mirror of `concurrency::channel`: `buffered_send`, `buffered_recv`,
//! `wake_parked` and the green arm of `block_on_channel`, step for step, with
//! their orderings. the lock-free ring is reduced to one atomic slot (a
//! capacity-one ring), which keeps its release/acquire publication and drops
//! the position bookkeeping, and the channel lock guards just the receiver
//! list. the receiver is a green task (`green_word::Task`).
//!
//! two properties: a send never strands a parked receiver (a lost wake leaves
//! the receiver blocked and loom reports the deadlock), and a receive that
//! returns has left no entry for itself on the list, even when a wake from
//! somewhere else got it out of its park. a stale entry is what let a later
//! send on this channel claim the task while it was parked on another one and
//! hand it this channel's value.

use super::green_word::Task;
use loom::sync::atomic::{fence, AtomicI64, AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;

/// the one receiver's task id.
const RECEIVER: usize = 0;

struct Channel {
    /// the ring: 0 when empty, else the queued value.
    ring: AtomicI64,
    parked_receivers: AtomicUsize,
    /// `ChannelState::green_receivers`, under the channel lock.
    green_receivers: Mutex<Vec<usize>>,
    /// the receiver's `SyncBlock::handoff` cell.
    handoff: Mutex<Option<i64>>,
    receiver: Task,
    /// whether a resumed receiver drops its own leftover entry (#1270).
    remove_stale: bool,
}

impl Channel {
    fn new(remove_stale: bool) -> Channel {
        Channel {
            ring: AtomicI64::new(0),
            parked_receivers: AtomicUsize::new(0),
            green_receivers: Mutex::new(Vec::new()),
            handoff: Mutex::new(None),
            receiver: Task::running(),
            remove_stale,
        }
    }

    fn try_enqueue(&self, value: i64) -> bool {
        self.ring
            .compare_exchange(0, value, Ordering::Release, Ordering::Relaxed)
            .is_ok()
    }

    fn try_dequeue(&self) -> Option<i64> {
        match self.ring.swap(0, Ordering::AcqRel) {
            0 => None,
            value => Some(value),
        }
    }

    /// `buffered_send` on a ring with room, then `wake_parked(Receiver)`.
    fn send(&self, value: i64) {
        assert!(self.try_enqueue(value), "the model sends into an empty ring");
        fence(Ordering::SeqCst);
        if self.parked_receivers.load(Ordering::Relaxed) == 0 {
            return;
        }
        let mut list = self.green_receivers.lock().unwrap();
        if let Some(id) = list.pop() {
            assert_eq!(id, RECEIVER);
            self.receiver.wake_with(&self.handoff, || self.try_dequeue());
            return;
        }
        // `wake_green`: drain the list, waking each entry.
        for id in std::mem::take(&mut *list) {
            assert_eq!(id, RECEIVER);
            self.receiver.wake();
        }
    }

    /// `buffered_recv` from a green task, which never spins.
    fn recv(&self) -> i64 {
        loop {
            if let Some(value) = self.try_dequeue() {
                return value;
            }
            let list = self.green_receivers.lock().unwrap();
            self.parked_receivers.fetch_add(1, Ordering::SeqCst);
            fence(Ordering::SeqCst);
            if let Some(value) = self.try_dequeue() {
                self.parked_receivers.fetch_sub(1, Ordering::SeqCst);
                drop(list);
                return value;
            }
            drop(self.block(list));
            self.parked_receivers.fetch_sub(1, Ordering::SeqCst);
            if let Some(value) = self.handoff.lock().unwrap().take() {
                return value;
            }
        }
    }

    /// the green arm of `block_on_channel`.
    fn block<'a>(
        &'a self,
        mut list: loom::sync::MutexGuard<'a, Vec<usize>>,
    ) -> loom::sync::MutexGuard<'a, Vec<usize>> {
        list.push(RECEIVER);
        drop(list);
        self.receiver.park();
        let mut list = self.green_receivers.lock().unwrap();
        if self.remove_stale {
            if let Some(pos) = list.iter().rposition(|&waiter| waiter == RECEIVER) {
                list.remove(pos);
            }
        }
        list
    }

    fn receiver_listed(&self) -> bool {
        self.green_receivers.lock().unwrap().contains(&RECEIVER)
    }
}

#[test]
fn loom_channel_send_wakes_a_parking_receiver() {
    loom::model(|| {
        let ch = Arc::new(Channel::new(true));
        let sender = Arc::clone(&ch);
        let send = thread::spawn(move || sender.send(5));
        assert_eq!(ch.recv(), 5);
        send.join().unwrap();
        assert!(!ch.receiver_listed(), "the receive left its entry on the list");
    });
}

/// the receive is also woken once from outside the channel, the way the
/// blocking pool's stray wake reached it before #1270.
fn stray_wake_model(remove_stale: bool) {
    super::model_bounded(move || {
        let ch = Arc::new(Channel::new(remove_stale));
        let (sender, stray) = (Arc::clone(&ch), Arc::clone(&ch));
        let send = thread::spawn(move || sender.send(5));
        let wake = thread::spawn(move || stray.receiver.wake());
        assert_eq!(ch.recv(), 5);
        send.join().unwrap();
        wake.join().unwrap();
        assert!(!ch.receiver_listed(), "the receive left its entry on the list");
    });
}

#[test]
fn loom_channel_stray_wake_leaves_no_stale_entry() {
    stray_wake_model(true);
}

#[test]
#[should_panic(expected = "the receive left its entry on the list")]
fn loom_channel_pre_fix_stray_wake_leaves_a_stale_entry() {
    stray_wake_model(false);
}
