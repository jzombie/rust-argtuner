use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};

use argtuner_sdk::prelude::*;

#[talkback_args]
struct ProbeArgs {
    /// Metric key to emit under
    metric_key: Option<String>,
    /// Checkpoint directory (injected: trial_dir)
    #[param(role = ParamRole::Injected, value_name = "trial_dir")]
    checkpoint_dir: Option<String>,
}

fn main() -> io::Result<()> {
    let (talkback, parsed) = argtuner_sdk::init::<ProbeArgs>();
    let metric_key = parsed.metric_key.as_deref().unwrap_or("metric");
    let checkpoint_dir = parsed.checkpoint_dir.as_deref();
    let mut reader = input_reader();
    let mut stdout = io::stdout();

    print_args_table(&mut stdout, metric_key, checkpoint_dir)?;
    writeln!(
        stdout,
        "argtuner interactive probe (metric key = {})",
        metric_key
    )?;
    print_help(&mut stdout)?;
    stdout.flush()?;

    let mut buffer = String::new();
    loop {
        buffer.clear();
        write!(stdout, "probe> ")?;
        stdout.flush()?;
        let bytes = reader.read_line(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        let line = buffer.trim_end();
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        let mut parts = input.split_whitespace();
        let raw_cmd = parts.next().unwrap_or("");
        let cmd = normalize_cmd(raw_cmd);
        match cmd.as_str() {
            "help" | "h" => {
                print_help(&mut stdout)?;
            }
            "quit" | "exit" | "q" => {
                writeln!(stdout, "exiting...")?;
                stdout.flush()?;
                return Ok(());
            }
            "result" | "r" => {
                let snapshot = parts.clone();
                let mut fields = parse_kv(snapshot);
                if fields.is_empty() {
                    let value = match parts.next() {
                        Some(value) => value,
                        None => {
                            writeln!(
                                stdout,
                                "missing value (try `result 0.42` or `result metric=0.42`)"
                            )?;
                            continue;
                        }
                    };
                    fields.insert(metric_key.to_string(), value.to_string());
                } else if !fields.contains_key(metric_key) {
                    let value = match parts.next() {
                        Some(value) => value,
                        None => {
                            // No extra positional value; keep kv-only.
                            talkback.emit_event(argtuner_common::EventKind::EpochEnd, &fields)?;
                            continue;
                        }
                    };
                    fields.insert(metric_key.to_string(), value.to_string());
                }
                talkback.emit_event(argtuner_common::EventKind::EpochEnd, &fields)?;
            }
            "event" | "e" => {
                let name = match parts.next() {
                    Some(name) => name,
                    None => {
                        writeln!(stdout, "missing event name")?;
                        continue;
                    }
                };
                let Some(event) = argtuner_common::EventKind::from_name(name) else {
                    writeln!(
                        stdout,
                        "unknown event: {name} (supported: model.early_stopped, model.invalid_config, model.epoch_end)"
                    )?;
                    continue;
                };
                let fields = parse_kv(parts);
                talkback.emit_event(event, &fields)?;
            }
            "invalid" | "i" => {
                let reason = parts.collect::<Vec<_>>().join(" ");
                let reason = if reason.is_empty() {
                    "invalid_config".to_string()
                } else {
                    reason
                };
                talkback.emit_event(
                    argtuner_common::EventKind::InvalidConfig,
                    &BTreeMap::from([("error".to_string(), reason)]),
                )?;
            }
            _ => {
                writeln!(stdout, "unknown command: {raw_cmd}")?;
            }
        }
        stdout.flush()?;
    }
    Ok(())
}

fn normalize_cmd(cmd: &str) -> String {
    let trimmed = cmd.trim_end_matches('/');
    let head = trimmed.split('/').next().unwrap_or(trimmed);
    head.to_string()
}

fn input_reader() -> Box<dyn BufRead> {
    #[cfg(unix)]
    {
        if let Ok(file) = std::fs::File::open("/dev/tty") {
            return Box::new(io::BufReader::new(file));
        }
    }
    Box::new(io::BufReader::new(io::stdin()))
}

fn print_help(stdout: &mut impl Write) -> io::Result<()> {
    writeln!(stdout, "Commands:")?;
    writeln!(
        stdout,
        "  result/r <value> [k=v...]         Emit model.epoch_end with metric key"
    )?;
    writeln!(
        stdout,
        "  result/r k=v [k=v...]             Emit model.epoch_end with explicit fields"
    )?;
    writeln!(
        stdout,
        "  event/e <name> [k=v...]           Emit EVENT with optional fields"
    )?;
    writeln!(
        stdout,
        "  invalid/i <reason>                Emit model.invalid_config + error result"
    )?;
    writeln!(stdout, "  help/h                            Show this help")?;
    writeln!(stdout, "  quit/q                            Exit the probe")?;
    writeln!(stdout, "Examples:")?;
    writeln!(stdout, "  r 0.42 last_epoch=3")?;
    writeln!(stdout, "  r metric=0.42 aux=1")?;
    writeln!(stdout, "  e model.early_stopped")?;
    writeln!(stdout, "  e model.epoch_end epoch=1 loss=0.42")?;
    Ok(())
}

fn print_args_table(
    stdout: &mut impl Write,
    metric_key: &str,
    checkpoint_dir: Option<&str>,
) -> io::Result<()> {
    let rows = [
        ("metric_key", metric_key),
        ("checkpoint_dir", checkpoint_dir.unwrap_or("-")),
    ];
    let col1 = rows
        .iter()
        .map(|(k, _)| k.len())
        .max()
        .unwrap_or(3)
        .max("arg".len());
    let max_value = rows
        .iter()
        .map(|(_, v)| v.len())
        .max()
        .unwrap_or(5)
        .max("value".len());
    let col2 = max_value.clamp(16, 72);

    print_rule(stdout, col1, col2)?;
    writeln!(
        stdout,
        "| {:<col1$} | {:<col2$} |",
        "arg",
        "value",
        col1 = col1,
        col2 = col2
    )?;
    print_rule(stdout, col1, col2)?;
    for (key, value) in rows {
        let chunks = wrap_value(value, col2);
        for (idx, chunk) in chunks.iter().enumerate() {
            if idx == 0 {
                writeln!(
                    stdout,
                    "| {:<col1$} | {:<col2$} |",
                    key,
                    chunk,
                    col1 = col1,
                    col2 = col2
                )?;
            } else {
                writeln!(
                    stdout,
                    "| {:<col1$} | {:<col2$} |",
                    "",
                    chunk,
                    col1 = col1,
                    col2 = col2
                )?;
            }
        }
    }
    print_rule(stdout, col1, col2)?;
    Ok(())
}

fn print_rule(stdout: &mut impl Write, col1: usize, col2: usize) -> io::Result<()> {
    writeln!(
        stdout,
        "+-{:-<col1$}-+-{:-<col2$}-+",
        "",
        "",
        col1 = col1,
        col2 = col2
    )
}

fn wrap_value(value: &str, width: usize) -> Vec<String> {
    if value.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut start = 0;
    let chars: Vec<char> = value.chars().collect();
    while start < chars.len() {
        let end = (start + width).min(chars.len());
        out.push(chars[start..end].iter().collect());
        start = end;
    }
    out
}

fn parse_kv<'a>(parts: impl Iterator<Item = &'a str>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for part in parts {
        if let Some((k, v)) = part.split_once('=') {
            out.insert(k.to_string(), percent_encode(v));
        }
    }
    out
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '%' => out.push_str("%25"),
            ',' => out.push_str("%2C"),
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            _ => out.push(ch),
        }
    }
    out
}
