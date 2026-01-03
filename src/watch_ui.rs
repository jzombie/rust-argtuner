use std::collections::BTreeMap;
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, MouseEventKind,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::{Constraint, Direction, Layout, Rect};
use ratatui::symbols::Marker;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, Borders, Chart, Dataset, GraphType, List, ListItem, ListState, Paragraph,
    Scrollbar, ScrollbarOrientation, ScrollbarState,
};
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
        epoch_rows: BTreeMap::new(),
        selected: 0,
        last_error: None,
        trials_scroll: ScrollPane::default(),
        metrics_scroll: ScrollPane::default(),
        details_scroll: ScrollPane::default(),
        focus: PaneFocus::Trials,
        pane_rects: PaneRects::default(),
    };

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, &mut app);

    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableMouseCapture, LeaveAlternateScreen)?;
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
    epoch_rows: BTreeMap<i64, Vec<TrialRow>>,
    selected: usize,
    last_error: Option<String>,
    trials_scroll: ScrollPane,
    metrics_scroll: ScrollPane,
    details_scroll: ScrollPane,
    focus: PaneFocus,
    pane_rects: PaneRects,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneFocus {
    Trials,
    Charts,
    Details,
}

#[derive(Debug, Clone, Copy)]
struct PaneRects {
    trials: Rect,
    charts: Rect,
    details: Rect,
}

impl Default for PaneRects {
    fn default() -> Self {
        Self {
            trials: Rect::default(),
            charts: Rect::default(),
            details: Rect::default(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct ScrollPane {
    offset: usize,
    pending: isize,
}

impl ScrollPane {
    fn reset(&mut self) {
        self.offset = 0;
        self.pending = 0;
    }

    fn bump(&mut self, delta: isize) {
        self.pending = self.pending.saturating_add(delta);
    }

    fn apply(&mut self, total: usize, view: usize) {
        let max_offset = total.saturating_sub(view);
        if self.pending != 0 {
            let delta = self.pending;
            self.pending = 0;
            let next = if delta.is_negative() {
                self.offset.saturating_sub(delta.unsigned_abs())
            } else {
                self.offset.saturating_add(delta as usize)
            };
            self.offset = next.min(max_offset);
        } else if self.offset > max_offset {
            self.offset = max_offset;
        }
    }
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

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Up | KeyCode::Char('k') => apply_focus_delta(app, -1),
                    KeyCode::Down | KeyCode::Char('j') => apply_focus_delta(app, 1),
                    KeyCode::Tab => {
                        app.focus = match app.focus {
                            PaneFocus::Trials => PaneFocus::Charts,
                            PaneFocus::Charts => PaneFocus::Details,
                            PaneFocus::Details => PaneFocus::Trials,
                        };
                    }
                    KeyCode::PageDown | KeyCode::Char(']') => apply_focus_delta(app, 5),
                    KeyCode::PageUp | KeyCode::Char('[') => apply_focus_delta(app, -5),
                    _ => {}
                },
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        let delta = -1;
                        if let Some(pane) = pane_for_mouse(app, mouse.column, mouse.row) {
                            apply_delta_for_pane(app, pane, delta);
                        } else {
                            apply_focus_delta(app, delta);
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        let delta = 1;
                        if let Some(pane) = pane_for_mouse(app, mouse.column, mouse.row) {
                            apply_delta_for_pane(app, pane, delta);
                        } else {
                            apply_focus_delta(app, delta);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
}

fn apply_focus_delta(app: &mut AppState, delta: isize) {
    apply_delta_for_pane(app, app.focus, delta);
}

fn apply_delta_for_pane(app: &mut AppState, pane: PaneFocus, delta: isize) {
    match pane {
        PaneFocus::Trials => move_trial_selection(app, delta),
        PaneFocus::Charts => app.metrics_scroll.bump(delta),
        PaneFocus::Details => app.details_scroll.bump(delta),
    }
}

fn pane_for_mouse(app: &AppState, column: u16, row: u16) -> Option<PaneFocus> {
    if rect_contains(app.pane_rects.trials, column, row) {
        Some(PaneFocus::Trials)
    } else if rect_contains(app.pane_rects.charts, column, row) {
        Some(PaneFocus::Charts)
    } else if rect_contains(app.pane_rects.details, column, row) {
        Some(PaneFocus::Details)
    } else {
        None
    }
}

fn rect_contains(rect: Rect, column: u16, row: u16) -> bool {
    let max_x = rect.x.saturating_add(rect.width);
    let max_y = rect.y.saturating_add(rect.height);
    column >= rect.x && column < max_x && row >= rect.y && row < max_y
}

fn move_trial_selection(app: &mut AppState, delta: isize) {
    if app.trials.is_empty() {
        return;
    }
    let max = (app.trials.len() - 1) as isize;
    let current = app.selected as isize;
    let next = (current + delta).clamp(0, max) as usize;
    if next != app.selected {
        app.selected = next;
        app.metrics_scroll.reset();
        app.details_scroll.reset();
    }
}

fn refresh_trials(app: &mut AppState) {
    match load_trials(&app.db_path) {
        Ok((trials, epoch_rows)) => {
            app.trials = trials;
            app.epoch_rows = epoch_rows;
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

fn load_trials(path: &Path) -> Result<(Vec<TrialRow>, BTreeMap<i64, Vec<TrialRow>>), String> {
    let conn = open_connection(path)?;
    let trials = load_trial_rows(&conn, "trial_records")?;
    let epoch_rows = load_epoch_rows(&conn)?;
    Ok((trials, epoch_rows))
}

fn load_trial_rows(conn: &Connection, table: &str) -> Result<Vec<TrialRow>, String> {
    let mut stmt = conn
        .prepare(&format!(
            r#"
            SELECT trial_id, status, elapsed_ms, error, fields_json
            FROM {table}
            ORDER BY trial_id ASC
            "#,
        ))
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

fn load_epoch_rows(conn: &Connection) -> Result<BTreeMap<i64, Vec<TrialRow>>, String> {
    let mut stmt = match conn.prepare(
        r#"
        SELECT trial_id, status, elapsed_ms, error, fields_json
        FROM trial_epoch_records
        ORDER BY trial_id ASC, row_id ASC
        "#,
    ) {
        Ok(stmt) => stmt,
        Err(err) => {
            if err
                .to_string()
                .contains("no such table: trial_epoch_records")
            {
                return Ok(BTreeMap::new());
            }
            return Err(err.to_string());
        }
    };
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

    let mut by_trial: BTreeMap<i64, Vec<TrialRow>> = BTreeMap::new();
    for row in rows {
        let row = row.map_err(|err| err.to_string())?;
        by_trial.entry(row.trial_id).or_default().push(row);
    }
    Ok(by_trial)
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
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    app.pane_rects.trials = columns[0];
    draw_trial_list(frame, app, columns[0]);
    draw_metrics_overview(frame, app, columns[1]);
}

fn draw_trial_list(frame: &mut Frame, app: &mut AppState, area: Rect) {
    let title = match app.focus {
        PaneFocus::Trials => "Trials (focus)",
        _ => "Trials",
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    let visible_rows = inner.height as usize;
    let total_items = app.trials.len();
    if total_items > 0 && visible_rows > 0 {
        let offset = &mut app.trials_scroll.offset;
        if app.selected < *offset {
            *offset = app.selected;
        } else if app.selected >= *offset + visible_rows {
            *offset = app.selected + 1 - visible_rows;
        }
        app.trials_scroll.apply(total_items, visible_rows);
    } else {
        app.trials_scroll.reset();
    }

    let items = app
        .trials
        .iter()
        .skip(app.trials_scroll.offset)
        .take(visible_rows.max(1))
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
        let selected = app.selected.saturating_sub(app.trials_scroll.offset);
        state.select(Some(selected));
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().fg(Color::Yellow));
    frame.render_stateful_widget(list, area, &mut state);

    render_scrollbar(frame, inner, total_items, visible_rows, app.trials_scroll.offset);
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

fn draw_metrics_overview(frame: &mut Frame, app: &mut AppState, area: Rect) {
    let title = "Trial Metrics (Tab to switch focus)";
    let block = Block::default().borders(Borders::ALL).title(title);
    frame.render_widget(&block, area);
    let inner = block.inner(area);
    if inner.height == 0 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(inner);
    app.pane_rects.charts = chunks[0];
    app.pane_rects.details = chunks[1];

    if let Some(err) = app.last_error.as_ref() {
        let text = vec![Line::from(Span::styled(
            err.clone(),
            Style::default().fg(Color::Red),
        ))];
        frame.render_widget(Paragraph::new(text), inner);
        return;
    }

    let Some(trial) = app.trials.get(app.selected) else {
        frame.render_widget(Paragraph::new("No trials loaded."), inner);
        return;
    };
    let trial = trial.clone();
    let epochs = app
        .epoch_rows
        .get(&trial.trial_id)
        .cloned()
        .unwrap_or_default();
    if epochs.is_empty() {
        frame.render_widget(
            Paragraph::new("No epoch metrics for selected trial."),
            inner,
        );
        return;
    }
    draw_metric_charts(frame, app, &epochs, chunks[0]);
    draw_trial_details(frame, app, &trial, &epochs, chunks[1]);
}

fn draw_metric_charts(frame: &mut Frame, app: &mut AppState, epochs: &[TrialRow], area: Rect) {
    let title = match app.focus {
        PaneFocus::Charts => "Metric Curves (focus)",
        _ => "Metric Curves",
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    frame.render_widget(&block, area);
    let inner = block.inner(area);
    if inner.height == 0 {
        return;
    }

    let metric_keys = collect_metric_keys_for_epochs(epochs);
    if metric_keys.is_empty() {
        frame.render_widget(Paragraph::new("No numeric metric curves."), inner);
        return;
    }
    let x_axis = select_x_axis_spec(epochs);

    let item_height = 7u16;
    let visible_items = (inner.height / item_height).max(1) as usize;
    let total_items = metric_keys.len();
    app.metrics_scroll.apply(total_items, visible_items);

    for (row, key) in metric_keys
        .iter()
        .skip(app.metrics_scroll.offset)
        .take(visible_items)
        .enumerate()
    {
        let y = inner.y + (row as u16 * item_height);
        let rect = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: item_height,
        };
        let series = metric_series_for_key(epochs, key, &x_axis);
        let (min_x, max_x, min_y, max_y) = series_bounds(&series);
        let x_labels = axis_labels(min_x, max_x);
        let y_labels = axis_labels(min_y, max_y);
        let x_title = if x_axis.label == x_axis.unit {
            x_axis.label.to_string()
        } else {
            format!("{} ({})", x_axis.label, x_axis.unit)
        };
        let y_title = metric_axis_title(key);
        let title = if let Some(last) = series.last().map(|point| point.1) {
            format!("{key}  last={last:.4}")
        } else {
            key.clone()
        };
        let dataset = Dataset::default()
            .name(key.clone())
            .graph_type(GraphType::Line)
            .marker(Marker::Braille)
            .style(Style::default().fg(Color::Cyan))
            .data(&series);
        let chart = Chart::new(vec![dataset])
            .block(Block::default().borders(Borders::ALL).title(title))
            .x_axis(
                Axis::default()
                    .title(x_title)
                    .bounds([min_x, max_x])
                    .labels(x_labels),
            )
            .y_axis(
                Axis::default()
                    .title(y_title)
                    .bounds([min_y, max_y])
                    .labels(y_labels),
            );
        frame.render_widget(chart, rect);
    }

    render_scrollbar(
        frame,
        inner,
        total_items,
        visible_items,
        app.metrics_scroll.offset,
    );
}

fn draw_trial_details(
    frame: &mut Frame,
    app: &mut AppState,
    trial: &TrialRow,
    epochs: &[TrialRow],
    area: Rect,
) {
    let title = match app.focus {
        PaneFocus::Details => "Trial Details (focus)",
        _ => "Trial Details",
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    frame.render_widget(&block, area);
    let inner = block.inner(area);
    if inner.height == 0 {
        return;
    }

    let text = trial_detail_lines(trial, epochs);
    let total_lines = text.len();
    let visible_lines = inner.height as usize;
    app.details_scroll.apply(total_lines, visible_lines);
    let paragraph = Paragraph::new(text).scroll((app.details_scroll.offset as u16, 0));
    frame.render_widget(paragraph, inner);

    render_scrollbar(frame, inner, total_lines, visible_lines, app.details_scroll.offset);
}

fn collect_metric_keys_for_epochs(epochs: &[TrialRow]) -> Vec<String> {
    let mut keys = BTreeMap::new();
    for epoch in epochs {
        for (key, value) in &epoch.fields {
            if !key.starts_with("metric.") || key.as_str() == "metric" {
                continue;
            }
            if value.parse::<f64>().is_ok() {
                keys.entry(key.clone()).or_insert(());
            }
        }
    }
    keys.keys().cloned().collect()
}

fn metric_series_for_key(
    epochs: &[TrialRow],
    key: &str,
    x_axis: &XAxisSpec,
) -> Vec<(f64, f64)> {
    let mut series = Vec::with_capacity(epochs.len());
    for (idx, epoch) in epochs.iter().enumerate() {
        let value = epoch
            .fields
            .get(key)
            .and_then(|v| v.parse::<f64>().ok());
        let Some(value) = value else {
            continue;
        };
        let x = epoch_index(&epoch.fields, idx, x_axis);
        series.push((x, value));
    }
    series
}

fn epoch_index(fields: &BTreeMap<String, String>, fallback: usize, x_axis: &XAxisSpec) -> f64 {
    if let Some(key) = x_axis.key {
        if let Some(value) = fields.get(key)
            && let Ok(parsed) = value.parse::<f64>()
        {
            return parsed;
        }
    }
    (fallback + 1) as f64
}

fn series_bounds(series: &[(f64, f64)]) -> (f64, f64, f64, f64) {
    if series.is_empty() {
        return (0.0, 1.0, 0.0, 1.0);
    }
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (x, y) in series {
        min_x = min_x.min(*x);
        max_x = max_x.max(*x);
        min_y = min_y.min(*y);
        max_y = max_y.max(*y);
    }
    if min_x == max_x {
        max_x = min_x + 1.0;
    }
    if min_y == max_y {
        max_y = min_y + 1.0;
    }
    (min_x, max_x, min_y, max_y)
}

#[derive(Debug, Clone, Copy)]
struct XAxisSpec {
    key: Option<&'static str>,
    label: &'static str,
    unit: &'static str,
}

fn select_x_axis_spec(epochs: &[TrialRow]) -> XAxisSpec {
    let candidates = [
        XAxisSpec {
            key: Some("metric.time_s"),
            label: "time",
            unit: "s",
        },
        XAxisSpec {
            key: Some("metric.time_sec"),
            label: "time",
            unit: "s",
        },
        XAxisSpec {
            key: Some("metric.elapsed_s"),
            label: "time",
            unit: "s",
        },
        XAxisSpec {
            key: Some("metric.time_ms"),
            label: "time",
            unit: "ms",
        },
        XAxisSpec {
            key: Some("metric.elapsed_ms"),
            label: "time",
            unit: "ms",
        },
        XAxisSpec {
            key: Some("metric.epoch"),
            label: "epoch",
            unit: "epoch",
        },
        XAxisSpec {
            key: Some("metric.last_epoch"),
            label: "epoch",
            unit: "epoch",
        },
        XAxisSpec {
            key: Some("metric.step"),
            label: "step",
            unit: "step",
        },
        XAxisSpec {
            key: Some("metric.step_idx"),
            label: "step",
            unit: "step",
        },
    ];

    for candidate in candidates {
        if let Some(key) = candidate.key {
            if epochs.iter().any(|epoch| {
                epoch
                    .fields
                    .get(key)
                    .and_then(|value| value.parse::<f64>().ok())
                    .is_some()
            }) {
                return candidate;
            }
        }
    }

    XAxisSpec {
        key: None,
        label: "index",
        unit: "idx",
    }
}

fn axis_labels(min: f64, max: f64) -> Vec<Line<'static>> {
    let mid = (min + max) / 2.0;
    vec![
        Line::from(format_axis_value(min)),
        Line::from(format_axis_value(mid)),
        Line::from(format_axis_value(max)),
    ]
}

fn format_axis_value(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1000.0 {
        format!("{value:.0}")
    } else if abs >= 100.0 {
        format!("{value:.1}")
    } else if abs >= 1.0 {
        format!("{value:.2}")
    } else {
        format!("{value:.3}")
    }
}

fn metric_axis_title(key: &str) -> String {
    if let Some(unit) = metric_unit_from_key(key) {
        format!("{key} ({unit})")
    } else {
        key.to_string()
    }
}

fn metric_unit_from_key(key: &str) -> Option<&'static str> {
    if key.ends_with("_ms") || key.ends_with(".ms") || key.contains("time_ms") {
        return Some("ms");
    }
    if key.ends_with("_s") || key.ends_with(".s") || key.contains("time_s") {
        return Some("s");
    }
    if key.ends_with("_pct") || key.ends_with(".pct") || key.contains("percent") {
        return Some("%");
    }
    None
}

fn trial_detail_lines(trial: &TrialRow, epochs: &[TrialRow]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(format!("trial_id: {}", trial.trial_id)));
    lines.push(Line::from(format!("status: {}", trial.status)));
    lines.push(Line::from(format!("elapsed_ms: {}", trial.elapsed_ms)));
    lines.push(Line::from(format!("epochs_logged: {}", epochs.len())));
    if let Some(error) = &trial.error {
        lines.push(Line::from(format!("error: {}", error)));
    }
    if let Some(score) = trial.fields.get("score") {
        lines.push(Line::from(format!("score: {}", score)));
    }
    if let Some(metric_name) = trial.fields.get("metric") {
        lines.push(Line::from(format!("metric_key: {}", metric_name)));
    }

    let mut metric_fields: BTreeMap<String, String> = BTreeMap::new();
    let mut other_fields = Vec::new();
    for (key, value) in &trial.fields {
        if key.starts_with("metric.") && key.as_str() != "metric" {
            metric_fields.insert(key.clone(), value.clone());
        } else if !key.starts_with("metric") {
            other_fields.push(format!("{key} = {value}"));
        }
    }
    if let Some(last_epoch) = epochs.last() {
        for (key, value) in &last_epoch.fields {
            if key.starts_with("metric.") && key.as_str() != "metric" {
                metric_fields.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
    }
    other_fields.sort();

    if !metric_fields.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from("metrics:"));
        for (key, value) in metric_fields {
            lines.push(Line::from(format!("  {key} = {value}")));
        }
    }
    if !other_fields.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from("fields:"));
        for item in other_fields {
            lines.push(Line::from(format!("  {item}")));
        }
    }
    lines
}

fn render_scrollbar(
    frame: &mut Frame,
    area: Rect,
    total: usize,
    view: usize,
    offset: usize,
) {
    if total <= view || view == 0 || area.height == 0 {
        return;
    }
    let content_len = total.saturating_sub(view).saturating_add(1).max(1);
    let mut state = ScrollbarState::new(content_len)
        .position(offset.min(content_len.saturating_sub(1)))
        .viewport_content_length(view.max(1));
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    frame.render_stateful_widget(scrollbar, area, &mut state);
}
