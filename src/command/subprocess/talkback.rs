use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedItem {
    Event {
        name: String,
        fields: BTreeMap<String, String>,
    },
    Result {
        name: String,
        value: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum JsonMessage {
    Event {
        name: String,
        #[serde(default)]
        fields: BTreeMap<String, String>,
    },
    Result {
        #[serde(default)]
        fields: BTreeMap<String, String>,
    },
}

fn parse_message(input: &str) -> Result<Vec<ParsedItem>, String> {
    let message: JsonMessage =
        serde_json::from_str(input).map_err(|e| format!("parse failed: {e}"))?;
    match message {
        JsonMessage::Event { name, fields } => Ok(vec![ParsedItem::Event { name, fields }]),
        JsonMessage::Result { fields } => Ok(fields
            .into_iter()
            .map(|(name, value)| ParsedItem::Result { name, value })
            .collect()),
    }
}

/// Parse the lines in `output` and return parsed items for lines starting with the given `prefix`.
/// Expects JSON messages after the prefix, e.g.:
/// `ARGTUNER::{"type":"event","name":"model.early_stopped","fields":{}}`.
pub fn parse_output(output: &str, prefix: &str) -> Result<Vec<ParsedItem>, String> {
    let mut items = Vec::new();
    for line in output.lines() {
        let line = sanitize_line(line);
        let line = line.trim();
        if !line.starts_with(prefix) {
            continue;
        }
        let rest = &line[prefix.len()..];
        let msg = rest.trim_start();
        if msg.is_empty() {
            continue;
        }
        match parse_message(msg) {
            Ok(mut v) => items.append(&mut v),
            Err(e) => return Err(e),
        }
    }
    Ok(items)
}

/// Parse each matching prefixed line into its own list of `ParsedItem`s and
/// return a vector where each element corresponds to a matched line in order.
pub fn parse_prefix_lines(output: &str, prefix: &str) -> Result<Vec<Vec<ParsedItem>>, String> {
    let mut lines_items: Vec<Vec<ParsedItem>> = Vec::new();
    for line in output.lines() {
        let line = sanitize_line(line);
        let line = line.trim();
        if !line.starts_with(prefix) {
            continue;
        }
        let rest = &line[prefix.len()..];
        let msg = rest.trim_start();
        if msg.is_empty() {
            continue;
        }
        match parse_message(msg) {
            Ok(v) => lines_items.push(v),
            Err(e) => return Err(e),
        }
    }
    Ok(lines_items)
}

// Workaround for control characters in subprocess output causing macOS tests to
// fail in GitHub Actions. Note, local development macOS does not have this issue.
fn sanitize_line(line: &str) -> String {
    line.chars().filter(|c| !c.is_ascii_control()).collect()
}
