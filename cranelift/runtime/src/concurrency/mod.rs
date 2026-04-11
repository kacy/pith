//! Concurrency primitives: Task, Channel, Mutex, WaitGroup, Semaphore

pub mod channel;
pub mod mutex;
pub mod semaphore;
pub mod task;
pub mod waitgroup;

// Re-export FFI functions for use by the codegen
pub use channel::{
    forge_channel_cap, forge_channel_close, forge_channel_free, forge_channel_len,
    forge_channel_new, forge_channel_recv, forge_channel_send,
};
pub use mutex::{forge_mutex_free, forge_mutex_lock, forge_mutex_new, forge_mutex_unlock};
pub use semaphore::{
    forge_semaphore_acquire, forge_semaphore_free, forge_semaphore_new, forge_semaphore_release,
};
pub use waitgroup::{
    forge_waitgroup_add, forge_waitgroup_done, forge_waitgroup_free, forge_waitgroup_new,
    forge_waitgroup_wait,
};
