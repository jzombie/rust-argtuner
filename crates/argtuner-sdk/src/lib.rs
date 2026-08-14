#![doc = include_str!("../README.md")]

/// Alias the crate to its own name so the `#[tuner_params]` derive's
/// generated `::argtuner_sdk::…` paths resolve when a struct is expanded
/// inside this crate (e.g. its own unit tests).
extern crate self as argtuner_sdk;

use std::collections::BTreeMap;
use std::io;

use serde::Serialize;
use serde::de::DeserializeOwned;

pub use argtuner_common::EventKind;

use argtuner_common::IpcMessage;
/// Re-export of the `#[tuner_params]` attribute macro so consumers only need
/// `argtuner-sdk`.
pub use argtuner_derive::tuner_params;

/// `clap` re-exported so the derive-generated code can reference it through
/// `argtuner_sdk::clap`, keeping clap off the consumer's dependency list.
pub use clap;
/// `serde` re-exported so consumers derive `Serialize` via this crate
/// (e.g. `use argtuner_sdk::serde::Serialize;`) without adding serde as a
/// direct dependency.
pub use serde;

/// Single import surface for training binaries: brings the derive macro, the
/// [`TunerParams`] contract, the [`ParamRole`]/[`ParamKind`] enums, initialization,
/// telemetry emission, and the [`IpcChannel`] handle into scope together.
///
/// ```rust,no_run
/// use argtuner_sdk::prelude::*;
///
/// #[tuner_params]
/// struct TrainParams {
///     #[param(role = ParamRole::Tune, default = 0.001, min = 0.0001, max = 0.1)]
///     lr: f64,
/// }
///
/// fn main() {
///     let (_channel, params) = init::<TrainParams>();
///     let _ = params.lr;
/// }
/// ```
pub mod prelude {
    pub use crate::emit_metrics;
    pub use crate::init;
    pub use crate::init_with_args;
    pub use crate::is_tuning_active;
    pub use crate::{
        EventKind, IpcChannel, MetricsBuilder, ParamKind, ParamRole, TunerParam, TunerParams,
        tuner_params,
    };
}

pub const PREFIX: &str = argtuner_common::RESULT_PREFIX;
pub const BINDING_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PRINT_TEMPLATE_FLAG: &str = "--print-template";
pub const PRINT_TEMPLATE_TOML_FLAG: &str = "--print-template-toml";
pub const PRINT_PROTOCOL_SCHEMA_FLAG: &str = "--print-protocol-schema";

/// Returns true when the process is running under argtuner, i.e. the tuner
/// exported [`argtuner_common::TUNING_MARKER_ENV`] to this subprocess.
///
/// All `emit_*` helpers no-op when this is false, so the same binary run
/// standalone (a human, a test, production inference) keeps `stdout` free of
/// `::ARGTUNER::` lines. The result is cached on first call.
pub fn is_tuning_active() -> bool {
    static ACTIVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ACTIVE.get_or_init(|| std::env::var_os(argtuner_common::TUNING_MARKER_ENV).is_some())
}

#[derive(Debug, Clone)]
pub struct IpcChannel {
    args_map: BTreeMap<String, Vec<String>>,
}

impl IpcChannel {
    /// Capture argv and emit the binding-version handshake.
    ///
    /// All [`IpcChannel`] `emit_*` methods (and the free `emit_*` functions) no-op
    /// when the process is not running under argtuner, so a standalone run of
    /// the binary keeps stdout clean. [`is_tuning_active`] reports the state.
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

    /// Start building a set of metric fields for emission (no serde derive).
    pub fn metrics(&self) -> MetricsBuilder<'_> {
        MetricsBuilder::new(self)
    }
}

pub fn emit_event<T: Serialize>(event: argtuner_common::EventKind, value: &T) -> io::Result<()> {
    if !is_tuning_active() {
        return Ok(());
    }
    if matches!(event, argtuner_common::EventKind::Result) {
        return emit_result(value);
    }
    let fields = fields_from_value(value)?;
    emit_json(&IpcMessage::Event {
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
    if !is_tuning_active() {
        return Ok(());
    }
    let fields = fields_from_value(value)?;
    if fields.is_empty() {
        return Ok(());
    }
    emit_json(&IpcMessage::Result { fields })
}

/// Build a set of metric fields for emission without any `serde` derive.
pub struct MetricsBuilder<'a> {
    channel: &'a IpcChannel,
    fields: BTreeMap<String, String>,
}

impl<'a> MetricsBuilder<'a> {
    pub fn new(channel: &'a IpcChannel) -> Self {
        Self {
            channel,
            fields: BTreeMap::new(),
        }
    }

    /// Record a metric field. Any `Display` value works (numbers, strings, …).
    pub fn record(&mut self, key: impl Into<String>, value: impl ToString) -> &mut Self {
        self.fields.insert(key.into(), value.to_string());
        self
    }

    /// Emit the recorded fields as an event of the given kind (no-op when not
    /// running under argtuner).
    pub fn emit_kind(&self, kind: argtuner_common::EventKind) -> io::Result<()> {
        self.channel.emit_event(kind, &self.fields)
    }

    /// Emit the recorded fields as a `model.epoch_end` event (no-op when not
    /// running under argtuner).
    pub fn emit(&self) -> io::Result<()> {
        self.emit_kind(argtuner_common::EventKind::EpochEnd)
    }

    /// Emit the recorded fields as a `model.step_end` event (no-op when not
    /// running under argtuner).
    pub fn emit_step(&self) -> io::Result<()> {
        self.emit_kind(argtuner_common::EventKind::StepEnd)
    }

    /// Emit the recorded fields as a flat result payload (no-op when not
    /// running under argtuner).
    pub fn emit_result(&self) -> io::Result<()> {
        self.channel.emit_result(&self.fields)
    }
}

/// Emit metric fields as a `model.epoch_end` event without any `serde` derive.
///
/// Keys and values are evaluated as expressions, so variables and
/// `format!(…)` work:
/// ```ignore
/// emit_metrics! { "loss" => loss, "epoch" => epoch };
/// let key = format!("epoch_{i}"); emit_metrics! { key => loss };
/// ```
#[macro_export]
macro_rules! emit_metrics {
    ($($key:expr => $value:expr),* $(,)?) => {{
        let mut fields = ::std::collections::BTreeMap::<
            ::std::string::String,
            ::std::string::String,
        >::new();
        $(
            fields.insert(
                ::std::string::ToString::to_string(&$key),
                ::std::string::ToString::to_string(&$value),
            );
        )*
        $crate::emit_epoch_end(&fields)
    }};
}

pub fn args_map() -> BTreeMap<String, Vec<String>> {
    args_map_from(std::env::args())
}

pub fn parse_args<T: DeserializeOwned>() -> Result<T, String> {
    parse_args_from_map(&args_map())
}

/// Who supplies the value of a declared parameter — orthogonal to its
/// structural [`ParamKind`]. Declared per field with
/// `#[param(role = ParamRole::Tune)]` (a bare `role = tune` also parses);
/// defaults to [`ParamRole::Fixed`] for every type.
///
/// # Usage
///
/// ```
/// use argtuner_sdk::prelude::*;
///
/// #[tuner_params]
/// struct TrainParams {
///     #[param(role = ParamRole::Tune, default = 0.001, min = 0.0001, max = 0.1)]
///     lr: f64,
///     #[param(role = ParamRole::Injected, value_name = "trial_dir")]
///     checkpoint_dir: Option<String>,
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamRole {
    /// Constant value: **you** supply it.
    ///
    /// - `#[param(...)]` attributes: `default` (optional).
    /// - Template: `--flag <default>` baked as a literal (a fixed field with no
    ///   default is a standalone-only CLI arg, excluded from the template).
    /// - `[space]`: excluded.
    Fixed,
    /// Sampled hyperparameter: **argtuner** supplies it from the search space.
    ///
    /// - `#[param(...)]` attributes: `min` + `max` (float/int), `choices`
    ///   (string), or a bare bool; `default`, `log`, `step`, `parent`,
    ///   `parent_values` also allowed. The derive rejects this role without the
    ///   bounds its kind requires, and rejects numeric bounds on bools.
    /// - Template: `--flag {name}` placeholder.
    /// - `[space]`: included.
    Tune,
    /// Runtime value: **argtuner** injects it per trial.
    ///
    /// - `#[param(...)]` attributes: `value_name` must be `"trial_dir"` or
    ///   `"trial_id"` (anything else is a compile error). No `default`.
    /// - Template: `--flag {value_name}` placeholder.
    /// - `[space]`: excluded.
    Injected,
    /// Operational CLI-only flag: excluded from tuning entirely.
    ///
    /// - `#[param(...)]` attributes: `default` (required for non-`Option`
    ///   fields, so absent flags don't panic standalone). No constraints.
    /// - Template: excluded.
    /// - `[space]`: excluded.
    Cli,
}

/// The structural type of a declared parameter: how it maps onto the search
/// space and CLI parsing. Tunability is a separate concern, carried by
/// [`ParamRole`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// A float hyperparameter.
    Float,
    /// An integer hyperparameter.
    Int,
    /// A categorical hyperparameter.
    Choice,
    /// A boolean hyperparameter.
    Bool,
    /// Any other scalar/string CLI argument.
    Other,
}

/// Static description of one field of a [`TunerParams`] struct, generated by
/// `#[tuner_params]`.
#[derive(Debug, Clone, Copy)]
pub struct TunerParam {
    /// Field name; the template placeholder token.
    pub name: &'static str,
    /// `--long` flag name.
    pub long: &'static str,
    /// Override value name (e.g. `"trial_dir"` marks the reserved placeholder).
    pub value_name: Option<&'static str>,
    /// Default value rendered as a string (also the clap default).
    pub default: Option<&'static str>,
    /// `--help` text (from the field's doc comment).
    pub help: Option<&'static str>,
    pub kind: ParamKind,
    /// Who supplies this parameter's value (`#[param(role = ...)]`).
    pub role: ParamRole,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub log: bool,
    pub step: Option<f64>,
    pub choices: &'static [&'static str],
    /// Parent param this field is conditional on (`#[param(parent = "...")]`).
    pub parent: Option<&'static str>,
    /// Parent values that activate this field
    /// (`#[param(parent_values = ["..."])]`).
    pub parent_values: &'static [&'static str],
}

impl TunerParam {
    /// Whether argtuner injects this parameter's value at runtime (role
    /// `injected` with a reserved `trial_dir`/`trial_id` placeholder).
    pub fn is_reserved(&self) -> bool {
        matches!(self.role, ParamRole::Injected)
    }

    /// Whether this parameter belongs in the generated `[space]`: only
    /// `role = ParamRole::Tune` parameters are sampled.
    pub fn is_tunable(&self) -> bool {
        matches!(self.role, ParamRole::Tune)
    }
}

/// The contract `#[tuner_params]` implements for a parameter struct: a plain
/// struct becomes both a production `clap` CLI and an argtuner template/space
/// definition.
pub trait TunerParams: Sized {
    fn app_name() -> &'static str;
    fn tuner_params() -> &'static [TunerParam];
    fn command() -> clap::Command;
    fn from_matches(m: &clap::ArgMatches) -> Self;
}

pub fn maybe_print_template_and_exit<T: TunerParams>() {
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

/// Quote a binary path for the argtuner command template on the current
/// platform. Arg t's runner splits templates with `shell_words` on Unix (POSIX
/// single quotes) and a Windows tokenizer that understands `"`-delimited
/// tokens but not the POSIX single-quote escape pattern — so the path must be
/// quoted per-platform or a spaced install dir would split into multiple
/// tokens. (No `shell_words` dep: the SDK stays dependency-light.)
fn quote_bin_path(path: &str) -> String {
    #[cfg(windows)]
    {
        format!("\"{path}\"")
    }
    #[cfg(not(windows))]
    {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

pub fn render_template_command<T: TunerParams>() -> String {
    let bin = quote_bin_path(&resolve_bin_path());
    let mut parts = vec![bin];
    for p in T::tuner_params() {
        match p.role {
            // Sampled params become placeholders filled from the search space;
            // injected params use their reserved value_name, supplied by argtuner.
            ParamRole::Tune => parts.push(format!("--{} {{{}}}", p.long, p.name)),
            ParamRole::Injected => {
                let placeholder = p.value_name.unwrap_or(p.name);
                parts.push(format!("--{} {{{placeholder}}}", p.long));
            }
            // Fixed CLI arg with a default: bake the literal default in. Bools
            // render as a bare `--flag` for `true` (all bools parse flag-style)
            // and are omitted for `false`, so no `--flag true/false` tokens.
            ParamRole::Fixed => match p.kind {
                ParamKind::Bool => {
                    if p.default == Some("true") {
                        parts.push(format!("--{}", p.long));
                    }
                }
                _ => {
                    if let Some(default) = p.default {
                        parts.push(format!("--{} {}", p.long, default));
                    }
                }
            },
            // Operational CLI-only flag: excluded from the template.
            ParamRole::Cli => {}
        }
        // Fixed/standalone args without a default are excluded so the generated
        // template stays renderable by argtuner.
    }
    parts.join(" ")
}

/// A tunable search-space parameter, serialized as a TOML `[[space.params]]`
/// entry by the `--print-template-toml` generator.
#[derive(serde::Serialize)]
#[serde(tag = "type")]
enum SpaceParam<'a> {
    Float {
        name: &'a str,
        min: f64,
        max: f64,
        #[serde(skip_serializing_if = "is_false")]
        log_scale: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent: Option<&'a str>,
        #[serde(skip_serializing_if = "is_empty")]
        parent_values: &'a [&'a str],
    },
    Int {
        name: &'a str,
        min: i64,
        max: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        step: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent: Option<&'a str>,
        #[serde(skip_serializing_if = "is_empty")]
        parent_values: &'a [&'a str],
    },
    Choice {
        name: &'a str,
        values: &'a [&'a str],
        #[serde(skip_serializing_if = "Option::is_none")]
        parent: Option<&'a str>,
        #[serde(skip_serializing_if = "is_empty")]
        parent_values: &'a [&'a str],
    },
    Bool {
        name: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent: Option<&'a str>,
        #[serde(skip_serializing_if = "is_empty")]
        parent_values: &'a [&'a str],
    },
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_empty(s: &&[&str]) -> bool {
    s.is_empty()
}

fn finite_i64(f: Option<f64>) -> i64 {
    f.filter(|f| f.is_finite()).map_or(0, |f| f.round() as i64)
}

impl<'a> SpaceParam<'a> {
    fn from_hint(p: &'a TunerParam) -> Option<Self> {
        let parent = p.parent;
        let parent_values = p.parent_values;
        match p.kind {
            ParamKind::Float => Some(SpaceParam::Float {
                name: p.name,
                min: p.min.unwrap_or(0.0),
                max: p.max.unwrap_or(0.0),
                log_scale: p.log,
                step: p.step,
                parent,
                parent_values,
            }),
            ParamKind::Int => Some(SpaceParam::Int {
                name: p.name,
                min: finite_i64(p.min),
                max: finite_i64(p.max),
                step: p.step.filter(|f| f.is_finite()).map(|f| f.round() as i64),
                parent,
                parent_values,
            }),
            ParamKind::Choice => Some(SpaceParam::Choice {
                name: p.name,
                values: p.choices,
                parent,
                parent_values,
            }),
            ParamKind::Bool => Some(SpaceParam::Bool {
                name: p.name,
                parent,
                parent_values,
            }),
            ParamKind::Other => None,
        }
    }
}

pub fn render_template_toml<T: TunerParams>() -> String {
    let template = render_template_command::<T>();
    let base = argtuner_common::render_starter_toml(&template);
    let mut doc: toml_edit::DocumentMut = base.parse().expect("starter template is valid TOML");
    let params: Vec<SpaceParam<'_>> = T::tuner_params()
        .iter()
        .filter(|p| p.is_tunable())
        .filter_map(SpaceParam::from_hint)
        .collect();
    if !params.is_empty() {
        let mut params_arr = toml_edit::ArrayOfTables::new();
        for sp in &params {
            let doc = toml_edit::ser::to_document(sp).expect("space param serializes to TOML");
            params_arr.push(doc.as_table().clone());
        }
        let space = doc
            .get_mut("space")
            .and_then(|s| s.as_table_mut())
            .expect("starter template has a [space] table");
        space.remove("params");
        space.insert("params", toml_edit::Item::ArrayOfTables(params_arr));
    }
    doc.to_string()
}

fn resolve_bin_path() -> String {
    let from_current = std::env::current_exe().ok();
    let from_hints = std::env::args().next().map(std::path::PathBuf::from);
    let candidate = from_current.or(from_hints);
    if let Some(path) = candidate
        && let Some(resolved) = normalize_bin_path(&path)
    {
        return resolved;
    }
    "binary".to_string()
}

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

/// Unified entry point for a struct annotated with `#[tuner_params]`.
///
/// Declare your algorithm's parameters once as a plain struct (with optional
/// `#[param(...)]` hints); the derive generates the `clap` CLI, the command
/// template, and the search space. `init`:
///
/// 1. pre-inspects raw `std::env::args()` for `--print-template`,
///    `--print-template-toml`, and `--print-protocol-schema`, printing the
///    generated config text and exiting 0 before any clap parsing runs,
/// 2. emits the `::ARGTUNER::` binding-version handshake when running under
///    argtuner (suppressed on standalone runs),
/// 3. parses the remaining flags via the generated `clap` command and returns
///    `(IpcChannel, T)`.
pub fn init<T: TunerParams>() -> (IpcChannel, T) {
    init_with_args::<T>()
}

pub fn init_with_args<T: TunerParams>() -> (IpcChannel, T) {
    maybe_print_protocol_schema_and_exit();
    maybe_print_template_and_exit::<T>();
    let channel = IpcChannel::init();
    let matches = T::command().get_matches();
    let args = T::from_matches(&matches);
    (channel, args)
}

/// Print the ipc protocol JSON Schema to stdout and exit if the
/// `--print-protocol-schema` flag is present on argv. Call early (before any
/// protocol messages are emitted) so stdout stays clean.
pub fn maybe_print_protocol_schema_and_exit() {
    if !std::env::args().any(|arg| arg == PRINT_PROTOCOL_SCHEMA_FLAG) {
        return;
    }
    print_protocol_schema();
    std::process::exit(0);
}

/// Print the ipc protocol JSON Schema to stdout.
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

#[cfg(test)]
mod tests {
    use super::*;

    struct TemplateTunerParams;

    impl TunerParams for TemplateTunerParams {
        fn app_name() -> &'static str {
            "template-params"
        }

        fn tuner_params() -> &'static [TunerParam] {
            &[]
        }

        fn command() -> clap::Command {
            clap::Command::new(Self::app_name())
        }

        fn from_matches(_m: &clap::ArgMatches) -> Self {
            Self
        }
    }

    #[test]
    fn rendered_template_quotes_bin_path() {
        // The exe may live in a spaced directory (e.g. `/Volumes/2TB Storage
        // Vault/...`); the generated template must quote the path so argtuner's
        // command tokenizer recovers it as one token on every platform.
        let cmd = render_template_command::<TemplateTunerParams>();
        let quoted = quote_bin_path(&resolve_bin_path());
        assert_eq!(
            cmd, quoted,
            "template must quote the bin path for spaced install dirs: {cmd:?} (expected {quoted:?})"
        );
    }

    // ── const-path defaults ────────────────────────────────────────────────

    const DEFAULT_EPOCHS: usize = 10;
    const DEFAULT_LR: f64 = 0.001;
    const DEFAULT_MODE: &str = "pair";

    #[tuner_params]
    struct ConstDefaults {
        #[param(role = ParamRole::Fixed, default = DEFAULT_EPOCHS)]
        epochs: usize,
        #[param(role = ParamRole::Tune, default = DEFAULT_LR, min = 1e-6, max = 0.01)]
        lr: f64,
        #[param(role = ParamRole::Fixed, default = DEFAULT_MODE)]
        mode: String,
    }

    #[test]
    fn const_defaults_bake_into_template() {
        let cmd = render_template_command::<ConstDefaults>();
        assert!(cmd.contains("--epochs 10"), "numeric const default baked: {cmd}");
        assert!(cmd.contains("--mode pair"), "&str const default baked: {cmd}");
        assert!(cmd.contains("--lr {lr}"), "tune param stays a placeholder: {cmd}");
    }

    #[test]
    fn const_defaults_parse_when_absent() {
        let matches = <ConstDefaults as TunerParams>::command()
            .no_binary_name(true)
            .try_get_matches_from(Vec::<String>::new())
            .expect("parses with const defaults");
        let p = ConstDefaults::from_matches(&matches);
        assert_eq!(p.epochs, DEFAULT_EPOCHS);
        assert_eq!(p.lr, DEFAULT_LR);
        assert_eq!(p.mode, DEFAULT_MODE);
    }
}
