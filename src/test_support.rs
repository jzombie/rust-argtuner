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

/// Env var consumed by [`self_invoking_helper`] selecting its role.
pub const SELF_ROLE_ENV: &str = "ARGTUNER_SELF_ROLE";

/// Env var carrying the path where a `grandchild`-role helper writes its pid.
pub const SELF_PID_FILE_ENV: &str = "ARGTUNER_SELF_PID_FILE";

/// Env var carrying the path where a `grandchild`-role helper writes a
/// heartbeat while it is running (its liveness signal).
pub const SELF_HEARTBEAT_ENV: &str = "ARGTUNER_SELF_HEARTBEAT";

/// libtest filter that runs exactly [`self_invoking_helper`].
const SELF_TEST_FILTER: &str = "test_support::self_invoking_helper";

/// A command that re-executes the current test binary, filtered with `--exact`
/// so the subprocess runs [`self_invoking_helper`] instead of the whole suite.
/// The role is supplied separately through [`SELF_ROLE_ENV`] (e.g. by placing
/// it in the runner's envs).
pub fn self_invoking_command() -> String {
    let exe = std::env::current_exe().expect("test binary path");
    #[cfg(windows)]
    {
        // `split_command_windows` tokenizes with Windows rules and does not
        // understand POSIX single quotes, so use double quotes on Windows.
        format!("\"{}\" --exact {SELF_TEST_FILTER}", exe.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        format!(
            "{} --exact {SELF_TEST_FILTER}",
            shell_words::quote(&exe.to_string_lossy())
        )
    }
}

/// Re-executable cross-platform helper for subprocess tests. Running it
/// directly under `cargo test` (no role) is a no-op pass. Spawned with
/// [`SELF_ROLE_ENV`] set it plays one role:
/// - `noop`: exits 0 immediately.
/// - `sleepy`: sleeps 100s (timeout / cancellation target).
/// - `grandchild`: writes its pid to [`SELF_PID_FILE_ENV`], then advances a
///   heartbeat in [`SELF_HEARTBEAT_ENV`] until killed (liveness signal).
/// - `child`: spawns a `grandchild` and waits for it (group-kill target).
#[test]
pub fn self_invoking_helper() {
    match std::env::var(SELF_ROLE_ENV).as_deref() {
        Ok("noop") => {}
        Ok("sleepy") => std::thread::sleep(std::time::Duration::from_secs(100)),
        Ok("grandchild") => {
            if let Ok(pid_file) = std::env::var(SELF_PID_FILE_ENV) {
                let _ = std::fs::write(&pid_file, std::process::id().to_string());
            }
            if let Ok(heartbeat_file) = std::env::var(SELF_HEARTBEAT_ENV) {
                use std::io::Write;
                if let Ok(mut file) = std::fs::File::create(&heartbeat_file) {
                    let mut counter: u64 = 0;
                    loop {
                        let _ = file.write_all(format!("{counter:020}\n").as_bytes());
                        // Flush so each heartbeat reaches the OS before any
                        // signal termination could race the write.
                        let _ = file.flush();
                        counter = counter.wrapping_add(1);
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(100));
        }
        Ok("child") => {
            let exe = std::env::current_exe().expect("test binary path");
            let mut cmd = std::process::Command::new(exe);
            cmd.args(["--exact", SELF_TEST_FILTER])
                .env(SELF_ROLE_ENV, "grandchild")
                .env(
                    SELF_PID_FILE_ENV,
                    std::env::var(SELF_PID_FILE_ENV).unwrap_or_default(),
                )
                .env(
                    SELF_HEARTBEAT_ENV,
                    std::env::var(SELF_HEARTBEAT_ENV).unwrap_or_default(),
                );
            let mut child = cmd.spawn().expect("spawn grandchild");
            let _ = child.wait();
        }
        _ => {}
    }
}

/// Assert that the process identified by `pid` is no longer running by
/// checking that its heartbeat file stopped advancing for at least `grace`.
///
/// A heartbeat rather than `kill(pid, 0)`: on Unix a terminated-but-unreaped
/// zombie still answers `kill(pid, 0)` successfully, but it cannot write a
/// file, so a frozen heartbeat proves the process is not executing. Works
/// identically on Windows with no handle API.
pub fn assert_no_longer_running(
    pid: u32,
    heartbeat_file: &std::path::Path,
    grace: std::time::Duration,
) {
    let read = || std::fs::read(heartbeat_file).unwrap_or_default();
    let before = read();
    std::thread::sleep(grace);
    let after = read();
    assert_eq!(
        before, after,
        "process {pid} is still running: its heartbeat advanced after the group kill"
    );
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
