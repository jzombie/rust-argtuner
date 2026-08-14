#![doc = include_str!("../README.md")]

//! Deprecated migration shim: re-exports the `argtuner-sdk` under the old crate
//! name so existing `argtuner-talkback` users get a clear deprecation warning
//! while they migrate to `argtuner-sdk`.

#[deprecated(
    since = "0.1.2-alpha",
    note = "argtuner-talkback is deprecated; use `argtuner-sdk` directly (e.g. `use argtuner_sdk::{init, emit_metrics, talkback_args};`)"
)]
pub use argtuner_sdk::*;
