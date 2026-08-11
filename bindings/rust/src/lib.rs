use std::collections::BTreeMap;
use std::io;

use serde::Serialize;
use serde::de::DeserializeOwned;

pub use argtuner_common::EventKind;
use argtuner_common::TalkbackMessage;

pub const PREFIX: &str = argtuner_common::RESULT_PREFIX;
pub const BINDING_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PRINT_TEMPLATE_FLAG: &str = "--print-template";
pub const PRINT_TEMPLATE_TOML_FLAG: &str = "--print-template-toml";
pub const PRINT_PROTOCOL_SCHEMA_FLAG: &str = "--print-protocol-schema";

#[derive(Debug, Clone)]
pub struct Talkback {
    args_map: BTreeMap<String, Vec<String>>,
}

impl Talkback {
    pub fn init() -> Self {
        let mut fields = BTreeMap::new();
        fields.insert(
            argtuner_common::BINDING_VERSION_FIELD.to_string(),
            BINDING_VERSION.to_string(),
        );
        let _ = emit_event(argtuner_common::EventKind::TunerApiVersion, &fields);

        let raw_args = std::env::args().collect::<Vec<_>>();
        let args_map = args_map_from(raw_args);
        Self { args_map }
    }

    pub fn args_map(&self) -> &BTreeMap<String, Vec<String>> {
        &self.args_map
    }

    pub fn parse_args<T: DeserializeOwned>(&self) -> Result<T, String> {
        parse_args_from_map(self.args_map())
    }

    pub fn emit_event<T: Serialize>(
        &self,
        event: argtuner_common::EventKind,
        value: &T,
    ) -> io::Result<()> {
        emit_event(event, value)
    }

    pub fn emit_result<T: Serialize>(&self, value: &T) -> io::Result<()> {
        emit_result(value)
    }

    pub fn emit_epoch_end<T: Serialize>(&self, value: &T) -> io::Result<()> {
        emit_epoch_end(value)
    }

    pub fn emit_step_end<T: Serialize>(&self, value: &T) -> io::Result<()> {
        emit_step_end(value)
    }
}

pub fn emit_event<T: Serialize>(event: argtuner_common::EventKind, value: &T) -> io::Result<()> {
    if matches!(event, argtuner_common::EventKind::Result) {
        return emit_result(value);
    }
    let fields = fields_from_value(value)?;
    emit_json(&TalkbackMessage::Event {
        name: event.as_str().to_string(),
        fields,
    })
}

pub fn emit_epoch_end<T: Serialize>(value: &T) -> io::Result<()> {
    emit_event(argtuner_common::EventKind::EpochEnd, value)
}

pub fn emit_step_end<T: Serialize>(value: &T) -> io::Result<()> {
    emit_event(argtuner_common::EventKind::StepEnd, value)
}

pub fn emit_result<T: Serialize>(value: &T) -> io::Result<()> {
    let fields = fields_from_value(value)?;
    if fields.is_empty() {
        return Ok(());
    }
    emit_json(&TalkbackMessage::Result { fields })
}

/// Milliseconds to hold the process open after flushing stdout so a PTY-based
/// parent can drain buffered protocol lines before this process exits.
pub const PTY_DRAIN_HOLD_MS: u64 = 50;

// TODO: This timer condition seems like a potential data loss issue. Is there a better
// way rather than relying on a timer?
//
/// Flush stdout and briefly hold the process open before exiting.
///
/// A parent reading this process's output through a PTY (e.g. the argtuner
/// command runner) only drains the kernel's PTY output buffer while the slave
/// side is open. If this process emits its final lines and exits immediately,
/// that buffer can be destroyed before the parent reads it (most visible on
/// macOS), losing the last `::ARGTUNER::` line. Call this after the last emit
/// so the parent has time to drain.
pub fn hold_stdout_open() {
    std::io::Write::flush(&mut std::io::stdout()).ok();
    std::thread::sleep(std::time::Duration::from_millis(PTY_DRAIN_HOLD_MS));
}

pub fn args_map() -> BTreeMap<String, Vec<String>> {
    args_map_from(std::env::args())
}

pub fn parse_args<T: DeserializeOwned>() -> Result<T, String> {
    parse_args_from_map(&args_map())
}

#[cfg(feature = "clap")]
pub fn maybe_print_template_and_exit<T: clap::CommandFactory>() {
    let wants_template = std::env::args().any(|arg| arg == PRINT_TEMPLATE_FLAG);
    let wants_toml = std::env::args().any(|arg| arg == PRINT_TEMPLATE_TOML_FLAG);
    if !wants_template && !wants_toml {
        return;
    }

    if wants_toml {
        println!("{}", render_template_toml::<T>());
    } else {
        println!("{}", render_template_command::<T>());
    }
    std::process::exit(0);
}

#[cfg(feature = "clap")]
fn placeholder_for_arg(long: &str) -> String {
    match long {
        "checkpoint-dir" => "trial_dir".to_string(),
        _ => long.replace('-', "_"),
    }
}

#[cfg(feature = "clap")]
pub fn render_template_command<T: clap::CommandFactory>() -> String {
    let bin = resolve_bin_path();
    let mut parts = vec![bin];
    let command = T::command();
    for arg in command.get_arguments() {
        if matches!(
            arg.get_long(),
            Some("print-template") | Some("print-template-toml") | Some("print-protocol-schema")
        ) {
            continue;
        }
        if arg.get_long() == Some("help") || arg.get_long() == Some("version") {
            continue;
        }
        let Some(long) = arg.get_long() else {
            continue;
        };
        if arg
            .get_num_args()
            .map(|range| !range.takes_values())
            .unwrap_or(false)
        {
            parts.push(format!("--{long}"));
        } else {
            let placeholder = placeholder_for_arg(long);
            parts.push(format!("--{long} {{{placeholder}}}"));
        }
    }
    parts.join(" ")
}

#[cfg(feature = "clap")]
pub fn render_template_toml<T: clap::CommandFactory>() -> String {
    let template = render_template_command::<T>();
    argtuner_common::render_starter_toml(&template)
}

#[cfg(feature = "clap")]
fn resolve_bin_path() -> String {
    let from_current = std::env::current_exe().ok();
    let from_args = std::env::args().next().map(std::path::PathBuf::from);
    let candidate = from_current.or(from_args);
    if let Some(path) = candidate
        && let Some(resolved) = normalize_bin_path(&path)
    {
        return resolved;
    }
    "binary".to_string()
}

#[cfg(feature = "clap")]
fn normalize_bin_path(path: &std::path::Path) -> Option<String> {
    let path = path.to_path_buf();
    let is_rs = path.extension().and_then(|ext| ext.to_str()) == Some("rs");
    if (is_rs || !path.exists())
        && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
        && let Some(resolved) = try_target_bin(stem)
    {
        return Some(resolved);
    }
    Some(path.display().to_string())
}

#[cfg(feature = "clap")]
fn try_target_bin(stem: &str) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let mut examples = cwd.join("target").join("debug").join("examples");
    if cfg!(windows) {
        examples.push(format!("{stem}.exe"));
    } else {
        examples.push(stem);
    }
    if examples.exists() {
        return Some(examples.display().to_string());
    }

    let mut bin = cwd.join("target").join("debug");
    if cfg!(windows) {
        bin.push(format!("{stem}.exe"));
    } else {
        bin.push(stem);
    }
    if bin.exists() {
        return Some(bin.display().to_string());
    }
    None
}

#[cfg(feature = "clap")]
pub fn init_with_args<T: clap::Parser + clap::CommandFactory>() -> (Talkback, T) {
    maybe_print_protocol_schema_and_exit();
    maybe_print_template_and_exit::<T>();
    let talkback = Talkback::init();
    let args = T::parse();
    (talkback, args)
}

/// Print the talkback protocol JSON Schema to stdout and exit if the
/// `--print-protocol-schema` flag is present on argv. Call early (before any
/// protocol messages are emitted) so stdout stays clean.
pub fn maybe_print_protocol_schema_and_exit() {
    if !std::env::args().any(|arg| arg == PRINT_PROTOCOL_SCHEMA_FLAG) {
        return;
    }
    print_protocol_schema();
    std::process::exit(0);
}

/// Print the talkback protocol JSON Schema to stdout.
pub fn print_protocol_schema() {
    print!("{}", argtuner_common::protocol_schema_string());
}

pub fn parse_args_from_map<T: DeserializeOwned>(
    map: &BTreeMap<String, Vec<String>>,
) -> Result<T, String> {
    let mut json = serde_json::Map::new();
    for (key, values) in map {
        if values.len() == 1 {
            json.insert(key.clone(), parse_value(&values[0]));
        } else {
            json.insert(
                key.clone(),
                serde_json::Value::Array(values.iter().map(|value| parse_value(value)).collect()),
            );
        }
    }
    serde_json::from_value(serde_json::Value::Object(json))
        .map_err(|err| format!("args parse error: {err}"))
}

fn fields_from_value<T: Serialize>(value: &T) -> io::Result<BTreeMap<String, String>> {
    let json = serde_json::to_value(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let serde_json::Value::Object(map) = json else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "result struct must serialize to an object",
        ));
    };
    let mut fields = BTreeMap::new();
    for (key, value) in map {
        fields.insert(key, value_to_string(&value));
    }
    Ok(fields)
}

fn value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(v) => v.to_string(),
        serde_json::Value::Number(v) => v.to_string(),
        serde_json::Value::String(v) => v.clone(),
        _ => value.to_string(),
    }
}

fn parse_value(value: &str) -> serde_json::Value {
    let trimmed = value.trim();
    if let Ok(v) = trimmed.parse::<bool>() {
        return serde_json::Value::Bool(v);
    }
    if let Ok(v) = trimmed.parse::<i64>() {
        return serde_json::Value::Number(v.into());
    }
    if let Ok(v) = trimmed.parse::<f64>()
        && let Some(number) = serde_json::Number::from_f64(v)
    {
        return serde_json::Value::Number(number);
    }
    serde_json::Value::String(value.to_string())
}

pub fn args_map_from<I>(args: I) -> BTreeMap<String, Vec<String>>
where
    I: IntoIterator<Item = String>,
{
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut iter = args.into_iter();
    let mut positional = Vec::new();
    let _program = iter.next();
    while let Some(arg) = iter.next() {
        if let Some(stripped) = arg.strip_prefix("--") {
            if let Some((key, value)) = stripped.split_once('=') {
                map.entry(key.to_string())
                    .or_default()
                    .push(value.to_string());
            } else {
                let value = iter.next().unwrap_or_default();
                map.entry(stripped.to_string()).or_default().push(value);
            }
        } else {
            positional.push(arg);
        }
    }
    if !positional.is_empty() {
        map.insert("_".to_string(), positional);
    }
    map
}

fn emit_line(line: String) -> io::Result<()> {
    println!("{line}");
    Ok(())
}

fn emit_json<T: Serialize>(value: &T) -> io::Result<()> {
    let json = serde_json::to_value(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    emit_line(format!("{PREFIX}{json}"))
}
