//! loom models of the runtime's concurrency protocols, run by `make loom-check`
//! (`RUSTFLAGS="--cfg loom" cargo test --release -p pith-runtime --lib loom_`).
//!
//! each model is a small state machine of one protocol, explored by loom over
//! every interleaving and every weak-memory outcome the orderings allow. a
//! model uses the real code where it runs under loom (the blocking pool's
//! `Slot`, `runtime_core::weak_upgrade`, the green word's `wake_step` and
//! `after_yield`) and mirrors the rest step for step; each file says which.
//!
//! next to each model sits a `pre_fix` twin that runs the protocol as it was
//! before its fix and must fail, so a model that has stopped exploring the
//! interleaving it was written for shows up as a test failure.

mod blocking_wake;
mod channel;
mod green_word;
mod primitives;
mod task_await;
mod weak;

/// run a model of three or more threads with loom's preemption bound at 3
/// (unless `LOOM_MAX_PREEMPTIONS` sets one). unbounded, the channel's stray
/// wake model takes about 9.4 million iterations; every bug these models
/// were written for needs at most two preemptions.
fn model_bounded<F: Fn() + Sync + Send + 'static>(model: F) {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = builder.preemption_bound.or(Some(3));
    builder.check(model);
}
