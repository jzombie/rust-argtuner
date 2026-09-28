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

use super::ipc::{ParsedItem, parse_prefix_lines};
use crate::constants::{METRIC_NAMESPACE, MODEL_NAMESPACE, TUNER_NAMESPACE};

#[cfg(not(windows))]
static FORCE_PIPES: AtomicBool = AtomicBool::new(false);

/// Grace period allowed for the process group to die after a kill is issued
/// before the wait is abandoned and reported as an error.
const KILL_GRACE: Duration = Duration::from_secs(10);

/// Callback invoked once per complete `\n`-terminated child-stdout line while
/// the subprocess runs. Used to stream-parse `::ARGTUNER::` protocol events
/// (live TUI updates) without waiting for child exit.
///
/// Delivery guarantee: lines carrying the protocol prefix always arrive
/// whole, however the reads split them (eager echo holds back while the
/// prefix is in flight). Long *non-protocol* lines (progress bars) may
/// arrive as the tail only — their head was already echoed to the terminal
/// eagerly and is not replayed here. The post-exit `CommandOutput.stdout`
/// always carries the complete raw stream regardless.
pub type LineCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// Options that supervise a trial subprocess. All fields are advisory, except
/// `timeout` (a hard deadline enforced on the whole process group) and `stop`
/// (the runner never clears it).
#[derive(Clone, Default)]
pub struct RunnerOptions {
    /// Hard deadline for the command. When it elapses the process group is
    /// killed and the result is marked `timed_out`.
    pub timeout: Option<Duration>,
    /// When set, the process group is killed as soon as the flag flips (e.g.
    /// Ctrl-C). Unset means the command runs to completion.
    pub stop: Option<Arc<AtomicBool>>,
    /// When set, invoked once per complete `\n`-terminated stdout line as it
    /// arrives (a trailing unterminated fragment is delivered once at EOF).
    /// Lines split across pipe/PTY read chunks are reassembled first.
    pub on_line: Option<LineCallback>,
    /// When true, stdout lines containing the `::ARGTUNER::` protocol prefix
    /// are still accumulated into `CommandOutput.stdout` (so post-exit parsing
    /// is unaffected) but are not echoed to the parent terminal. Progress-bar
    /// fragments glued onto the same line (a `\r` update with no newline) are
    /// suppressed along with it; the next bar update redraws the line.
    pub suppress_protocol_echo: bool,
}

impl std::fmt::Debug for RunnerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunnerOptions")
            .field("timeout", &self.timeout)
            .field("stop", &self.stop)
            .field("on_line", &self.on_line.as_ref().map(|_| "LineCallback"))
            .field("suppress_protocol_echo", &self.suppress_protocol_echo)
            .finish()
    }
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

pub(crate) fn payload_fields_from(data: &BTreeMap<String, String>) -> BTreeMap<String, String> {
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
    let stdout_handle = spawn_reader(
        child_stdout,
        false,
        opts.on_line.clone(),
        opts.suppress_protocol_echo,
    );
    let stderr_handle = spawn_reader(child_stderr, true, None, false);
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
    let output = spawn_reader(
        reader,
        false,
        opts.on_line.clone(),
        opts.suppress_protocol_echo,
    );

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

/// Byte count of `pending` safe to echo eagerly (see the call site).
/// Returns 0 while the buffer contains a protocol line, and always holds
/// back the trailing bytes: they could be the head of a prefix split across
/// read chunks, and a split prefix must reach the callback intact.
/// (`hold` is `len(prefix) - 1`, which covers every completable split: any
/// split head is shorter than the prefix itself.) The returned index is
/// always a char boundary — slicing a multibyte UTF-8 sequence would panic
/// in `drain`, and barnetext may be non-ASCII on any platform.
fn eager_echo_len(pending: &str, prefix: &str) -> usize {
    if pending.contains(prefix) {
        return 0;
    }
    let hold = prefix.len().saturating_sub(1);
    if pending.len() <= hold {
        return 0;
    }
    let mut end = pending.len() - hold;
    while !pending.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Upper bound on unparsed buffered stdout before it is dropped with a
/// warning. Genuine protocol lines are single short `println!`s, so anything
/// this large without a newline is a runaway writer, not an event. Only
/// reachable when the buffer contains protocol-looking content (prefix-free
/// output is eagerly echoed away each chunk); any event spanning the dropped
/// bytes is lost, and the warning says so.
const MAX_PENDING_BYTES: usize = 1 << 20;

/// Drop an overgrown reassembly buffer, warning loudly. Returns true when
/// anything was dropped. Pure enough to unit-test (the warning goes to
/// stderr, which libtest captures).
fn enforce_pending_cap(pending: &mut String) -> bool {
    if pending.len() > MAX_PENDING_BYTES {
        eprintln!(
            "warning: dropping {} bytes of newline-free subprocess output \
             (exceeds {MAX_PENDING_BYTES} bytes); any protocol event spanning \
             the dropped bytes is lost",
            pending.len()
        );
        pending.clear();
        true
    } else {
        false
    }
}

/// Fit an echo fragment to the live viewport so in-place redraws never wrap:
/// a redraw wider than the terminal wraps to two rows, and the next erase
/// (`\x1b[2K\r`) clears only one — heads of lines appear truncated. Only
/// fragments carrying `\r` (in-place updates) are ever cut; plain
/// `\n`-terminated lines wrap harmlessly and stay whole (they may be the
/// only record: info lines live on the terminal, not in the DB). Pure
/// display: stored output and the `on_line` callback keep every byte.
/// Untouched when our stdout is not a terminal (piped/logged: the echo IS
/// the log) or the width is unknown. Re-queried per write, so resizes
/// apply immediately with no contracts and no child cooperation.
fn fit_echo(fragment: &str, live_cols: Option<u16>) -> &str {
    if !fragment.contains('\r') {
        return fragment;
    }
    let max = match live_cols {
        Some(cols) if cols > 0 => (cols as usize).saturating_sub(1),
        _ => return fragment,
    };
    if fragment.chars().count() <= max {
        return fragment;
    }
    let end = fragment
        .char_indices()
        .nth(max)
        .map(|(i, _)| i)
        .unwrap_or(fragment.len());
    &fragment[..end]
}

/// Live viewport width for echo fitting: our own terminal columns, or
/// `None` when piped/logged (echo must stay whole) or unknown.
fn live_echo_cols() -> Option<u16> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return None;
    }
    crossterm::terminal::size().ok().map(|(cols, _)| cols)
}

/// Pumps one complete stdout line to the terminal echo and the live
/// callback (see `spawn_reader`).
fn pump_line(
    line: &str,
    stdout: &mut std::io::Stdout,
    on_line: &Option<LineCallback>,
    suppress_protocol_echo: bool,
) {
    if suppress_protocol_echo {
        match line.find(crate::RESULT_PREFIX) {
            // Echo any human-readable fragment glued before the
            // protocol payload (e.g. a `\r` progress-bar update)
            // so bars keep animating; the protocol JSON itself
            // stays out of the terminal. No trailing newline:
            // the next bar update overwrites this one in place.
            // (Heads of such lines may already have been echoed eagerly;
            // re-emitting the fragment is a harmless in-place redraw.)
            Some(idx) => {
                let pre = &line[..idx];
                if !pre.is_empty() {
                    let _ = stdout.write_all(fit_echo(pre, live_echo_cols()).as_bytes());
                }
            }
            None => {
                let fitted = fit_echo(line, live_echo_cols());
                let _ = stdout.write_all(fitted.as_bytes());
                let _ = stdout.write_all(b"\n");
            }
        }
    } else {
        let fitted = fit_echo(line, live_echo_cols());
        let _ = stdout.write_all(fitted.as_bytes());
        let _ = stdout.write_all(b"\n");
    }
    if let Some(cb) = on_line.as_ref() {
        cb(line);
    }
    let _ = stdout.flush();
}

/// Decode freshly-read bytes incrementally: a multibyte UTF-8 sequence may
/// straddle two reads, and decoding each read with `from_utf8_lossy` would
/// emit U+FFFD for each half — corrupting non-ASCII bar text and, worse, any
/// split multibyte inside protocol JSON (a parse error that drops the whole
/// event). A truncated trailing sequence is retained in `carry` for the next
/// read; genuinely invalid bytes still become U+FFFD.
fn decode_chunk(carry: &mut Vec<u8>, fresh: &[u8]) -> String {
    carry.extend_from_slice(fresh);
    let mut chunk = String::new();
    let mut idx = 0;
    while idx < carry.len() {
        match std::str::from_utf8(&carry[idx..]) {
            Ok(valid) => {
                chunk.push_str(valid);
                idx = carry.len();
            }
            Err(e) => {
                let up_to = e.valid_up_to();
                chunk
                    .push_str(std::str::from_utf8(&carry[idx..idx + up_to]).expect("valid prefix"));
                match e.error_len() {
                    Some(len) => {
                        chunk.push('\u{FFFD}');
                        idx += up_to + len;
                    }
                    None => {
                        idx += up_to;
                        break;
                    }
                }
            }
        }
    }
    carry.drain(..idx);
    chunk
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    to_stderr: bool,
    on_line: Option<LineCallback>,
    suppress_protocol_echo: bool,
) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut out = String::new();
        if to_stderr {
            let mut stderr = std::io::stderr();
            let mut carry = Vec::new();
            loop {
                let read = reader.read(&mut buf).unwrap_or(0);
                if read == 0 {
                    break;
                }
                let chunk = decode_chunk(&mut carry, &buf[..read]);
                let _ = stderr.write_all(chunk.as_bytes());
                let _ = stderr.flush();
                out.push_str(&chunk);
            }
            if !carry.is_empty() {
                let tail = String::from_utf8_lossy(&carry);
                let _ = stderr.write_all(tail.as_bytes());
                let _ = stderr.flush();
                out.push_str(&tail);
            }
        } else {
            let mut stdout = std::io::stdout();
            // `pending` reassembles lines split across read chunks (and holds
            // `\r`-terminated progress-bar fragments until the next `\n`).
            // `out` keeps the raw byte stream exactly as before.
            let mut pending = String::new();
            let mut carry = Vec::new();
            loop {
                let read = reader.read(&mut buf).unwrap_or(0);
                if read == 0 {
                    break;
                }
                let chunk = decode_chunk(&mut carry, &buf[..read]);
                out.push_str(&chunk);
                pending.push_str(&chunk);
                while let Some(pos) = pending.find('\n') {
                    let line: String = pending.drain(..=pos).collect();
                    pump_line(
                        line.strip_suffix('\n').unwrap_or(&line),
                        &mut stdout,
                        &on_line,
                        suppress_protocol_echo,
                    );
                }
                // Eagerly echo output that hasn't formed a line yet (e.g. `\r`
                // progress-bar updates during long stretches with no `\n`,
                // such as grad-accum windows of hundreds of micro-batches or
                // long eval phases). See `eager_echo_len` for the safety
                // rules (protocol hold-back, char boundaries).
                let echo_up_to = eager_echo_len(&pending, crate::RESULT_PREFIX);
                if echo_up_to > 0 {
                    let fragment: String = pending.drain(..echo_up_to).collect();
                    let fitted = fit_echo(&fragment, live_echo_cols());
                    let _ = stdout.write_all(fitted.as_bytes());
                    let _ = stdout.flush();
                }
                // Belt-and-braces bound (see `enforce_pending_cap`): with
                // eager echo active this only trips on pathological
                // newline-free output containing protocol-looking text.
                enforce_pending_cap(&mut pending);
            }
            // EOF with a retained partial sequence (child died mid-char):
            // emit lossily rather than dropping bytes.
            if !carry.is_empty() {
                let tail = String::from_utf8_lossy(&carry);
                out.push_str(&tail);
                pending.push_str(&tail);
            }
            if !pending.is_empty() {
                pump_line(&pending, &mut stdout, &on_line, suppress_protocol_echo);
            }
        }
        out
    })
}

pub(crate) fn split_command(command: &str) -> Result<Vec<String>, String> {
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
                ..Default::default()
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
                ..Default::default()
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
                ..Default::default()
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

    #[test]
    fn pending_cap_drops_runaway_buffers_with_warning() {
        let mut small = String::from("progress fragment");
        assert!(!enforce_pending_cap(&mut small));
        assert_eq!(small, "progress fragment");
        let mut huge = "x".repeat((1 << 20) + 1);
        assert!(enforce_pending_cap(&mut huge));
        assert!(huge.is_empty());
    }

    #[test]
    fn decode_chunk_retains_split_multibyte_sequences() {
        let approx = "≈".as_bytes(); // E2 89 88
        let mut carry = Vec::new();
        assert_eq!(decode_chunk(&mut carry, &approx[..2]), "");
        assert_eq!(carry, approx[..2]);
        assert_eq!(decode_chunk(&mut carry, &approx[2..]), "≈");
        assert!(carry.is_empty());
    }

    #[test]
    fn fit_echo_cuts_only_carriage_return_lines_to_live_width() {
        // No `\r`: untouched at any width — plain lines wrap harmlessly
        // whole and may be the only record.
        let plain = "model_dim=768 layers=6 heads=8 out_dim=768 max_seq_len=512";
        assert_eq!(fit_echo(plain, Some(80)), plain);
        assert_eq!(fit_echo(plain, None), plain);
        // `\r` redraw fitting in width: untouched.
        let bar = "\x1b[2K\rok step 1/22";
        assert_eq!(fit_echo(bar, Some(80)), bar);
        // Over-wide redraw: cut to width-1, head escapes intact.
        let long_bar = format!("\x1b[2K\r{}", "x".repeat(100));
        let fitted = fit_echo(&long_bar, Some(80));
        assert_eq!(fitted.chars().count(), 79);
        assert!(fitted.starts_with("\x1b[2K\r"));
        // Multibyte near the cut: never split a char.
        let uni = format!("\r{}", "é".repeat(50));
        let fitted_uni = fit_echo(&uni, Some(10));
        assert_eq!(fitted_uni.chars().count(), 9);
        assert!(fitted_uni.ends_with('é'));
        // Unknown, zero, or non-terminal width: untouched.
        assert_eq!(fit_echo(&long_bar, None), long_bar.as_str());
        assert_eq!(fit_echo(&long_bar, Some(0)), long_bar.as_str());
    }

    #[test]
    fn long_lines_survive_chunked_reads_and_parse_cleanly() {
        use super::super::ipc::{ParsedItem, parse_prefix_lines};
        use std::io::Read;
        use std::sync::Mutex;

        /// Reader yielding at most `chunk` bytes per call, splitting lines
        /// (and multibyte sequences) mid-flight the way OS pipes do.
        struct ChunkReader {
            data: Vec<u8>,
            pos: usize,
            chunk: usize,
        }
        impl Read for ChunkReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.pos >= self.data.len() {
                    return Ok(0);
                }
                let n = self.chunk.min(buf.len()).min(self.data.len() - self.pos);
                buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                Ok(n)
            }
        }

        // A long in-place bar: ANSI wipe + `\r` redraws, non-ASCII tail, no
        // newline until the end — the exact soup that used to eat line heads.
        let mut bar = String::from("\x1b[2K\rtrain accum[1/88] step 0/22 dropout=0.000 bytes≈100");
        for i in 2..=88u32 {
            bar.push_str(&format!(
                "\rtrain accum[{i}/88] step 0/22 dropout=0.000 bytes≈100"
            ));
        }
        // A long protocol line: epoch event with a wide metric map.
        let mut fields = String::new();
        for i in 0..200u32 {
            fields.push_str(&format!(r#""metric.m{i}":"{i}","#));
        }
        fields.pop();
        let proto = format!(r#"{{"type":"event","name":"model.epoch_end","fields":{{{fields}}}}}"#);
        let proto = format!("{}{proto}", crate::RESULT_PREFIX);
        let soup = format!("info: start\n{bar}\n{proto}\ninfo: done\n");

        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_cb = seen.clone();
        let callback: LineCallback = Arc::new(move |line: &str| {
            seen_cb.lock().unwrap().push(line.to_string());
        });
        let reader = ChunkReader {
            data: soup.as_bytes().to_vec(),
            pos: 0,
            chunk: 7,
        };
        let handle = spawn_reader(reader, false, Some(callback), true);
        let out = handle.join().unwrap();

        // Raw stream round-trips byte-exactly, however the reads split it.
        assert_eq!(out, soup);
        // Callback delivery: short lines whole; the long protocol line
        // whole (the guarantee the live parser relies on); the long
        // non-protocol bar as its tail (its head went to terminal echo).
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert_eq!(seen[0], "info: start");
        assert!(bar.ends_with(&seen[1]), "bar tail: {}", seen[1]);
        assert_eq!(seen[2], proto);
        assert_eq!(seen[3], "info: done");
        drop(seen);
        // And the long protocol line parses without errors while the bar
        // line yields no items (also without errors).
        let parsed = parse_prefix_lines(&out, crate::RESULT_PREFIX).unwrap();
        assert_eq!(parsed.len(), 1, "parsed: {parsed:?}");
        assert!(
            parsed[0].iter().any(|item| matches!(
                item,
                ParsedItem::Event { name, .. } if name == "model.epoch_end"
            )),
            "parsed: {:?}",
            parsed[0]
        );
    }

    #[test]
    fn eager_echo_holds_short_buffers_and_protocol_lines() {
        // Too short to hold anything back: nothing echoable.
        assert_eq!(eager_echo_len("", crate::RESULT_PREFIX), 0);
        assert_eq!(eager_echo_len("short", crate::RESULT_PREFIX), 0);
        // A complete protocol line anywhere suppresses eager echo.
        let with_prefix = format!("bar {} {{}}", crate::RESULT_PREFIX);
        assert_eq!(eager_echo_len(&with_prefix, crate::RESULT_PREFIX), 0);
    }

    #[test]
    fn eager_echo_holds_prefix_head_split_across_chunks() {
        // Trailing partial prefix ("::ARGT", 6 of 12 bytes) must survive for
        // reassembly: only bytes before the 11-byte holdback window echo.
        let pending = "xxxxxx::ARGT";
        assert_eq!(pending.len(), 12);
        assert_eq!(eager_echo_len(pending, crate::RESULT_PREFIX), 1);
    }

    #[test]
    fn eager_echo_never_splits_a_multibyte_char() {
        // "X" + é (2 bytes) + 9 ASCII = 12 bytes; len - hold lands on é's
        // second byte, which would panic `drain`. Must floor to 1.
        let pending = "XéYYYYYYYYY";
        assert_eq!(pending.len(), 12);
        let n = eager_echo_len(pending, crate::RESULT_PREFIX);
        assert_eq!(n, 1);
        assert!(pending.is_char_boundary(n));
        // Pure-ASCII equivalent echoes the full window.
        assert_eq!(eager_echo_len("XAYYYYYYYYYY", crate::RESULT_PREFIX), 1);
    }

    #[test]
    fn on_line_receives_complete_lines_and_stdout_stays_intact() {
        CommandRunner::force_pipes_for_tests();
        // The mock only emits protocol lines when it sees the tuning marker.
        let envs = BTreeMap::from([(
            argtuner_common::TUNING_MARKER_ENV.to_string(),
            argtuner_common::TUNING_MARKER_VALUE.to_string(),
        )]);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_cb = seen.clone();
        let output = CommandRunner::run_with_options(
            &crate::test_support::bin_command("mock_emit_result"),
            &envs,
            RunnerOptions {
                timeout: None,
                stop: None,
                on_line: Some(std::sync::Arc::new(move |line: &str| {
                    seen_cb.lock().unwrap().push(line.to_string());
                })),
                suppress_protocol_echo: true,
            },
        )
        .expect("run");
        assert_eq!(output.exit_code, 0);
        // Accumulation is unaffected by echo suppression or the callback.
        let payload = output.parse_payload(crate::RESULT_PREFIX).expect("payload");
        assert_eq!(payload.get_metric("metric").expect("metric"), 0.42);
        // Every delivered line is complete (reassembled across read chunks,
        // no embedded newlines) and the protocol event arrived intact.
        let seen = seen.lock().unwrap();
        assert!(
            seen.iter()
                .any(|l| l.contains(argtuner_common::MODEL_EPOCH_END_EVENT)),
            "callback must observe the epoch_end line: {seen:?}"
        );
        assert!(seen.iter().all(|l| !l.contains('\n')), "lines: {seen:?}");
    }
}
