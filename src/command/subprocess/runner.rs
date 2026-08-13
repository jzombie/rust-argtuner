use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

#[cfg(not(windows))]
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use command_group::{CommandGroup, GroupChild};

use super::talkback::{ParsedItem, parse_prefix_lines};
use crate::constants::{METRIC_NAMESPACE, MODEL_NAMESPACE, TUNER_NAMESPACE};

#[cfg(not(windows))]
static FORCE_PIPES: AtomicBool = AtomicBool::new(false);

/// Grace period allowed for the process group to die after a kill is issued
/// before the wait is abandoned and reported as an error.
const KILL_GRACE: Duration = Duration::from_secs(10);

/// Options that supervise a trial subprocess. Both fields are advisory: the
/// runner never clears `stop`, and a `timeout` is a hard deadline enforced on
/// the whole process group.
#[derive(Debug, Clone, Default)]
pub struct RunnerOptions {
    /// Hard deadline for the command. When it elapses the process group is
    /// killed and the result is marked `timed_out`.
    pub timeout: Option<Duration>,
    /// When set, the process group is killed as soon as the flag flips (e.g.
    /// Ctrl-C). Unset means the command runs to completion.
    pub stop: Option<Arc<AtomicBool>>,
}

#[derive(Debug)]
pub struct CommandOutput {
    pub stdout: String,
    pub _stderr: String,
    pub exit_code: i32,
    /// True when the process group was killed because `RunnerOptions.timeout`
    /// elapsed (as opposed to a normal exit or cancellation).
    pub timed_out: bool,
}

impl CommandOutput {
    pub fn parse_payload(&self, prefix: &str) -> Result<CommandResultPayload, String> {
        // Parse prefixed lines separately. We collect all `Event` items from any
        // matching line, but only use `Result` items from the last matching line
        // (this preserves the historical "last-result-line wins" semantics).
        let lines =
            parse_prefix_lines(&self.stdout, prefix).map_err(|e| format!("parse error: {e}"))?;
        let mut map = BTreeMap::new();
        let mut epoch_results = Vec::new();
        let mut step_results = Vec::new();
        let mut last_result_fields: Option<BTreeMap<String, String>> = None;
        let mut last_epoch_fields: Option<BTreeMap<String, String>> = None;

        // Collect events from any line. Expose the event name as a top-level
        // boolean-like key (e.g., `model.invalid_config=true`) and place any event fields
        // under `name.field` so callers can observe them via `payload_to_fields`.
        for items in &lines {
            let mut epoch_fields: Option<BTreeMap<String, String>> = None;
            for item in items {
                if let ParsedItem::Event { name, fields } = item {
                    map.insert(name.clone(), "true".to_string());
                    for (k, v) in fields {
                        map.insert(format!("{}.{}", name, k), v.clone());
                    }
                    if let Some(kind) = argtuner_common::EventKind::from_name(name) {
                        match kind {
                            argtuner_common::EventKind::EpochEnd => {
                                let mut entry = fields.clone();
                                entry.insert(name.clone(), "true".to_string());
                                epoch_fields = Some(entry);
                            }
                            argtuner_common::EventKind::StepEnd => {
                                let mut entry = fields.clone();
                                entry.insert(name.clone(), "true".to_string());
                                step_results.push(entry);
                            }
                            argtuner_common::EventKind::InvalidConfig => {
                                if let Some(err) = fields.get("error") {
                                    map.entry("error".to_string()).or_insert(err.clone());
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            if let Some(entry) = epoch_fields {
                last_epoch_fields = Some(entry.clone());
                epoch_results.push(entry);
            }
        }

        // Use only Result items from the last matching line, if any.
        if let Some(last_items) = lines.into_iter().rev().find(|items| {
            items
                .iter()
                .any(|it| matches!(it, ParsedItem::Result { .. }))
        }) {
            let mut entry = BTreeMap::new();
            for item in last_items {
                if let ParsedItem::Result { name, value } = item {
                    entry.insert(name, value);
                }
            }
            if !entry.is_empty() {
                last_result_fields = Some(entry);
            }
        }

        let final_fields = last_epoch_fields.or(last_result_fields);
        if let Some(fields) = final_fields {
            for (key, value) in fields {
                map.insert(key, value);
            }
        }

        Ok(CommandResultPayload {
            data: map,
            epoch_results,
            step_results,
        })
    }
}

pub struct CommandResultPayload {
    pub data: BTreeMap<String, String>,
    pub epoch_results: Vec<BTreeMap<String, String>>,
    pub step_results: Vec<BTreeMap<String, String>>,
}

impl CommandResultPayload {
    pub fn get_metric(&self, metric_key: &str) -> Result<f64, String> {
        let metric = self
            .data
            .get(metric_key)
            .ok_or_else(|| format!("result missing key '{metric_key}'"))?;
        let text = metric.trim();
        if text.eq_ignore_ascii_case("null") {
            return Ok(f64::NAN);
        }
        text.parse::<f64>()
            .map_err(|_| format!("result key '{metric_key}' not numeric"))
    }

    pub fn get_bool(&self, key: &str) -> bool {
        self.data
            .get(key)
            .and_then(|value| value.parse::<bool>().ok())
            .unwrap_or(false)
    }

    pub fn to_fields(&self) -> BTreeMap<String, String> {
        payload_fields_from(&self.data)
    }

    pub fn epoch_fields(&self) -> Vec<BTreeMap<String, String>> {
        self.epoch_results.iter().map(payload_fields_from).collect()
    }

    pub fn step_fields(&self) -> Vec<BTreeMap<String, String>> {
        self.step_results.iter().map(payload_fields_from).collect()
    }
}

fn payload_fields_from(data: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for (key, value) in data {
        if let Some((namespace, rest)) = key.split_once('.') {
            match namespace {
                TUNER_NAMESPACE => {
                    fields.insert(format!("{TUNER_NAMESPACE}.{rest}"), value.clone());
                }
                MODEL_NAMESPACE => {
                    fields.insert(format!("{MODEL_NAMESPACE}.{rest}"), value.clone());
                }
                METRIC_NAMESPACE => {
                    fields.insert(format!("{METRIC_NAMESPACE}.{rest}"), value.clone());
                }
                _ => {
                    fields.insert(format!("{METRIC_NAMESPACE}.{key}"), value.clone());
                }
            }
        } else {
            fields.insert(format!("{METRIC_NAMESPACE}.{key}"), value.clone());
        }
    }
    fields
}

pub struct CommandRunner;

/// Run a command with piped stdout/stderr (no PTY). POSIX pipes deliver all
/// bytes before EOF, so there is no PTY buffer-destruction race on child exit.
fn run_piped(
    command: &str,
    envs: &BTreeMap<String, String>,
    opts: &RunnerOptions,
) -> Result<CommandOutput, String> {
    use std::process::Stdio;

    let parts = split_command(command).map_err(|err| format!("command parse failed: {err}"))?;
    if parts.is_empty() {
        return Err("command is empty".to_string());
    }
    let cwd = std::env::current_dir().map_err(|err| format!("command cwd failed: {err}"))?;
    let mut cmd = std::process::Command::new(&parts[0]);
    cmd.current_dir(cwd)
        .args(&parts[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        cmd.env(key, value);
    }
    // Spawn into a dedicated process group so a timeout or cancellation can
    // terminate the whole tree; the group leader is the child itself.
    let mut child = cmd
        .group()
        .spawn()
        .map_err(|err| format!("command failed: {err}"))?;
    let child_stdout = child
        .inner()
        .stdout
        .take()
        .ok_or_else(|| "command stdout unavailable".to_string())?;
    let child_stderr = child
        .inner()
        .stderr
        .take()
        .ok_or_else(|| "command stderr unavailable".to_string())?;
    let stdout_handle = spawn_reader(child_stdout, false);
    let stderr_handle = spawn_reader(child_stderr, true);
    // Always join the reader threads first so their output has drained even
    // when the command was killed for a timeout or cancellation.
    let wait = wait_with_timeout(&mut child, opts);
    let stdout = stdout_handle
        .join()
        .map_err(|_| "stdout reader thread panicked".to_string())?;
    let stderr = stderr_handle
        .join()
        .map_err(|_| "stderr reader thread panicked".to_string())?;
    let (exit_code, timed_out) = wait?;
    Ok(CommandOutput {
        stdout,
        _stderr: stderr,
        exit_code,
        timed_out,
    })
}

/// A child the runner can poll and kill as a unit. Implemented for the piped
/// path's `GroupChild` and the PTY path's `Box<dyn Child>`.
trait RunnableChild {
    /// Reap the child if it has exited, returning its exit code.
    fn poll_exit(&mut self) -> Result<Option<i32>, String>;
    /// Terminate the child and its whole process group.
    fn kill_group(&mut self) -> Result<(), String>;
}

impl RunnableChild for GroupChild {
    fn poll_exit(&mut self) -> Result<Option<i32>, String> {
        match self.try_wait() {
            Ok(Some(status)) => Ok(Some(status.code().unwrap_or(-1))),
            Ok(None) => Ok(None),
            Err(err) => Err(format!("command wait failed: {err}")),
        }
    }
    fn kill_group(&mut self) -> Result<(), String> {
        self.kill()
            .map_err(|err| format!("command kill failed: {err}"))
    }
}

#[cfg(not(windows))]
impl RunnableChild for Box<dyn portable_pty::Child + Send + Sync> {
    fn poll_exit(&mut self) -> Result<Option<i32>, String> {
        match self.try_wait() {
            Ok(Some(status)) => Ok(Some(status.exit_code() as i32)),
            Ok(None) => Ok(None),
            Err(err) => Err(format!("command wait failed: {err}")),
        }
    }
    fn kill_group(&mut self) -> Result<(), String> {
        // portable-pty spawns the child as a session leader (setsid), so its
        // pid doubles as the process-group id; signal the whole group. We only
        // ever call this while `poll_exit` has reported the child still alive,
        // so the pgid cannot have been recycled yet.
        if let Some(pid) = self.process_id() {
            unsafe {
                libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            }
            Ok(())
        } else {
            self.kill()
                .map_err(|err| format!("command kill failed: {err}"))
        }
    }
}

/// Wait for `child` to exit, killing its process group when `opts.timeout`
/// elapses or `opts.stop` flips. Returns `(exit_code, timed_out)`.
fn wait_with_timeout<C: RunnableChild>(
    child: &mut C,
    opts: &RunnerOptions,
) -> Result<(i32, bool), String> {
    let start = Instant::now();
    let mut kill_issued_at: Option<Instant> = None;
    let mut timed_out = false;
    loop {
        if let Some(exit_code) = child.poll_exit()? {
            return Ok((exit_code, timed_out));
        }
        if let Some(since) = kill_issued_at {
            if since.elapsed() >= KILL_GRACE {
                return Err("command did not terminate after kill signal".to_string());
            }
        } else {
            let timeout_hit = opts.timeout.is_some_and(|t| start.elapsed() >= t);
            let cancelled = opts
                .stop
                .as_ref()
                .is_some_and(|s| s.load(Ordering::Relaxed));
            if timeout_hit || cancelled {
                timed_out = timeout_hit;
                child.kill_group()?;
                kill_issued_at = Some(Instant::now());
                continue;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
}

impl CommandRunner {
    pub fn run(command: &str, envs: &BTreeMap<String, String>) -> Result<CommandOutput, String> {
        Self::run_with_options(command, envs, RunnerOptions::default())
    }

    /// Run `command` with per-invocation supervision options (timeout, stop).
    pub fn run_with_options(
        command: &str,
        envs: &BTreeMap<String, String>,
        opts: RunnerOptions,
    ) -> Result<CommandOutput, String> {
        #[cfg(windows)]
        {
            run_piped(command, envs, &opts)
        }
        #[cfg(not(windows))]
        {
            if FORCE_PIPES.load(Ordering::Relaxed)
                || envs.contains_key(argtuner_common::FORCE_PIPES_ENV)
                || std::env::var_os(argtuner_common::FORCE_PIPES_ENV).is_some()
            {
                run_piped(command, envs, &opts)
            } else {
                run_pty(command, envs, &opts)
            }
        }
    }

    /// Test-only: force all subprocesses in this process to use piped stdio
    /// instead of a PTY (avoids the macOS PTY buffer-destruction race on child
    /// exit). Idempotent; safe to call from any test thread. No-op on Windows,
    /// where subprocesses always run over pipes.
    #[cfg(windows)]
    #[doc(hidden)]
    pub fn force_pipes_for_tests() {}

    #[cfg(not(windows))]
    #[doc(hidden)]
    pub fn force_pipes_for_tests() {
        FORCE_PIPES.store(true, Ordering::Relaxed);
    }
}

/// Run a command attached to a fresh PTY. The child becomes a session leader
/// (portable-pty calls setsid), so the process group is killable as a unit.
#[cfg(not(windows))]
fn run_pty(
    command: &str,
    envs: &BTreeMap<String, String>,
    opts: &RunnerOptions,
) -> Result<CommandOutput, String> {
    let parts = split_command(command).map_err(|err| format!("command parse failed: {err}"))?;
    if parts.is_empty() {
        return Err("command is empty".to_string());
    }
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| format!("pty open failed: {err}"))?;
    let mut cmd = CommandBuilder::new(&parts[0]);
    let cwd = std::env::current_dir().map_err(|err| format!("command cwd failed: {err}"))?;
    cmd.cwd(cwd);
    if parts.len() > 1 {
        cmd.args(&parts[1..]);
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|err| format!("command failed: {err}"))?;
    drop(pair.slave);
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|err| format!("pty reader failed: {err}"))?;
    #[cfg(unix)]
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|err| format!("pty writer failed: {err}"))?;
    let output = spawn_reader(reader, false);

    #[cfg(unix)]
    let input_guard = {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let handle = thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let fd = stdin.as_raw_fd();
            let mut buf = [0u8; 1024];
            loop {
                if stop_for_thread.load(Ordering::Relaxed) {
                    break;
                }
                let mut fds = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = unsafe { libc::poll(&mut fds, 1, 100) };
                if ready < 0 {
                    break;
                }
                if ready == 0 {
                    continue;
                }
                if (fds.revents & libc::POLLIN) == 0 {
                    continue;
                }
                let read = stdin.read(&mut buf).unwrap_or(0);
                if read == 0 {
                    break;
                }
                if writer.write_all(&buf[..read]).is_err() {
                    break;
                }
                let _ = writer.flush();
            }
        });
        Some(InputGuard {
            handle: Some(handle),
            stop: Some(stop),
        })
    };

    #[cfg(not(any(unix, windows)))]
    let input_guard: Option<InputGuard> = None;
    let wait = wait_with_timeout(&mut child, opts);
    let stdout = output
        .join()
        .map_err(|_| "pty reader thread panicked".to_string())?;
    if let Some(mut guard) = input_guard {
        guard.stop();
    }
    let (exit_code, timed_out) = wait?;
    Ok(CommandOutput {
        stdout,
        _stderr: String::new(),
        exit_code,
        timed_out,
    })
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    to_stderr: bool,
) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut out = String::new();
        if to_stderr {
            let mut stderr = std::io::stderr();
            loop {
                let read = reader.read(&mut buf).unwrap_or(0);
                if read == 0 {
                    break;
                }
                let chunk = String::from_utf8_lossy(&buf[..read]);
                let _ = stderr.write_all(chunk.as_bytes());
                let _ = stderr.flush();
                out.push_str(&chunk);
            }
        } else {
            let mut stdout = std::io::stdout();
            loop {
                let read = reader.read(&mut buf).unwrap_or(0);
                if read == 0 {
                    break;
                }
                let chunk = String::from_utf8_lossy(&buf[..read]);
                let _ = stdout.write_all(chunk.as_bytes());
                let _ = stdout.flush();
                out.push_str(&chunk);
            }
        }
        out
    })
}

fn split_command(command: &str) -> Result<Vec<String>, String> {
    #[cfg(windows)]
    {
        split_command_windows(command)
    }
    #[cfg(not(windows))]
    {
        shell_words::split(command).map_err(|err| err.to_string())
    }
}

#[cfg(not(windows))]
struct InputGuard {
    handle: Option<thread::JoinHandle<()>>,
    stop: Option<Arc<AtomicBool>>,
}

#[cfg(windows)]
fn split_command_windows(command: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut in_quotes: Option<char> = None;
    while let Some(ch) = chars.next() {
        if let Some(q) = in_quotes {
            if ch == q {
                in_quotes = None;
            } else {
                current.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            in_quotes = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                args.push(current);
                current = String::new();
            }
            while matches!(chars.peek(), Some(next) if next.is_whitespace()) {
                chars.next();
            }
        } else {
            current.push(ch);
        }
    }
    if in_quotes.is_some() {
        return Err("unterminated quote".to_string());
    }
    if !current.is_empty() {
        args.push(current);
    }
    Ok(args)
}

#[cfg(not(windows))]
impl InputGuard {
    fn stop(&mut self) {
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_result_payload_uses_last_matching_line() {
        let event1 = serde_json::json!({"type":"event","name":"model.epoch_end","fields":{"metric":"0.5","epoch":"1"}});
        let event2 = serde_json::json!({"type":"event","name":"model.epoch_end","fields":{"aux":"123","metric":"0.9","epoch":"2"}});
        let event3 = serde_json::json!({"type":"event","name":"model.epoch_end","fields":{"metric":"0.2","epoch":"3"}});

        let output_str = format!(
            "info: start\n{}{}\n{}{}\ninfo: still running\n{}{}\ndone\n",
            crate::RESULT_PREFIX,
            event1,
            crate::RESULT_PREFIX,
            event2,
            crate::RESULT_PREFIX,
            event3
        );
        let output = CommandOutput {
            stdout: output_str,
            _stderr: String::new(),
            exit_code: 0,
            timed_out: false,
        };
        let payload = output.parse_payload(crate::RESULT_PREFIX).expect("payload");
        let metric = payload.get_metric("metric").expect("metric");
        assert_eq!(metric, 0.2);
        assert!(!payload.data.contains_key("aux"));
    }

    #[test]
    fn run_piped_completes_without_options() {
        CommandRunner::force_pipes_for_tests();
        let envs = BTreeMap::from([(
            crate::test_support::SELF_ROLE_ENV.to_string(),
            "noop".to_string(),
        )]);
        let output =
            CommandRunner::run(&crate::test_support::self_invoking_command(), &envs).expect("run");
        assert_eq!(output.exit_code, 0);
        assert!(!output.timed_out);
    }

    #[test]
    fn run_times_out_and_kills_process_group() {
        CommandRunner::force_pipes_for_tests();
        let envs = BTreeMap::from([(
            crate::test_support::SELF_ROLE_ENV.to_string(),
            "sleepy".to_string(),
        )]);
        let start = Instant::now();
        let output = CommandRunner::run_with_options(
            &crate::test_support::self_invoking_command(),
            &envs,
            RunnerOptions {
                timeout: Some(Duration::from_secs(2)),
                stop: None,
            },
        )
        .expect("run");
        assert!(output.timed_out, "should be marked as timed out");
        assert_ne!(output.exit_code, 0, "killed process has non-zero status");
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "should not wait out the full 100s sleep"
        );
    }

    #[test]
    fn run_stop_flag_kills_process_group_without_timeout() {
        CommandRunner::force_pipes_for_tests();
        let envs = BTreeMap::from([(
            crate::test_support::SELF_ROLE_ENV.to_string(),
            "sleepy".to_string(),
        )]);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_worker = stop.clone();
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            stop_for_worker.store(true, Ordering::Relaxed);
        });
        let output = CommandRunner::run_with_options(
            &crate::test_support::self_invoking_command(),
            &envs,
            RunnerOptions {
                timeout: None,
                stop: Some(stop),
            },
        )
        .expect("run");
        worker.join().expect("worker");
        assert!(!output.timed_out, "cancellation is not a timeout");
        assert_ne!(output.exit_code, 0, "killed process has non-zero status");
    }

    #[test]
    fn run_timeout_kills_grandchild_processes() {
        CommandRunner::force_pipes_for_tests();
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_path = dir.path().join("grandchild.pid");
        let heartbeat_path = dir.path().join("grandchild.heartbeat");
        let envs = BTreeMap::from([
            (
                crate::test_support::SELF_ROLE_ENV.to_string(),
                "child".to_string(),
            ),
            (
                crate::test_support::SELF_PID_FILE_ENV.to_string(),
                pid_path.to_string_lossy().into_owned(),
            ),
            (
                crate::test_support::SELF_HEARTBEAT_ENV.to_string(),
                heartbeat_path.to_string_lossy().into_owned(),
            ),
        ]);
        // The child role spawns a grandchild and waits for it; the timeout
        // kills the whole process group, so the grandchild must stop too.
        let output = CommandRunner::run_with_options(
            &crate::test_support::self_invoking_command(),
            &envs,
            RunnerOptions {
                timeout: Some(Duration::from_secs(2)),
                stop: None,
            },
        )
        .expect("run");
        assert!(output.timed_out, "should be marked as timed out");
        let pid_text = std::fs::read_to_string(&pid_path).unwrap_or_else(|err| {
            panic!(
                "grandchild failed to write PID before timeout: {err} (stdout: {})",
                output.stdout
            )
        });
        let pid: u32 = pid_text.trim().parse().unwrap_or_else(|err| {
            panic!("invalid grandchild PID in {pid_path:?}: {pid_text:?}: {err}")
        });
        crate::test_support::assert_no_longer_running(
            pid,
            &heartbeat_path,
            Duration::from_millis(600),
        );
    }
}
