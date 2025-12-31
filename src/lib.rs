//! Rust Timing Engine
//!
//! A deterministic "logical-time" timing engine with:
//! - Drift-free waits (time & beats)
//! - Structured concurrency (branch / branch_wait, cancellation cascades)
//! - Interactive tempo changes (wait(beats) retimes automatically)
//! - Dual execution modes: realtime (spin_sleep) and offline (stepping API)

pub mod pq;
pub mod tempo;
pub mod rng;
pub mod executor;
pub mod scheduler;
pub mod context;
pub mod barrier;
pub mod engine;

#[cfg(test)]
mod timing_tests;

pub use context::{Ctx, BranchOptions, BranchHandle, CancelHandlerUnsubscribe, WaitError};
pub use engine::{Engine, SchedulerMode};
pub use tempo::TempoMap;
pub use barrier::{start_barrier, resolve_barrier, await_barrier};
