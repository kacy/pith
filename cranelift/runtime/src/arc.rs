//! Automatic Reference Counting (ARC) with cycle detection
//!
//! Every heap-allocated object has a header stored before it:
//! [RC Header][Object Data]
//!            ↑
//!         User pointer

use std::alloc::{alloc, dealloc, Layout};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Mutex;
use std::sync::LazyLock;

/// RC Header stored before each heap-allocated object
/// 
/// Layout in memory:
/// ```
/// [ref_count: i64][type_tag: u32][flags: u32][next: *mut RcHeader][...object data...]
///                                         ↑
///                                      User pointer
/// ```
#[repr(C)]
pub struct RcHeader {
    /// Reference count
    pub ref_count: AtomicI64,
    /// Type identifier for cycle detection
    pub type_tag: AtomicU32,
    /// Flags for cycle collector (MARKED, ROOT, etc.)
    pub flags: AtomicU32,
    /// Next pointer for global object list
    pub next: Mutex<Option<NonNull<RcHeader>>>,
}

/// Type tags for cycle detection
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

/// Get pointer to RC header from object pointer
/// 
/// # Safety
/// ptr must be a valid heap-allocated Forge object
#[inline]
pub unsafe fn header_from_ptr(ptr: *mut u8) -> *mut RcHeader {
    ptr.sub(HEADER_SIZE) as *mut RcHeader
}

/// Get object pointer from RC header
#[inline]
pub fn ptr_from_header(header: *mut RcHeader) -> *mut u8 {
    unsafe { (header as *mut u8).add(HEADER_SIZE) }
}

/// Allocate memory with RC header
/// 
/// # Safety
/// Returns a valid pointer to uninitialized memory
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
    
    // Add to global object list
    add_to_object_list(header);
    
    ptr.add(HEADER_SIZE)
}

/// Increment reference count
/// 
/// # Safety
/// ptr must be a valid heap-allocated Forge object or null
#[no_mangle]
pub unsafe extern "C" fn forge_rc_retain(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let header = header_from_ptr(ptr);
    let count = (*header).ref_count.fetch_add(1, Ordering::Relaxed);
    
    // Debug: detect double-retain issues
    if count >= 1_000_000_000 {
        eprintln!("forge: warning - suspiciously high ref count: {}", count);
    }
}

/// Decrement reference count, return true if object should be freed
/// 
/// # Safety
/// ptr must be a valid heap-allocated Forge object or null
#[inline]
pub unsafe fn rc_release_internal(ptr: *mut u8) -> bool {
    if ptr.is_null() {
        return false;
    }
    
    let header = header_from_ptr(ptr);
    let count = (*header).ref_count.fetch_sub(1, Ordering::Release);
    
    // If count was 1, we're the last reference
    count == 1
}

/// Release with destructor callback
/// 
/// # Safety
/// ptr must be a valid heap-allocated Forge object or null
#[no_mangle]
pub unsafe extern "C" fn forge_rc_release(ptr: *mut u8, destructor: Option<extern "C" fn(*mut u8)>) {
    if !rc_release_internal(ptr) {
        return;
    }
    
    // Last reference, destroy and free
    if let Some(dtor) = destructor {
        dtor(ptr);
    }
    
    let header = header_from_ptr(ptr);
    
    // Remove from global list
    remove_from_object_list(header);
    
    // Free memory
    // Note: We need to store object size separately to properly dealloc
    // For now, using a reasonable max size
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

pub fn add_to_object_list(header: *mut RcHeader) {
    let mut list = OBJECT_LIST.lock().unwrap();
    let header_nn = NonNull::new(header).unwrap();
    
    // Insert at head
    unsafe {
        if let Some(old_head) = (*header).next.lock().unwrap().take() {
            *(*header).next.lock().unwrap() = Some(old_head);
        }
    }
    list.head = Some(header_nn);
}

pub fn remove_from_object_list(_header: *mut RcHeader) {
    // TODO: Implement removal from linked list
    // For now, we just leave it (will be cleaned up on cycle collection)
}

pub fn init_cycle_collector() {
    // TODO: Start background thread
}

pub fn shutdown_cycle_collector() {
    // TODO: Stop background thread and clean up
}

/// Mark and scan cycle collection
pub fn collect_cycles() {
    // TODO: Implement proper cycle collection
    // This is a placeholder
}
