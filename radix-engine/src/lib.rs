#![cfg_attr(not(feature = "std"), no_std)]

extern crate core;
#[cfg(not(any(feature = "std", feature = "alloc")))]
compile_error!("Either feature `std` or `alloc` must be enabled for this crate.");
#[cfg(all(feature = "std", feature = "alloc"))]
compile_error!("Feature `std` and `alloc` can't be enabled at the same time.");

/// Radix Engine implementation.
pub mod engine;
/// Radix Engine state abstraction.
pub mod ledger;
/// Radix Engine models.
pub mod model;
/// Radix Engine transaction abstraction.
pub mod transaction;
/// Wasm validation, instrumentation and execution.
pub mod wasm;
