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

fn parse_message(input: &str) -> Result<Vec<ParsedItem>, String> {
    let message: argtuner_common::TalkbackMessage =
        serde_json::from_str(input).map_err(|e| format!("parse failed: {e}"))?;
    match message {
        argtuner_common::TalkbackMessage::Event { name, fields } => {
            Ok(vec![ParsedItem::Event { name, fields }])
        }
        argtuner_common::TalkbackMessage::Result { fields } => Ok(fields
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
        let line = strip_ansi(line);
        let start_idx = match line.find(prefix) {
            Some(idx) => idx,
            None => continue,
        };
        let rest = &line[start_idx + prefix.len()..];
        let msg = rest.trim();
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
        let line = strip_ansi(line);
        let start_idx = match line.find(prefix) {
            Some(idx) => idx,
            None => continue,
        };
        let rest = &line[start_idx + prefix.len()..];
        let msg = rest.trim();
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
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    let mut prev_esc = false;
                    for c in chars.by_ref() {
                        if c == '\u{07}' {
                            break;
                        }
                        if prev_esc && c == '\\' {
                            break;
                        }
                        prev_esc = c == '\u{1b}';
                    }
                }
                _ => {}
            }
            continue;
        }
        out.push(ch);
    }
    out
}
