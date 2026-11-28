//! Synchronous Bacon-Rajan trial-deletion pass run inside a stopped world.

use super::stw::set_on_collector;
use super::suspects::{lock_graveyard, lock_suspects};
use super::tracers::{
    install_visit_hook, invoke_tracer, tracer_for_dtor, traced_children_push,
    traced_children_take, USER_DROPS,
};
use super::{CYCLE_KIND_CLOSURE, CYCLE_KIND_LIST, CYCLE_KIND_MAP, CYCLE_KIND_STRUCT};
use crate::child_kind::{
    CLOSURE as CHILD_KIND_CLOSURE, LIST as CHILD_KIND_LIST, MAP as CHILD_KIND_MAP,
    SET as CHILD_KIND_SET, STRUCT as CHILD_KIND_STRUCT,
};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

// --- the collection pass ----------------------------------------------------
//
// trial deletion (Bacon-Rajan), synchronous, inside the stopped world. the
// suspects are the only places a garbage cycle can root, so the pass drains
// them, walks the ownership graph they reach, subtracts the edges internal to
// that graph from each member's real count (MarkGray), restores the counts of
// everything still owned from outside (Scan/ScanBlack), and frees what is
// left (the white set): the members of cycles no live reference reaches.
//
// child kinds use the closure env tag codes, the one numbering the runtime,
// the tracers, and `pith_cc_visit` already share: 2=list 3=map 4=set 6=struct
// (and tuple boxes) 7=closure. strings and bytes never appear in the graph:
// they hold no edges, so they cannot sit in a cycle, and the destruction of
// their owner releases them the ordinary way. sets appear as nodes (they have
// a count of their own, so a set owned only by garbage is garbage) but are
// never enumerated into: ints and copied byte strings are all they hold.


/// Collections that ran to completion (world stopped, whites freed).
pub static CYCLE_COLLECTIONS: AtomicU64 = AtomicU64::new(0);
/// Live suspects examined as candidate roots across all collections.
pub static CYCLE_ROOTS_EXAMINED: AtomicU64 = AtomicU64::new(0);
/// Cycle members reclaimed by trial deletion.
pub static CYCLE_WHITES_FREED: AtomicU64 = AtomicU64::new(0);
/// Graveyard shells reclaimed by the collector.
pub static CYCLE_GRAVEYARD_FREED: AtomicU64 = AtomicU64::new(0);

// how many buffered suspects trigger a collection. resolved once from
// PITH_CYCLE_GC_THRESHOLD; 0 in the cell means not yet resolved.
pub(super) static COLLECT_THRESHOLD: AtomicUsize = AtomicUsize::new(0);
const COLLECT_THRESHOLD_DEFAULT: usize = 4096;

pub(super) fn collect_threshold() -> usize {
    let cached = COLLECT_THRESHOLD.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let threshold = std::env::var("PITH_CYCLE_GC_THRESHOLD")
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(COLLECT_THRESHOLD_DEFAULT);
    COLLECT_THRESHOLD.store(threshold, Ordering::Relaxed);
    threshold
}
/// The hook installed for the span of a collection: buffer each reported
/// child for the tracer caller to drain. Installing it process-wide is safe
/// because the world is stopped, so only the collector runs tracers.
pub(super) extern "C" fn collector_visit(child: i64, kind: i64) {
    traced_children_push(child, kind);
}

/// The object's real, current strong count, which the trial counts seed
/// from. `None` when the pointer does not validate as the claimed kind (a
/// corpse, or a miscarried edge), in which case it never joins the graph.
unsafe fn real_strong_count(ptr: usize, kind: u8) -> Option<i64> {
    match kind {
        CHILD_KIND_STRUCT => {
            // SAFETY: Caller holds STW and `ptr` came from a root or traced struct edge.
            unsafe { crate::runtime_core::cycle_struct_strong_count(ptr as i64) }.map(i64::from)
        }
        CHILD_KIND_CLOSURE => {
            // SAFETY: Caller holds STW and `ptr` came from a root or traced closure edge.
            unsafe { crate::runtime_core::cycle_closure_strong_count(ptr as i64) }
        }
        CHILD_KIND_LIST => {
            // SAFETY: Caller holds STW and `ptr` came from a root or traced list edge.
            unsafe { crate::collections::list::cycle_list_strong_count(ptr as i64) }.map(i64::from)
        }
        CHILD_KIND_MAP => {
            // SAFETY: Caller holds STW and `ptr` came from a root or traced map edge.
            unsafe { crate::collections::map::cycle_map_strong_count(ptr as i64) }.map(i64::from)
        }
        CHILD_KIND_SET => {
            // SAFETY: Caller holds STW and `ptr` came from a traced set edge.
            unsafe { crate::collections::set::cycle_set_strong_count(ptr as i64) }.map(i64::from)
        }
        _ => None,
    }
}

#[inline(always)]
fn is_cycle_child_kind(kind: u8) -> bool {
    matches!(
        kind,
        CHILD_KIND_LIST
            | CHILD_KIND_MAP
            | CHILD_KIND_SET
            | CHILD_KIND_STRUCT
            | CHILD_KIND_CLOSURE
    )
}

/// Every rc edge out of the object, as (child pointer, child kind). One
/// function for every kind:
///
/// - a struct-family box reaches its children through the tracer keyed by
///   its destructor pointer; no destructor, or no tracer for it (only
///   string/bytes/weak fields), means a leaf.
/// - a closure walks its tagged env slots; weak captures (tag 8) hold no
///   strong count and are skipped with the string/bytes leaves.
/// - lists and maps enumerate the values they own counts on.
/// - sets are nodes with no out-edges.
pub(super) unsafe fn children_of(ptr: usize, kind: u8) -> Vec<(usize, u8)> {
    let mut children: Vec<(usize, u8)> = Vec::new();
    let mut push = |child: i64, child_kind: u8| {
        if child != 0 && is_cycle_child_kind(child_kind) {
            children.push((child as usize, child_kind));
        }
    };
    match kind {
        CHILD_KIND_STRUCT => {
            // SAFETY: Caller holds STW and `ptr` validated as a live struct.
            let dtor = unsafe { crate::runtime_core::cycle_struct_dtor_slot(ptr as i64) };
            if dtor != 0 {
                if let Some(tracer) = tracer_for_dtor(dtor as usize) {
                    // SAFETY: `tracer` is registered for `ptr`'s destructor and `ptr` is live.
                    unsafe { invoke_tracer(tracer, ptr as i64) };
                    for (child, child_kind) in traced_children_take() {
                        push(child, child_kind as u8);
                    }
                }
            }
        }
        CHILD_KIND_CLOSURE => {
            // SAFETY: Caller holds STW and `ptr` validated as a live closure.
            unsafe { crate::runtime_core::cycle_closure_children(ptr as i64, &mut push) };
        }
        CHILD_KIND_LIST => {
            // SAFETY: Caller holds STW and `ptr` validated as a live list.
            unsafe { crate::collections::list::cycle_list_children(ptr as i64, &mut push) };
        }
        CHILD_KIND_MAP => {
            // SAFETY: Caller holds STW and `ptr` validated as a live map.
            unsafe { crate::collections::map::cycle_map_children(ptr as i64, &mut push) };
        }
        _ => {}
    }
    children
}

/// mark-gray colors. every node starts `Fresh` when seeded; MarkGray turns
/// the candidate graph gray while subtracting internal edges, Scan restores
/// (black) what outside references still hold, and what stays with no
/// external count comes out white: the garbage.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Color {
    Fresh,
    Gray,
    Black,
    White,
}

/// one graph node, keyed by pointer in the pass-local map. the children are
/// enumerated once at seeding (the world is stopped, so the edges cannot
/// change) and reused by every later phase.
pub(super) struct Node {
    pub(super) kind: u8,
    pub(super) count: i64,
    pub(super) color: Color,
    pub(super) children: Vec<(usize, u8)>,
}

/// Seed a node with its real count and its edges. `false` when the pointer
/// does not validate, which keeps a miscarried edge out of the graph. its
/// trial count is then simply never decremented, the leak direction.
pub(super) unsafe fn seed_node(nodes: &mut HashMap<usize, Node>, ptr: usize, kind: u8) -> bool {
    if nodes.contains_key(&ptr) {
        return true;
    }
    // SAFETY: Caller holds STW and `ptr` came from a validated root or traced edge.
    let Some(count) = (unsafe { real_strong_count(ptr, kind) }) else {
        return false;
    };
    // SAFETY: `ptr` just validated via `real_strong_count` under STW.
    let children = unsafe { children_of(ptr, kind) };
    nodes.insert(
        ptr,
        Node {
            kind,
            count,
            color: Color::Fresh,
            children,
        },
    );
    true
}

/// MarkGray: walk from a root, graying each node once and subtracting one
/// from the trial count of every edge target. after every root is walked,
/// each node's trial count is its real count minus the edges internal to
/// the candidate graph.
fn mark_gray(
    nodes: &mut HashMap<usize, Node>,
    root: usize,
    seed: &mut impl FnMut(&mut HashMap<usize, Node>, usize, u8) -> bool,
) {
    let mut stack = vec![root];
    while let Some(ptr) = stack.pop() {
        let children = match nodes.get_mut(&ptr) {
            Some(node) if node.color != Color::Gray => {
                node.color = Color::Gray;
                node.children.clone()
            }
            _ => continue,
        };
        for (child, child_kind) in children {
            if !seed(nodes, child, child_kind) {
                continue;
            }
            // seeded just above, so the lookup can only hit.
            let Some(child_node) = nodes.get_mut(&child) else {
                continue;
            };
            child_node.count -= 1;
            if child_node.color != Color::Gray {
                stack.push(child);
            }
        }
    }
}

/// Scan: a gray node with a positive trial count is owned from outside the
/// candidate graph: re-blacken it and everything it reaches, restoring the
/// counts MarkGray subtracted. a gray node at zero or below goes white and
/// its children are scanned in turn.
fn scan(nodes: &mut HashMap<usize, Node>, root: usize) {
    let mut stack = vec![root];
    while let Some(ptr) = stack.pop() {
        let Some(node) = nodes.get_mut(&ptr) else {
            continue;
        };
        if node.color != Color::Gray {
            continue;
        }
        if node.count > 0 {
            scan_black(nodes, ptr);
            continue;
        }
        node.color = Color::White;
        let children = node.children.clone();
        for (child, _) in children {
            stack.push(child);
        }
    }
}

/// ScanBlack: blacken everything reachable from a node with external owners,
/// re-incrementing each edge target's trial count exactly once per edge.
fn scan_black(nodes: &mut HashMap<usize, Node>, root: usize) {
    let mut stack = vec![root];
    while let Some(ptr) = stack.pop() {
        let children = match nodes.get_mut(&ptr) {
            Some(node) if node.color != Color::Black => {
                node.color = Color::Black;
                node.children.clone()
            }
            _ => continue,
        };
        for (child, _) in children {
            if let Some(child_node) = nodes.get_mut(&child) {
                child_node.count += 1;
                if child_node.color != Color::Black {
                    stack.push(child);
                }
            }
        }
    }
}

/// Blacken every white struct whose destructor runs a user `drop`, every
/// white ancestor that can reach one, and everything reachable from them.
/// What stays white then has no path to any user `Drop` struct, so tearing
/// down the white set during stop-the-world can neither execute a user `drop`
/// directly nor drop the last strong reference to a `Drop` struct held by a
/// non-`Drop` cycle: any cycle holding a `Drop` struct leaks instead.
pub(super) fn keep_user_drops(nodes: &mut HashMap<usize, Node>, mut dtor_slot: impl FnMut(usize) -> u64) {
    let user_drops = crate::sync::lock(&USER_DROPS);
    if user_drops.is_empty() {
        return;
    }
    let pinned: Vec<usize> = nodes
        .iter()
        .filter(|(&ptr, node)| {
            node.color == Color::White
                && node.kind == CHILD_KIND_STRUCT
                && user_drops.contains(&(dtor_slot(ptr) as usize))
        })
        .map(|(&ptr, _)| ptr)
        .collect();
    drop(user_drops);
    if pinned.is_empty() {
        return;
    }

    // Build reverse edges among white nodes so every white ancestor that can
    // transitively reach a `Drop` struct is also blackened.
    let mut parents: HashMap<usize, Vec<usize>> = HashMap::new();
    for (&parent, node) in nodes.iter() {
        if node.color != Color::White {
            continue;
        }
        for &(child, _) in &node.children {
            if nodes.get(&child).is_some_and(|n| n.color == Color::White) {
                parents.entry(child).or_default().push(parent);
            }
        }
    }

    let mut worklist = pinned;
    let mut keep: HashSet<usize> = worklist.iter().copied().collect();
    while let Some(node_ptr) = worklist.pop() {
        if let Some(ps) = parents.get(&node_ptr) {
            for &p in ps {
                if keep.insert(p) {
                    worklist.push(p);
                }
            }
        }
    }

    for ptr in keep {
        scan_black(nodes, ptr);
    }
}

/// Run the Bacon-Rajan trial-deletion phases (MarkGray, Scan, user-drop pin)
/// over `roots` and return the garbage `(ptr, kind)` white set.
pub(super) fn run_trial_deletion(
    roots: &[(usize, u8)],
    mut seed: impl FnMut(&mut HashMap<usize, Node>, usize, u8) -> bool,
    dtor_slot: impl FnMut(usize) -> u64,
) -> Vec<(usize, u8)> {
    let mut nodes: HashMap<usize, Node> = HashMap::new();
    for &(ptr, kind) in roots {
        if seed(&mut nodes, ptr, kind) {
            mark_gray(&mut nodes, ptr, &mut seed);
        }
    }
    for &(ptr, _) in roots {
        scan(&mut nodes, ptr);
    }
    keep_user_drops(&mut nodes, dtor_slot);
    nodes
        .iter()
        .filter(|(_, node)| node.color == Color::White)
        .map(|(&ptr, node)| (ptr, node.kind))
        .collect()
}

// a teardown guard far above any real count: added to every white's real
// counter before destruction, so a release arriving from another dying
// object decrements a huge number instead of ever reaching a death path
// the collector is about to run by hand.
const WHITE_GUARD: i64 = 1 << 30;

pub(super) unsafe fn guard_white(ptr: usize, kind: u8) {
    match kind {
        // a struct takes the destructor guard instead, which a weak upgrade
        // refuses: a drop running during teardown must not revive a white
        // through a weak reference, since its shell is freed below.
        CHILD_KIND_STRUCT => {
            // SAFETY: `ptr` is a validated white struct under STW.
            unsafe {
                crate::runtime_core::cycle_struct_guard_strong(
                    ptr as i64,
                    crate::runtime_core::STRUCT_DTOR_GUARD,
                );
            }
        }
        CHILD_KIND_CLOSURE => {
            // SAFETY: `ptr` is a validated white closure under STW.
            unsafe { crate::runtime_core::cycle_closure_guard_strong(ptr as i64, WHITE_GUARD) };
        }
        CHILD_KIND_LIST => {
            // SAFETY: `ptr` is a validated white list under STW.
            unsafe {
                crate::collections::list::cycle_list_guard_strong(ptr as i64, WHITE_GUARD as u32);
            }
        }
        CHILD_KIND_MAP => {
            // SAFETY: `ptr` is a validated white map under STW.
            unsafe {
                crate::collections::map::cycle_map_guard_strong(ptr as i64, WHITE_GUARD as u32);
            }
        }
        CHILD_KIND_SET => {
            // SAFETY: `ptr` is a validated white set under STW.
            unsafe {
                crate::collections::set::cycle_set_guard_strong(ptr as i64, WHITE_GUARD as u32);
            }
        }
        _ => {}
    }
}

/// A white's normal destruction body: release everything it owns. edges to
/// other whites decrement guarded counts harmlessly; edges out of the
/// garbage release for real, which can cascade ordinary frees and can push
/// fresh suspects. both are fine, because no buffer lock is held here.
unsafe fn destroy_white(ptr: usize, kind: u8) {
    match kind {
        CHILD_KIND_STRUCT => {
            // SAFETY: All whites are guarded under STW; safe to run struct destructor.
            unsafe { crate::runtime_core::cycle_struct_run_dtor(ptr as i64) }
        }
        CHILD_KIND_CLOSURE => {
            // SAFETY: All whites are guarded under STW; safe to release closure env.
            unsafe { crate::runtime_core::cycle_closure_release_env(ptr as i64) }
        }
        CHILD_KIND_LIST => {
            // SAFETY: All whites are guarded under STW; safe to release list elements.
            unsafe { crate::collections::list::cycle_list_release_elements(ptr as i64) }
        }
        CHILD_KIND_MAP => {
            // SAFETY: All whites are guarded under STW; safe to release map values.
            unsafe { crate::collections::map::cycle_map_release_values(ptr as i64) }
        }
        CHILD_KIND_SET => {
            // SAFETY: All whites are guarded under STW; safe to release set elements.
            unsafe { crate::collections::set::cycle_set_release_elements(ptr as i64) }
        }
        _ => {}
    }
}

/// A white's terminal free, everything its kind's death path does after the
/// destruction body it already ran.
unsafe fn free_white_shell(ptr: usize, kind: u8) {
    match kind {
        CHILD_KIND_STRUCT => {
            // SAFETY: All white destruction bodies completed under STW; safe to free dead struct.
            unsafe { crate::runtime_core::cycle_struct_free_dead(ptr as i64) }
        }
        CHILD_KIND_CLOSURE => {
            // SAFETY: All white destruction bodies completed under STW; safe to free dead closure.
            unsafe { crate::runtime_core::cycle_closure_free_dead(ptr as i64) }
        }
        CHILD_KIND_LIST => {
            // SAFETY: All white destruction bodies completed under STW; safe to free dead list.
            unsafe { crate::collections::list::cycle_list_free_dead(ptr as i64) }
        }
        CHILD_KIND_MAP => {
            // SAFETY: All white destruction bodies completed under STW; safe to free dead map.
            unsafe { crate::collections::map::cycle_map_free_dead(ptr as i64) }
        }
        CHILD_KIND_SET => {
            // SAFETY: All white destruction bodies completed under STW; safe to free dead set.
            unsafe { crate::collections::set::cycle_set_free_dead(ptr as i64) }
        }
        _ => {}
    }
}

/// Validate one drained suspect under STW, clearing its buffered bit.
/// Returns `Some((child_kind, is_live_struct))` if the suspect is live and
/// should enter the candidate root set; dead structs have their buffer weak
/// count released immediately.
pub(super) unsafe fn validate_suspect(ptr: usize, kind: u8) -> Option<(u8, bool)> {
    match kind {
        CYCLE_KIND_STRUCT => {
            // SAFETY: Caller holds STW and `ptr` is a buffered struct with a held weak count.
            match unsafe { crate::runtime_core::cycle_struct_alive(ptr as i64) } {
                Some(true) => {
                    // SAFETY: Struct header is live and buffered.
                    unsafe { crate::runtime_core::cycle_clear_struct_buffered(ptr as i64) };
                    Some((CHILD_KIND_STRUCT, true))
                }
                Some(false) => {
                    // the value died while buffered: nothing to collect, the
                    // buffer's weak count is all that held the header.
                    // SAFETY: Struct header is alive via the buffer's weak count.
                    unsafe {
                        crate::runtime_core::cycle_clear_struct_buffered(ptr as i64);
                        crate::runtime_core::pith_struct_weak_release(ptr as i64);
                    }
                    None
                }
                None => None,
            }
        }
        CYCLE_KIND_CLOSURE => {
            // SAFETY: Caller holds STW and `ptr` came from a buffered closure entry.
            if unsafe { crate::runtime_core::cycle_closure_strong_count(ptr as i64) }
                .is_some_and(|c| c > 0)
            {
                // SAFETY: Closure validated live above.
                unsafe { crate::runtime_core::cycle_clear_closure_buffered(ptr as i64) };
                Some((CHILD_KIND_CLOSURE, false))
            } else {
                // scrubbed magic or dying (rc == 0): the object died into the
                // graveyard, whose snapshot below frees the shell.
                None
            }
        }
        CYCLE_KIND_LIST => {
            // SAFETY: Caller holds STW and `ptr` came from a buffered list entry.
            if unsafe { crate::collections::list::cycle_list_strong_count(ptr as i64) }
                .is_some_and(|c| c > 0)
            {
                // SAFETY: List validated live above.
                unsafe { crate::collections::list::cycle_clear_list_buffered(ptr as i64) };
                Some((CHILD_KIND_LIST, false))
            } else {
                None
            }
        }
        CYCLE_KIND_MAP => {
            // SAFETY: Caller holds STW and `ptr` came from a buffered map entry.
            if unsafe { crate::collections::map::cycle_map_strong_count(ptr as i64) }
                .is_some_and(|c| c > 0)
            {
                // SAFETY: Map validated live above.
                unsafe { crate::collections::map::cycle_clear_map_buffered(ptr as i64) };
                Some((CHILD_KIND_MAP, false))
            } else {
                None
            }
        }
        _ => None,
    }
}

unsafe fn drop_parked_shell(ptr: usize, kind: u8) {
    match kind {
        CYCLE_KIND_CLOSURE => {
            // SAFETY: `ptr` is a fully destructed closure shell parked in the graveyard.
            unsafe { crate::runtime_core::cycle_drop_closure_shell(ptr) }
        }
        CYCLE_KIND_LIST => {
            // SAFETY: `ptr` is a fully destructed list shell parked in the graveyard.
            unsafe { crate::collections::list::cycle_drop_list_shell(ptr) }
        }
        CYCLE_KIND_MAP => {
            // SAFETY: `ptr` is a fully destructed map shell parked in the graveyard.
            unsafe { crate::collections::map::cycle_drop_map_shell(ptr) }
        }
        _ => {}
    }
}

/// restores the flag (and removes the visit hook) even when a collection
/// panics, so the process is left inert rather than half-instrumented.
struct CollectGuard {
    was_on: bool,
}

impl CollectGuard {
    fn enter() -> Self {
        let was_on = set_on_collector(true);
        install_visit_hook(collector_visit as extern "C" fn(i64, i64) as usize);
        CollectGuard { was_on }
    }
}

impl Drop for CollectGuard {
    fn drop(&mut self) {
        install_visit_hook(0);
        let _ = set_on_collector(self.was_on);
    }
}

/// One trial-deletion collection. Must run with the world stopped: it reads
/// and rewrites reference counts assuming no mutator moves them.
///
/// # Safety
/// The caller holds the world stopped (`with_world_stopped`), and every
/// pointer in the suspect buffer and graveyard satisfies the invariants the
/// buffer maintains (see the module doc).
pub(crate) unsafe fn collect() {
    let _guard = CollectGuard::enter();

    // 1. drain and validate the roots. the buffer is swapped empty first, so
    // suspects pushed by teardown below land in a fresh buffer for the next
    // round. the graveyard is snapshotted at the same moment: shells parked
    // during teardown pair with entries in that fresh buffer and must
    // survive until the pass that drains those entries.
    let drained = std::mem::take(&mut *lock_suspects());
    let parked = std::mem::take(&mut *lock_graveyard());

    let mut roots: Vec<(usize, u8)> = Vec::new();
    // live struct roots keep the buffer's weak count until the very end of
    // the pass: the world is stopped, so nothing re-suspends them mid-pass,
    // and the held count is what guarantees the header outlives every phase.
    let mut root_struct_weaks: Vec<i64> = Vec::new();
    for (ptr, kind) in drained {
        // SAFETY: World is stopped and `drained` holds buffer-invariant entries.
        if let Some((child_kind, is_struct)) = unsafe { validate_suspect(ptr, kind) } {
            if is_struct {
                root_struct_weaks.push(ptr as i64);
            }
            roots.push((ptr, child_kind));
        }
    }
    CYCLE_ROOTS_EXAMINED.fetch_add(roots.len() as u64, Ordering::Relaxed);

    // 2-3. trial deletion over the graph the roots reach.
    let whites = run_trial_deletion(
        &roots,
        |nodes, ptr, kind| {
            // SAFETY: World is stopped; `ptr` came from a validated root or traced edge.
            unsafe { seed_node(nodes, ptr, kind) }
        },
        |ptr| {
            // SAFETY: World is stopped and `ptr` is a validated white struct node.
            unsafe { crate::runtime_core::cycle_struct_dtor_slot(ptr as i64) }
        },
    );

    // 4. free the whites: guard every real count first so teardown can never
    // recursively free a member, then run every destruction body, then free
    // every shell. three full passes, in that order: a destruction body may
    // touch any other white, so no white's shell may go before the last
    // body has run.
    for &(ptr, kind) in &whites {
        // SAFETY: `ptr` is a validated white garbage object under STW.
        unsafe { guard_white(ptr, kind) };
    }
    for &(ptr, kind) in &whites {
        // SAFETY: All whites are guarded; teardown releases edges safely under STW.
        unsafe { destroy_white(ptr, kind) };
    }
    for &(ptr, kind) in &whites {
        // SAFETY: All white destruction bodies completed; safe to free shells.
        unsafe { free_white_shell(ptr, kind) };
    }
    CYCLE_WHITES_FREED.fetch_add(whites.len() as u64, Ordering::Relaxed);

    // 5. the graveyard snapshot: shells fully destructed before this pass,
    // whose buffer entries were discarded in the drain above.
    CYCLE_GRAVEYARD_FREED.fetch_add(parked.len() as u64, Ordering::Relaxed);
    for (ptr, kind) in parked {
        // SAFETY: `parked` was snapshotted before teardown; its entries were drained above.
        unsafe { drop_parked_shell(ptr, kind) };
    }

    // 6. drop the buffer's weak count on every drained live struct root. a
    // root that stayed black is simply un-buffered again; one that went
    // white had its shell freed above, and this lets outstanding weak
    // references (or nothing) decide when the header goes.
    for ptr in root_struct_weaks {
        // SAFETY: Each entry owns one weak count held by the drained suspect buffer.
        unsafe { crate::runtime_core::pith_struct_weak_release(ptr) };
    }

    CYCLE_COLLECTIONS.fetch_add(1, Ordering::Relaxed);
}
