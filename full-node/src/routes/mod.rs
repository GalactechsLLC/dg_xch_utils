//! HTTP routes registered with Portfu.

#[cfg(feature = "profiling")]
mod debug_flamegraph;
#[cfg(feature = "profiling")]
mod debug_heap;
mod health;
mod metrics;
pub mod rpc;

/// Only one CPU or heap profile may run at a time.
#[cfg(feature = "profiling")]
pub(super) static PROFILING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
