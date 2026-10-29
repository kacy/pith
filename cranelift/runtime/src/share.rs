//! Which structs more than one task can reach.
//!
//! A struct starts out local: only the task that built it holds a reference,
//! so its counts can be updated with plain loads and stores, without the
//! locked read-modify-write an atomic count costs. When a value can become
//! reachable from another task or os thread it is marked shared, along with
//! everything reachable from it, and from then on its counts are atomic. The
//! places a value can cross are:
//!
//! - a spawn's closure, which carries what the new task captured (`pith_spawn`)
//! - a task's result, read by whoever awaits it (marked by the spawn wrapper)
//! - a channel send (marked by the emitter at the send)
//! - a module global, which any task can read (marked at each store and at the
//!   end of global initialization)
//! - a store into a struct, list or map that is already shared (the write
//!   barriers in the field store and the container store paths)
//!
//! A struct's mark is a bit in its header's flag word; lists, maps and
//! closures carry the same mark in their flag byte. Their own counts stay
//! atomic, and their mark only records that the walk has been through them,
//! so a container shared once is not walked again, and a later store into it
//! knows to mark what it stores. A weak edge is walked like a strong one: a
//! task that can read a weak reference can upgrade it.
//!
//! `PITH_RC_CHECK=1` records the task that built each struct and aborts when
//! another task updates a local count, which is how a missing mark shows up.
//! `PITH_LOCAL_COUNTS=0` keeps every count atomic, for comparison.

use crate::collections::list::ListTypeTag;
use std::collections::HashMap;
use std::sync::Mutex;

/// The mark a list, map or closure carries in its flag byte once shared.
pub(crate) const CONTAINER_SHARED_FLAG: u8 = 0x80;

// the child kinds a tracer reports and a closure tags its captures with
const CHILD_LIST: u8 = 2;
const CHILD_MAP: u8 = 3;
const CHILD_STRUCT: u8 = 6;
const CHILD_CLOSURE: u8 = 7;
const CHILD_WEAK: u8 = 8;

/// Mark `value`, a heap value of the kind `tag` (an element tag code), and
/// everything it reaches as shared.
///
/// # Safety
/// `value` must be a live value of that kind, or 0.
#[no_mangle]
pub unsafe extern "C" fn pith_share(value: i64, tag: i64) {
    if value == 0 {
        return;
    }
    share_value(value, crate::collections::list::element_tag_from_code(tag as i32));
}

/// Mark `value` shared when `holder`, a struct it is being stored into, is
/// shared already: the write barrier of a field store.
///
/// # Safety
/// `holder` must be a live struct; `value` a live value of the kind `tag`.
#[no_mangle]
pub unsafe extern "C" fn pith_share_into(holder: i64, value: i64, tag: i64) {
    if value != 0 && crate::runtime_core::struct_is_shared(holder) {
        pith_share(value, tag);
    }
}

pub(crate) unsafe fn share_value(value: i64, tag: ListTypeTag) {
    let child = match tag {
        ListTypeTag::Struct => CHILD_STRUCT,
        ListTypeTag::Closure => CHILD_CLOSURE,
        ListTypeTag::List => CHILD_LIST,
        ListTypeTag::Map => CHILD_MAP,
        // strings, bytes, sets and channels hold no struct, and their own
        // counts are atomic
        _ => return,
    };
    // the common case is a value with nothing to walk: one already shared, or
    // a spawn's closure over plain values. the work list stays empty, and so
    // unallocated, until a child turns up.
    let mut work: Vec<(i64, u8)> = Vec::new();
    mark_one(value, child, &mut work);
    while let Some((v, kind)) = work.pop() {
        mark_one(v, kind, &mut work);
    }
}

/// Mark one value and queue the children it reaches, unless it was marked
/// already, in which case everything it reaches was marked with it.
unsafe fn mark_one(v: i64, kind: u8, work: &mut Vec<(i64, u8)>) {
    if v == 0 {
        return;
    }
    let mut push = |c: i64, k: u8| {
        if c != 0 && matches!(k, CHILD_LIST | CHILD_MAP | CHILD_STRUCT | CHILD_CLOSURE | CHILD_WEAK) {
            work.push((c, if k == CHILD_WEAK { CHILD_STRUCT } else { k }));
        }
    };
    match kind {
        CHILD_STRUCT => {
            if crate::runtime_core::struct_mark_shared(v) {
                forget_owner(v);
                crate::cycle::struct_children(v, &mut push);
            }
        }
        CHILD_CLOSURE => {
            if crate::runtime_core::closure_mark_shared(v) {
                crate::runtime_core::cycle_closure_children(v, &mut push);
            }
        }
        CHILD_LIST => {
            if crate::collections::list::list_mark_shared(v) {
                crate::collections::list::cycle_list_children(v, &mut push);
            }
        }
        CHILD_MAP => {
            if crate::collections::map::map_mark_shared(v) {
                crate::collections::map::cycle_map_children(v, &mut push);
            }
        }
        _ => {}
    }
}

// --- the check mode ---------------------------------------------------------

static OWNERS: Mutex<Option<HashMap<usize, u64>>> = Mutex::new(None);

thread_local! {
    static THREAD_MARK: u8 = const { 0 };
}

/// The running task, or this os thread when no green task is running.
fn current_owner() -> u64 {
    if let Some(task) = crate::concurrency::green::current_task() {
        return (task as u64) | (1 << 63);
    }
    thread_mark()
}

// behind a call of its own, like the runtime's other thread-local accessors
#[inline(never)]
fn thread_mark() -> u64 {
    THREAD_MARK.with(|m| m as *const u8 as u64)
}

fn lock_owners() -> std::sync::MutexGuard<'static, Option<HashMap<usize, u64>>> {
    OWNERS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A struct was just built: it belongs to the running task.
#[inline(always)]
pub(crate) fn note_owner(ptr: i64) {
    if check_on() {
        note_owner_slow(ptr);
    }
}

// the check mode's bookkeeping stays out of line: inlined into the
// allocation and free paths it made every call save registers it never uses
#[cold]
#[inline(never)]
fn note_owner_slow(ptr: i64) {
    lock_owners().get_or_insert_with(HashMap::new).insert(ptr as usize, current_owner());
}

/// A struct was shared or freed: any task may touch it now, or none.
#[inline(always)]
pub(crate) fn forget_owner(ptr: i64) {
    if check_on() {
        forget_owner_slow(ptr);
    }
}

#[cold]
#[inline(never)]
fn forget_owner_slow(ptr: i64) {
    if let Some(owners) = lock_owners().as_mut() {
        owners.remove(&(ptr as usize));
    }
}

/// Whether `PITH_RC_CHECK` asked for owner checks on plain counts.
#[inline(always)]
fn check_on() -> bool {
    crate::switches::any(crate::switches::RC_CHECK)
}

/// A plain count update on a local struct: in check mode, abort if a task
/// other than the one that built it is making it. `switches` is the caller's
/// copy of `switches::get()`.
#[inline(always)]
pub(crate) fn check_local(switches: u8, ptr: i64, op: &str) {
    if switches & crate::switches::RC_CHECK != 0 {
        check_local_slow(ptr, op);
    }
}

#[cold]
fn check_local_slow(ptr: i64, op: &str) {
    let owner = lock_owners().as_ref().and_then(|owners| owners.get(&(ptr as usize)).copied());
    if let Some(owner) = owner {
        if owner != current_owner() {
            eprintln!(
                "pith rc check: {op} of a local struct at {ptr:#x} from a task that did not build it; a value reached another task without being marked shared"
            );
            eprintln!("{}", std::backtrace::Backtrace::force_capture());
            // the check mode exists to stop at the first unmarked crossing
            std::process::abort();
        }
    }
}
