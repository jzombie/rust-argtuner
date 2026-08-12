//! Helpers for tests that need to invoke argtuner or its mock binaries.
//!
//! The main entry points are [`bin_command`] (a shell-quoted command string
//! suitable for use as a template in `argtuner.toml`) and [`bin_path`] (the raw
//! absolute path). Both resolve the compiled binary for a given bin name.
//!
//! The mock emit binaries live in the unpublished `argtuner-mock-bin` workspace
//! crate (never shipped with the published `argtuner` package). They are built
//! by `cargo test --workspace` / `cargo build --workspace` into the shared
//! `target/debug/` directory; [`bin_path`] returns that path and panics loudly
//! if the binary is missing so a test never silently runs a stale artifact.
//!
//! ```ignore
//! let template = CommandTemplate::new(argtuner::test_support::bin_command("mock_emit_result"));
//! ```

use std::path::PathBuf;

/// Absolute path to a compiled binary for the given bin name.
///
/// Resolution order:
/// 1. `CARGO_BIN_EXE_<bin>` (set by Cargo for same-package binaries), then
/// 2. `<workspace>/target/debug/<bin>` (built by `cargo test --workspace`).
///
/// Panics with a clear message if neither is available.
pub fn bin_path(bin: &str) -> PathBuf {
    // Run mock subprocesses over pipes instead of a PTY: POSIX pipes deliver
    // all bytes before EOF, so there is no macOS PTY buffer-destruction race on
    // child exit (and no need for a timer-based hold in the mock binaries).
    crate::command::subprocess::CommandRunner::force_pipes_for_tests();

    let env_key = format!("CARGO_BIN_EXE_{bin}");
    if let Ok(path) = std::env::var(&env_key) {
        return PathBuf::from(path);
    }

    let mut path = crate::workspace_root()
        .join("target")
        .join("debug")
        .join(bin);
    if cfg!(windows) {
        path.set_extension("exe");
    }
    if path.exists() {
        return path;
    }

    panic!(
        "{env_key} is unset and {} does not exist; \
         build the mock binaries first with `cargo build --workspace` \
         (or run tests via `cargo test --workspace`)",
        path.display()
    );
}

/// Returns a shell-quoted command string for the given mock binary.
///
/// The path is shell-quoted so that paths containing spaces (e.g.
/// `/Volumes/2TB Storage Vault/...`) are handled correctly when the string is
/// split by `shell_words::split` at runtime.
pub fn bin_command(bin: &str) -> String {
    shell_words::quote(&bin_path(bin).to_string_lossy()).to_string()
}

/// Extract a README fenced block: everything between the `marker` comment's
/// following `` ```<fence_lang> `` fence and the closing `` ``` `` fence, line
/// endings normalized to LF and trimmed.
pub fn extract_fenced_block(readme: &str, marker: &str, fence_lang: &str) -> Option<String> {
    let marker_end = readme.find(marker)?.checked_add(marker.len())?;
    let rest = &readme[marker_end..];
    let fence = format!("```{fence_lang}");
    let fence_start = rest.find(&fence)? + fence.len();
    let after_fence = &rest[fence_start..];
    let after_fence = line_ending::LineEnding::normalize(after_fence);
    let content = after_fence.strip_prefix(line_ending::LineEnding::LF.as_char())?;
    let close = content.find(&format!("{}```", line_ending::LineEnding::LF.as_str()))?;
    Some(content[..close].trim().to_string())
}
