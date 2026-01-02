use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use super::talkback::{ParsedItem, parse_prefix_lines};
use crate::constants::{METRIC_NAMESPACE, MODEL_NAMESPACE, TUNER_NAMESPACE};

#[derive(Debug)]
pub struct CommandOutput {
    pub stdout: String,
    pub _stderr: String,
    pub exit_code: i32,
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
        })
    }
}

pub struct CommandResultPayload {
    pub data: BTreeMap<String, String>,
    pub epoch_results: Vec<BTreeMap<String, String>>,
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

impl CommandRunner {
    pub fn run(command: &str, envs: &BTreeMap<String, String>) -> Result<CommandOutput, String> {
        let parts =
            shell_words::split(command).map_err(|err| format!("command parse failed: {err}"))?;
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
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| format!("pty reader failed: {err}"))?;
        #[cfg(any(unix, windows))]
        let mut writer = pair
            .master
            .take_writer()
            .map_err(|err| format!("pty writer failed: {err}"))?;
        let output = thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut stdout = std::io::stdout();
            let mut out = String::new();
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
            out
        });
        #[cfg(windows)]
        let input_guard = {
            let handle = thread::spawn(move || {
                let mut stdin = std::io::stdin();
                let mut buf = [0u8; 1024];
                loop {
                    let read = stdin.read(&mut buf).unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    let _ = writer.write_all(&buf[..read]);
                    let _ = writer.flush();
                }
            });
            Some(InputGuard {
                handle: Some(handle),
                stop: None,
            })
        };

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
        let status = child
            .wait()
            .map_err(|err| format!("command wait failed: {err}"))?;
        let stdout = output
            .join()
            .map_err(|_| "pty reader thread panicked".to_string())?;
        if let Some(mut guard) = input_guard {
            guard.stop();
        }
        Ok(CommandOutput {
            stdout,
            _stderr: String::new(),
            exit_code: status.exit_code() as i32,
        })
    }
}

struct InputGuard {
    handle: Option<thread::JoinHandle<()>>,
    stop: Option<Arc<AtomicBool>>,
}

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
    use indoc::indoc;

    #[test]
    fn parse_result_payload_uses_last_matching_line() {
        let output_str = indoc! {r#"
            info: start
            ::ARGTUNER::{"type":"event","name":"model.epoch_end","fields":{"metric":"0.5","epoch":"1"}}
            ::ARGTUNER::{"type":"event","name":"model.epoch_end","fields":{"aux":"123","metric":"0.9","epoch":"2"}}
            info: still running
            ::ARGTUNER::{"type":"event","name":"model.epoch_end","fields":{"metric":"0.2","epoch":"3"}}
            done
        "#};
        let output = CommandOutput {
            stdout: output_str.to_string(),
            _stderr: String::new(),
            exit_code: 0,
        };
        let payload = output.parse_payload(crate::RESULT_PREFIX).expect("payload");
        let metric = payload.get_metric("metric").expect("metric");
        assert_eq!(metric, 0.2);
        assert!(!payload.data.contains_key("aux"));
    }
}
