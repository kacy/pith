//! Concurrency primitives: Task, Channel, Mutex, WaitGroup, Semaphore

pub mod channel;
pub mod mutex;
pub mod semaphore;
pub mod task;
pub mod waitgroup;

// Re-export FFI functions for use by the codegen
pub use channel::{
    pith_channel_cap, pith_channel_close, pith_channel_is_closed, pith_channel_len,
    pith_channel_new, pith_channel_recv, pith_channel_send, pith_channel_try_recv,
    pith_channel_try_send, pith_select_next_index,
};
pub use mutex::{pith_mutex_lock, pith_mutex_new, pith_mutex_unlock};
pub use semaphore::{
    pith_semaphore_acquire, pith_semaphore_new, pith_semaphore_release,
};
pub use waitgroup::{
    pith_waitgroup_add, pith_waitgroup_done, pith_waitgroup_new, pith_waitgroup_wait,
};
