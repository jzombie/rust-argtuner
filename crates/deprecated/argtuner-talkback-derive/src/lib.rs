#![doc = include_str!("../README.md")]

//! Deprecated migration shim: re-exports the `#[talkback_args]` attribute macro
//! under the old crate name so existing `argtuner-talkback-derive` users get a
//! clear deprecation warning while they migrate.

#[deprecated(
    since = "0.1.2-alpha",
    note = "argtuner-talkback-derive is deprecated; use `argtuner::talkback_args` instead"
)]
pub use argtuner_derive::talkback_args;
