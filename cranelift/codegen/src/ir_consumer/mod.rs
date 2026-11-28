//! IR consumer: translates Pith text IR to Cranelift native code.
//!
//! Pipeline: Pith text IR → DCE & string fusion (`dce`, `passes`) →
//! function lowering (`lower`, `fast_paths`, `arith`, `instrumentation`) →
//! post-lowering inlining & loop header hoisting (`module`) → native object code.
//!
//! The IR grammar, type/retkind vocabulary, and ABI conventions are specified
//! in `docs/ir-contract.md`.

mod arith;
mod dce;
mod fast_paths;
mod instrumentation;
mod ir;
mod lower;
mod module;
mod passes;

#[cfg(test)]
mod tests;

pub use module::compile_from_ir;
