//! Automatic Reference Counting (ARC) - FFI Boundary
//!
//! This module provides the FFI-compatible ARC layer.
//! Internally, we use std::sync::Arc for Rust code.
//! 
//! The functions here are called from the compiler's generated code
//! to manage heap-allocated objects.

use std::alloc::{alloc, dealloc, Layout};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Mutex;
use std::sync::LazyLock;

/// RC Header stored before each heap-allocated object (for FFI compatibility)
/// 
/// Note: This is mainly for objects that cross the FFI boundary.
/// Internal Rust code uses std::sync::Arc instead.
#[repr(C)]
pub struct RcHeader {
    /// Reference count
    pub ref_count: AtomicI64,
    /// Type identifier for cycle detection
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

/// Size of the RC header
pub const HEADER_SIZE: usize = std::mem::size_of::<RcHeader>();

/// Allocate memory with RC header (for FFI objects)
/// 
/// # Safety
/// Returns a valid pointer to uninitialized memory after the header
#[no_mangle]
pub unsafe extern "C" fn forge_rc_alloc(size: usize, type_tag: u32) -> *mut u8 {
    let total_size = HEADER_SIZE + size;
    let layout = match Layout::from_size_align(total_size, 8) {
        Ok(l) => l,
        Err(_) => {
            eprintln!("forge: allocation size overflow");
            std::process::abort();
        }
    };
    
    let ptr = alloc(layout);
    if ptr.is_null() {
        eprintln!("forge: out of memory");
        std::process::abort();
    }
    
    // Initialize header
    let header = ptr as *mut RcHeader;
    (*header).ref_count = AtomicI64::new(1);
    (*header).type_tag = AtomicU32::new(type_tag);
    (*header).flags = AtomicU32::new(0);
    (*header).next = Mutex::new(None);
    
    // Add to global object list for cycle detection
    add_to_object_list(header);
    
    ptr.add(HEADER_SIZE)
}

/// Increment reference count (for FFI)
/// 
/// # Safety
/// ptr must be a valid heap-allocated FFI object or null
#[no_mangle]
pub unsafe extern "C" fn forge_rc_retain(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let header = (ptr.sub(HEADER_SIZE)) as *mut RcHeader;
    (*header).ref_count.fetch_add(1, Ordering::Relaxed);
}

/// Decrement reference count, return true if object should be freed
#[inline]
pub unsafe fn rc_release_internal(ptr: *mut u8) -> bool {
    if ptr.is_null() {
        return false;
    }
    
    let header = (ptr.sub(HEADER_SIZE)) as *mut RcHeader;
    let count = (*header).ref_count.fetch_sub(1, Ordering::Release);
    
    count == 1
}

/// Release with destructor callback (for FFI)
/// 
/// # Safety
/// ptr must be a valid heap-allocated FFI object or null
#[no_mangle]
pub unsafe extern "C" fn forge_rc_release(ptr: *mut u8, destructor: Option<extern "C" fn(*mut u8)>) {
    if !rc_release_internal(ptr) {
        return;
    }
    
    // Last reference, destroy and free
    if let Some(dtor) = destructor {
        dtor(ptr);
    }
    
    let header = (ptr.sub(HEADER_SIZE)) as *mut RcHeader;
    
    // Remove from global list
    remove_from_object_list(header);
    
    // Free memory (use a reasonable max size since we don't track exact size)
    let layout = Layout::from_size_align(HEADER_SIZE + 4096, 8).unwrap();
    dealloc(header as *mut u8, layout);
}

// Global object list for cycle detection
struct ObjectList {
    head: Option<NonNull<RcHeader>>,
}

unsafe impl Send for ObjectList {}
unsafe impl Sync for ObjectList {}

static OBJECT_LIST: LazyLock<Mutex<ObjectList>> = 
    LazyLock::new(|| Mutex::new(ObjectList { head: None }));

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

fn remove_from_object_list(_header: *mut RcHeader) {
    // TODO: Implement removal from linked list
}

/// Initialize cycle collector
pub fn init_cycle_collector() {
    // TODO: Start background thread
}

/// Shutdown cycle collector
pub fn shutdown_cycle_collector() {
    // TODO: Stop background thread
}

/// Mark and scan cycle collection
pub fn collect_cycles() {
    // TODO: Implement proper cycle collection
}

// Helper to get header from pointer
pub unsafe fn header_from_ptr(ptr: *mut u8) -> *mut RcHeader {
    (ptr.sub(HEADER_SIZE)) as *mut RcHeader
}
