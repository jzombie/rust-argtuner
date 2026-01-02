use std::collections::BTreeMap;
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Sparkline};
use ratatui::{Frame, Terminal};
use rusqlite::{Connection, OpenFlags};

pub fn run(db_path: PathBuf, poll_ms: u64) -> io::Result<()> {
    let mut app = AppState {
        db_path,
        poll: Duration::from_millis(poll_ms),
        last_refresh: Instant::now()
            .checked_sub(Duration::from_secs(60))
            .unwrap_or_else(Instant::now),
        trials: Vec::new(),
        selected: 0,
        last_error: None,
    };

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, &mut app);

    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

#[derive(Debug, Clone)]
struct TrialRow {
    trial_id: i64,
    status: String,
    elapsed_ms: i64,
    error: Option<String>,
    fields: BTreeMap<String, String>,
}

struct AppState {
    db_path: PathBuf,
    poll: Duration,
    last_refresh: Instant,
    trials: Vec<TrialRow>,
    selected: usize,
    last_error: Option<String>,
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut AppState,
) -> io::Result<()> {
    loop {
        if app.last_refresh.elapsed() >= app.poll {
            refresh_trials(app);
            app.last_refresh = Instant::now();
        }

        terminal.draw(|frame| draw_ui(frame, app))?;

        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => {
                    if !app.trials.is_empty() {
                        app.selected = app.selected.saturating_sub(1);
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if !app.trials.is_empty() {
                        app.selected = (app.selected + 1).min(app.trials.len() - 1);
                    }
                }
                _ => {}
            }
        }
    }
}

fn refresh_trials(app: &mut AppState) {
    match load_trials(&app.db_path) {
        Ok(trials) => {
            app.trials = trials;
            if app.selected >= app.trials.len() && !app.trials.is_empty() {
                app.selected = app.trials.len() - 1;
            }
            app.last_error = None;
        }
        Err(err) => {
            app.last_error = Some(err);
        }
    }
}

fn load_trials(path: &Path) -> Result<Vec<TrialRow>, String> {
    let conn = open_connection(path)?;
    let mut stmt = conn
        .prepare(
            r#"
            SELECT trial_id, status, elapsed_ms, error, fields_json
            FROM trial_records
            ORDER BY trial_id ASC
            "#,
        )
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let fields_json: String = row.get(4)?;
            let fields: BTreeMap<String, String> =
                serde_json::from_str(&fields_json).map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(err),
                    )
                })?;
            Ok(TrialRow {
                trial_id: row.get(0)?,
                status: row.get(1)?,
                elapsed_ms: row.get(2)?,
                error: row.get(3)?,
                fields,
            })
        })
        .map_err(|err| err.to_string())?;

    let mut trials = Vec::new();
    for row in rows {
        trials.push(row.map_err(|err| err.to_string())?);
    }
    Ok(trials)
}

fn open_connection(path: &Path) -> Result<Connection, String> {
    if !path.exists() {
        return Err(format!("missing db: {}", path.display()));
    }

    let uri = format!("file:{}?immutable=1", path.display());
    let ro_flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    if let Ok(conn) = Connection::open_with_flags(&uri, ro_flags) {
        return Ok(conn);
    }

    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|err| {
        format!(
            "db open failed (uri={uri}, fallback rw err={err}) for {}",
            path.display()
        )
    })
}

fn draw_ui(frame: &mut Frame, app: &mut AppState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(0)])
        .split(area);

    draw_sparkline(frame, app, chunks[0]);
    draw_detail(frame, app, chunks[1]);
}

fn draw_sparkline(frame: &mut Frame, app: &AppState, area: Rect) {
    let (metric_label, values) = metric_series(app);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("Metric Sparkline ({metric_label})"));
    let sparkline = Sparkline::default()
        .block(block)
        .data(&values)
        .style(Style::default().fg(Color::Green));
    frame.render_widget(sparkline, area);
}

fn draw_detail(frame: &mut Frame, app: &mut AppState, area: Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    draw_trial_list(frame, app, columns[0]);
    draw_trial_detail(frame, app, columns[1]);
}

fn draw_trial_list(frame: &mut Frame, app: &mut AppState, area: Rect) {
    let items = app
        .trials
        .iter()
        .map(|trial| {
            let metric_text = metric_value_text(trial).unwrap_or_else(|| "-".to_string());
            let line = format!(
                "trial {:>3}  {:<7}  {}",
                trial.trial_id, trial.status, metric_text
            );
            ListItem::new(line)
        })
        .collect::<Vec<_>>();

    let mut state = ListState::default();
    if !app.trials.is_empty() {
        state.select(Some(app.selected));
    }

    let block = Block::default().borders(Borders::ALL).title("Trials");
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().fg(Color::Yellow));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_trial_detail(frame: &mut Frame, app: &mut AppState, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title("Details");
    let text = if let Some(err) = app.last_error.as_ref() {
        vec![Line::from(Span::styled(
            err.clone(),
            Style::default().fg(Color::Red),
        ))]
    } else if let Some(trial) = app.trials.get(app.selected) {
        let mut lines = Vec::new();
        lines.push(Line::from(format!("trial_id: {}", trial.trial_id)));
        lines.push(Line::from(format!("status: {}", trial.status)));
        lines.push(Line::from(format!("elapsed_ms: {}", trial.elapsed_ms)));
        if let Some(error) = &trial.error {
            lines.push(Line::from(format!("error: {}", error)));
        }
        if let Some((label, value)) = metric_for_trial(trial) {
            lines.push(Line::from(format!("metric: {label} = {value}")));
        }
        if let Some(score) = trial.fields.get("score") {
            lines.push(Line::from(format!("score: {}", score)));
        }
        let mut metric_fields = trial
            .fields
            .iter()
            .filter(|(k, _)| k.starts_with("metric.") && k.as_str() != "metric")
            .map(|(k, v)| format!("{k} = {v}"))
            .collect::<Vec<_>>();
        metric_fields.sort();
        if !metric_fields.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from("metrics:"));
            for item in metric_fields {
                lines.push(Line::from(format!("  {item}")));
            }
        }
        lines
    } else {
        vec![Line::from("No trials loaded.")]
    };

    let paragraph = Paragraph::new(text).block(block);
    frame.render_widget(paragraph, area);
}

fn metric_for_trial(trial: &TrialRow) -> Option<(String, f64)> {
    if let Some(key) = trial.fields.get("metric") {
        let field = format!("metric.{key}");
        let value = trial.fields.get(&field)?.parse::<f64>().ok()?;
        return Some((field, value));
    }
    trial
        .fields
        .iter()
        .find(|(k, _)| k.starts_with("metric."))
        .and_then(|(k, v)| v.parse::<f64>().ok().map(|value| (k.clone(), value)))
}

fn metric_value_text(trial: &TrialRow) -> Option<String> {
    metric_for_trial(trial).map(|(label, value)| format!("{label}={value:.4}"))
}

fn metric_series(app: &AppState) -> (String, Vec<u64>) {
    let Some(trial) = app.trials.get(app.selected) else {
        return ("none".to_string(), Vec::new());
    };
    let metric_key = metric_for_trial(trial)
        .map(|(label, _)| label)
        .or_else(|| {
            app.trials
                .iter()
                .find_map(|t| metric_for_trial(t).map(|(label, _)| label))
        })
        .unwrap_or_else(|| "metric".to_string());

    let mut values = Vec::new();
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut parsed = Vec::new();
    for trial in &app.trials {
        let value = trial
            .fields
            .get(&metric_key)
            .and_then(|v| v.parse::<f64>().ok());
        parsed.push(value);
        if let Some(v) = value {
            min = min.min(v);
            max = max.max(v);
        }
    }
    let range = if max > min { max - min } else { 1.0 };
    for value in parsed {
        let scaled = value.map(|v| ((v - min) / range * 100.0).round() as u64);
        values.push(scaled.unwrap_or(0));
    }
    (metric_key, values)
}
