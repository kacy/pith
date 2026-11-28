//! file and process-environment entry points.
//!
//! ## why half of this file goes through `blocking`
//!
//! a regular file is always reported ready by epoll, however slow the device
//! behind it is, so the reactor that keeps socket i/o off a green worker cannot
//! help here. anything that opens, reads, writes, or walks a file therefore
//! hands the syscall to the file thread pool and parks the calling task, the
//! same way dns does: see `blocking` for the pool, the park, and the rule about
//! what may cross to a pool thread.
//!
//! that rule is the reason these functions are shaped the way they are. the job
//! that runs on the pool thread deals only in plain owned data (a `String`
//! path, a `Vec<u8>`, a `std::fs::File`), and every pith-visible value (a
//! `Bytes` handle, a C string, a list) is built afterwards, back on the task's
//! own thread, where the thread-local freelists it comes from live.
//!
//! metadata and name operations (`exists`, `size`, `mkdir`, `rename`, the
//! single-file `remove`) stay direct. each is one syscall that the kernel
//! answers from its dentry cache in the overwhelmingly common case, which is
//! the same order of cost as the handoff itself; paying a park to protect
//! against a slow network mount would make the ordinary case worse for every
//! program. `remove_tree` and `list_dir` do not get that argument (their cost
//! grows with the tree), so those are offloaded.

use crate::blocking::Pool;

/// the threads that run file syscalls for green tasks. file i/o has its own
/// pool rather than sharing the resolver's so that one slow multi-megabyte
/// write cannot queue in front of a dial, or the reverse.
pub(super) static POOL: Pool = Pool::new("pith-file", 4);

mod env;
mod handles;
mod ops;
#[cfg(test)]
mod tests;

pub use env::*;
pub use handles::*;
pub use ops::*;
