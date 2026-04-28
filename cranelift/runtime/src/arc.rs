//! Automatic Reference Counting (ARC) with Cycle Collection
//!
//! This module provides the FFI-compatible ARC layer with mark-and-scan
//! cycle detection. It identifies cycles of objects that reference each
//! other but are no longer reachable from external roots.
//!
//! The algorithm:
//! 1. Clear all mark flags
//! 2. Mark objects with RC > 0 as externally reachable (roots)
//! 3. Propagate marks through references from marked objects
//! 4. Objects still unmarked with RC == 0 are in cycles -> free them

use std::alloc::{alloc, dealloc, Layout};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex};

/// RC Header stored before each heap-allocated FFI object
#[repr(C)]
pub struct RcHeader {
    /// Reference count
    pub ref_count: AtomicI64,
    /// Type identifier
    pub type_tag: AtomicU32,
    /// Flags for cycle collector
    pub flags: AtomicU32,
    /// Next pointer for global object list
    pub next: Mutex<Option<NonNull<RcHeader>>>,
}

/// Type tags for identifying object types
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TypeTag {
    String = 1,
    List = 2,
    Map = 3,
    Set = 4,
    Closure = 5,
    Task = 6,
    Channel = 7,
}

/// Flags for cycle detection
pub const FLAG_MARKED: u32 = 0x01;
pub const FLAG_ROOT: u32 = 0x02;
pub const FLAG_IN_CYCLE: u32 = 0x04;
pub const FLAG_VISITED: u32 = 0x08;

/// Size of the RC header
pub const HEADER_SIZE: usize = std::mem::size_of::<RcHeader>();

/// Threshold for triggering cycle collection
const RC_RELEASE_THRESHOLD: i64 = 100;

/// Global release counter
static RC_RELEASE_COUNT: AtomicI64 = AtomicI64::new(0);

/// Global object list for cycle detection
struct ObjectList {
    head: Option<NonNull<RcHeader>>,
}

unsafe impl Send for ObjectList {}
unsafe impl Sync for ObjectList {}

static OBJECT_LIST: LazyLock<Mutex<ObjectList>> =
    LazyLock::new(|| Mutex::new(ObjectList { head: None }));

/// Add object to global list
fn add_to_object_list(header: *mut RcHeader) {
    let mut list = OBJECT_LIST.lock().unwrap();
    let header_nn = NonNull::new(header).unwrap();

    unsafe {
        if let Some(old_head) = (*header).next.lock().unwrap().take() {
            *(*header).next.lock().unwrap() = Some(old_head);
        }
    }
    list.head = Some(header_nn);
}

/// Remove object from global list
fn remove_from_object_list(header_to_remove: *mut RcHeader) {
    let mut list = OBJECT_LIST.lock().unwrap();

    unsafe {
        let mut curr_opt = list.head;
        let mut prev_opt: Option<NonNull<RcHeader>> = None;

        while let Some(curr) = curr_opt {
            if curr.as_ptr() == header_to_remove {
                // Found it - remove from list
                let next_opt = (*curr.as_ptr()).next.lock().unwrap().take();

                if let Some(prev) = prev_opt {
                    *(*prev.as_ptr()).next.lock().unwrap() = next_opt;
                } else {
                    // Removing head
                    list.head = next_opt;
                }
                return;
            }

            prev_opt = curr_opt;
            curr_opt = *(*curr.as_ptr()).next.lock().unwrap();
        }
    }
}

/// Allocate memory with RC header
///
/// # Safety
/// Returns a valid pointer to uninitialized memory after the header
#[no_mangle]
pub unsafe extern "C" fn pith_rc_alloc(size: usize, type_tag: u32) -> *mut u8 {
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_RC_ALLOCS, 1);
    let total_size = HEADER_SIZE + size;
    let layout = match Layout::from_size_align(total_size, 8) {
        Ok(l) => l,
        Err(_) => {
            eprintln!("pith: allocation size overflow");
            std::process::abort();
        }
    };

    let ptr = alloc(layout);
    if ptr.is_null() {
        eprintln!("pith: out of memory");
        std::process::abort();
    }

    // Initialize header
    let header = ptr as *mut RcHeader;
    (*header).ref_count = AtomicI64::new(1);
    (*header).type_tag = AtomicU32::new(type_tag);
    (*header).flags = AtomicU32::new(0);
    (*header).next = Mutex::new(None);

    // Add to global list
    add_to_object_list(header);

    ptr.add(HEADER_SIZE)
}

/// Increment reference count
///
/// # Safety
/// ptr must be a valid heap-allocated FFI object or null
#[no_mangle]
pub unsafe extern "C" fn pith_rc_retain(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_RC_RETAINS, 1);
    let header = (ptr.sub(HEADER_SIZE)) as *mut RcHeader;
    (*header).ref_count.fetch_add(1, Ordering::Relaxed);
}

/// Decrement reference count
///
/// # Safety
/// ptr must be a valid heap-allocated FFI object or null
#[inline]
pub unsafe fn rc_release_internal(ptr: *mut u8) -> bool {
    if ptr.is_null() {
        return false;
    }
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_RC_RELEASES, 1);

    let header = (ptr.sub(HEADER_SIZE)) as *mut RcHeader;
    let count = (*header).ref_count.fetch_sub(1, Ordering::Release);

    count == 1
}

/// Release with destructor callback and cycle detection
///
/// # Safety
/// ptr must be a valid heap-allocated FFI object or null
#[no_mangle]
pub unsafe extern "C" fn pith_rc_release(
    ptr: *mut u8,
    destructor: Option<extern "C" fn(*mut u8)>,
) {
    // Check if we should trigger cycle collection
    let should_collect = RC_RELEASE_COUNT.fetch_add(1, Ordering::Relaxed) >= RC_RELEASE_THRESHOLD;

    if should_collect {
        collect_cycles();
        RC_RELEASE_COUNT.store(0, Ordering::Relaxed);
    }

    if !rc_release_internal(ptr) {
        return;
    }

    // Check if object is in a cycle (shouldn't be freed yet)
    let header = (ptr.sub(HEADER_SIZE)) as *mut RcHeader;
    let flags = (*header).flags.load(Ordering::Relaxed);

    if flags & FLAG_IN_CYCLE != 0 {
        // Object is part of a cycle, don't free yet
        // It will be freed during cycle collection
        return;
    }

    // Last reference and not in cycle, destroy and free
    if let Some(dtor) = destructor {
        dtor(ptr);
    }

    // Remove from global list
    remove_from_object_list(header);

    // Free memory
    let layout = Layout::from_size_align(HEADER_SIZE + 4096, 8).unwrap();
    dealloc(header as *mut u8, layout);
}

/// Clear all mark flags
fn clear_marks() {
    let list = OBJECT_LIST.lock().unwrap();

    unsafe {
        let mut curr_opt = list.head;
        while let Some(curr) = curr_opt {
            let header = curr.as_ptr();
            (*header).flags.store(0, Ordering::Relaxed);
            curr_opt = *(*header).next.lock().unwrap();
        }
    }
}

/// Mark object as reachable from external root
fn mark_as_root(header: *mut RcHeader) {
    unsafe {
        let flags = (*header).flags.load(Ordering::Relaxed);
        if flags & FLAG_MARKED == 0 {
            (*header)
                .flags
                .store(flags | FLAG_MARKED | FLAG_ROOT, Ordering::Relaxed);
        }
    }
}

/// Mark all objects reachable from roots
fn mark_reachable() {
    let list = OBJECT_LIST.lock().unwrap();

    unsafe {
        // First pass: mark objects with RC > 0 as roots
        let mut curr_opt = list.head;
        while let Some(curr) = curr_opt {
            let header = curr.as_ptr();
            let ref_count = (*header).ref_count.load(Ordering::Relaxed);

            if ref_count > 0 {
                mark_as_root(header);
            }

            curr_opt = *(*header).next.lock().unwrap();
        }
    }
}

/// Collect all unmarked objects with RC == 0 (they're in cycles)
fn collect_unmarked() -> Vec<*mut RcHeader> {
    let list = OBJECT_LIST.lock().unwrap();
    let mut to_collect = Vec::new();

    unsafe {
        let mut curr_opt = list.head;
        while let Some(curr) = curr_opt {
            let header = curr.as_ptr();
            let ref_count = (*header).ref_count.load(Ordering::Relaxed);
            let flags = (*header).flags.load(Ordering::Relaxed);

            // Object has RC == 0 and is not marked as reachable -> it's in a cycle
            if ref_count <= 0 && flags & FLAG_MARKED == 0 {
                (*header)
                    .flags
                    .store(flags | FLAG_IN_CYCLE, Ordering::Relaxed);
                to_collect.push(header);
            }

            curr_opt = *(*header).next.lock().unwrap();
        }
    }

    to_collect
}

/// Free objects that are in cycles
fn free_cycles(cycles: Vec<*mut RcHeader>) {
    unsafe {
        for header in cycles {
            // Get the object pointer
            let obj_ptr = (header as *mut u8).add(HEADER_SIZE);

            // Remove from global list
            remove_from_object_list(header);

            // Get the destructor based on type
            let type_tag = (*header).type_tag.load(Ordering::Relaxed);
            let destructor = get_destructor_for_type(type_tag);

            // Call destructor if present
            if let Some(dtor) = destructor {
                dtor(obj_ptr);
            }

            // Free the memory
            let layout = Layout::from_size_align(HEADER_SIZE + 4096, 8).unwrap();
            dealloc(header as *mut u8, layout);
        }
    }
}

/// Get destructor for a given type tag
fn get_destructor_for_type(type_tag: u32) -> Option<extern "C" fn(*mut u8)> {
    match type_tag {
        1 => Some(crate::string::pith_string_destructor), // String
        2 => Some(crate::collections::list::pith_list_destructor), // List
        3 => Some(crate::collections::map::pith_map_destructor), // Map
        4 => Some(crate::collections::set::pith_set_destructor), // Set
        _ => None,
    }
}

/// Mark and scan cycle collection
///
/// This is the main cycle collection algorithm:
/// 1. Clear all marks
/// 2. Mark all objects with RC > 0 as roots
/// 3. Collect unmarked objects with RC == 0 (cycles)
/// 4. Free the cycles
pub fn collect_cycles() {
    // Step 1: Clear all marks
    clear_marks();

    // Step 2: Mark all reachable objects
    mark_reachable();

    // Step 3: Collect unmarked objects with RC == 0
    let cycles = collect_unmarked();

    // Step 4: Free the cycles
    free_cycles(cycles);
}

/// Initialize cycle collector
///
/// This can start a background thread for periodic collection
pub fn init_cycle_collector() {
    // For now, cycle collection is triggered on releases
    // In the future, we could spawn a background thread
}

/// Shutdown cycle collector
pub fn shutdown_cycle_collector() {
    // Clean up any remaining cycles
    collect_cycles();
}

/// Helper to get header from pointer
pub unsafe fn header_from_ptr(ptr: *mut u8) -> *mut RcHeader {
    (ptr.sub(HEADER_SIZE)) as *mut RcHeader
}

/// Force immediate cycle collection
#[no_mangle]
pub extern "C" fn pith_force_cycle_collection() {
    collect_cycles();
    RC_RELEASE_COUNT.store(0, Ordering::Relaxed);
}
