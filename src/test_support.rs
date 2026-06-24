//! Helpers for integration tests that need to invoke argtuner binaries.
//!
//! The main entry point is [`bin_command`], which returns a shell-quoted
//! command string suitable for use as a template in `argtuner.toml`.
//!
//! ```ignore
//! let template = CommandTemplate::new(argtuner::test_support::bin_command("emit_result"));
//! ```

use std::path::{Path, PathBuf};

/// Resolves the filesystem path to a compiled argtuner binary.
///
/// Checks `CARGO_BIN_EXE_<bin>` (set by `cargo test`) first, then falls
/// back to `target/debug/<bin>` relative to the workspace root.
fn command_path(bin: &str) -> Option<PathBuf> {
    let env_key = format!("CARGO_BIN_EXE_{bin}");
    if let Ok(path) = std::env::var(env_key) {
        return Some(PathBuf::from(path));
    }

    let mut path = crate::workspace_root()
        .join("target")
        .join("debug")
        .join(bin);
    if cfg!(windows) {
        path.set_extension("exe");
    }
    if path.exists() {
        return Some(path);
    }
    None
}

/// Shell-quotes a path so spaces and special characters are preserved
/// when the string is later parsed by `shell_words::split`.
fn quote_path(path: &Path) -> String {
    shell_words::quote(&path.to_string_lossy()).to_string()
}

/// Returns a shell-quoted command string for the given argtuner binary.
///
/// When running under `cargo test`, `CARGO_BIN_EXE_<bin>` points to the
/// compiled binary; the returned string is a directly executable command.
/// Otherwise a `cargo run` invocation is returned instead.
///
/// The result is shell-quoted so that paths containing spaces (e.g.
/// `/Volumes/2TB Storage Vault/...`) are handled correctly when the
/// string is split by `shell_words::split` at runtime.
pub fn bin_command(bin: &str) -> String {
    if let Some(path) = command_path(bin) {
        return quote_path(&path);
    }
    let manifest = crate::workspace_root().join("Cargo.toml");
    let manifest_text = manifest.to_string_lossy();
    let manifest_arg = shell_words::quote(&manifest_text);
    format!("cargo run -q --manifest-path {manifest_arg} -p argtuner --bin {bin} --")
}
