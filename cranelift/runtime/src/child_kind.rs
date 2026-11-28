//! Child-edge kind tags shared by closure capture environments, cycle-collector
//! tracers (`pith_cc_visit`), and the task-sharing walker (`share`).
//!
//! These codes are part of the compiler/runtime ABI and **must not be
//! renumbered**. They are distinct from [`crate::collections::list::ListTypeTag`]
//! element tag codes (`4 = Struct`, `6 = Closure`, `7 = Set`), which appear on
//! container store instructions.

pub(crate) const STRING: u8 = 1;
pub(crate) const LIST: u8 = 2;
pub(crate) const MAP: u8 = 3;
pub(crate) const SET: u8 = 4;
pub(crate) const BYTES: u8 = 5;
pub(crate) const STRUCT: u8 = 6;
pub(crate) const CLOSURE: u8 = 7;
pub(crate) const WEAK: u8 = 8;
pub(crate) const CHANNEL: u8 = 9;
pub(crate) const SYNC: u8 = 10;
