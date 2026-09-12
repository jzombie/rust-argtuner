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
    let message: argtuner_common::IpcMessage =
        serde_json::from_str(input).map_err(|e| format!("parse failed: {e}"))?;
    match message {
        argtuner_common::IpcMessage::Event { name, fields } => {
            Ok(vec![ParsedItem::Event { name, fields }])
        }
        argtuner_common::IpcMessage::Result { fields } => Ok(fields
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
        let mut v = parse_message(msg)?;
        items.append(&mut v);
    }
    Ok(items)
}

/// Parse a single output line into its `ParsedItem`s. Returns an empty vec
/// when the line carries no prefixed message (plain log output). Tolerates a
/// trailing `\r` (PTY `\r\n`, progress-bar fragments) and a protocol prefix
/// glued mid-line after a `\r` progress-bar update with no newline.
pub fn parse_line(line: &str, prefix: &str) -> Result<Vec<ParsedItem>, String> {
    let line = strip_ansi(line);
    let start_idx = match line.find(prefix) {
        Some(idx) => idx,
        None => return Ok(Vec::new()),
    };
    let rest = &line[start_idx + prefix.len()..];
    let msg = rest.trim();
    if msg.is_empty() {
        return Ok(Vec::new());
    }
    parse_message(msg)
}

/// Parse each matching prefixed line into its own list of `ParsedItem`s and
/// return a vector where each element corresponds to a matched line in order.
pub fn parse_prefix_lines(output: &str, prefix: &str) -> Result<Vec<Vec<ParsedItem>>, String> {
    let mut lines_items: Vec<Vec<ParsedItem>> = Vec::new();
    for line in output.lines() {
        let v = parse_line(line, prefix)?;
        if v.is_empty() {
            continue;
        }
        lines_items.push(v);
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

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str = "::ARGTUNER::";

    fn event_line(metric: &str) -> String {
        format!(
            r#"{PREFIX}{{"type":"event","name":"model.epoch_end","fields":{{"metric":"{metric}"}}}}"#
        )
    }

    #[test]
    fn parse_line_plain_log_returns_empty() {
        assert!(
            parse_line("info: still running", PREFIX)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn parse_line_protocol_event() {
        let items = parse_line(&event_line("0.42"), PREFIX).unwrap();
        assert_eq!(items.len(), 1);
        match &items[0] {
            ParsedItem::Event { name, fields } => {
                assert_eq!(name, "model.epoch_end");
                assert_eq!(fields.get("metric").map(String::as_str), Some("0.42"));
            }
            other => panic!("expected event, got {other:?}"),
        }
    }

    #[test]
    fn parse_line_glued_after_progress_fragment() {
        // Progress bars write `\x1b[2K\r<bar>` with no trailing newline, so the
        // protocol line arrives glued onto the bar fragment.
        let line = format!("[2K\rtrain 1/3 [###---] step 5/10{}", event_line("0.5"));
        let items = parse_line(&line, PREFIX).unwrap();
        assert_eq!(items.len(), 1);
        match &items[0] {
            ParsedItem::Event { name, fields } => {
                assert_eq!(name, "model.epoch_end");
                assert_eq!(fields.get("metric").map(String::as_str), Some("0.5"));
            }
            other => panic!("expected event, got {other:?}"),
        }
    }

    #[test]
    fn parse_line_trailing_cr_from_pty() {
        let items = parse_line(&format!("{}\r", event_line("0.7")), PREFIX).unwrap();
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn parse_line_prefix_without_payload_returns_empty() {
        assert!(parse_line(PREFIX, PREFIX).unwrap().is_empty());
        assert!(
            parse_line(&format!("{PREFIX}   "), PREFIX)
                .unwrap()
                .is_empty()
        );
    }
}
