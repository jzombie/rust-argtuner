#![doc = include_str!("../README.md")]

//! Deprecated migration shim: re-exports the `argtuner` SDK under the old crate
//! name so existing `argtuner-talkback` users get a clear deprecation warning
//! while they migrate to `argtuner`.

#[deprecated(
    since = "0.1.2-alpha",
    note = "argtuner-talkback is deprecated; use `argtuner` directly (e.g. `use argtuner::{init, emit_metrics, talkback_args};`)"
)]
pub use argtuner::*;
