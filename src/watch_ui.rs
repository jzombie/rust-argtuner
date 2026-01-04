use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use argtuner::constants::{FIELD_METRIC, FIELD_SCORE, HP_PREFIX};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton,
    MouseEventKind,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::{Constraint, Direction, Rect};
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    canvas::{Canvas, Line as CanvasLine},
    Axis, Block, Borders, Chart, Dataset, GraphType, Paragraph,
};
use ratatui::{Frame, Terminal};
use rusqlite::{Connection, OpenFlags};
use term_wm::components::{
    Component, ListComponent, ScrollView, StatusBar, ToggleItem, ToggleListComponent,
};
use term_wm::layout::{LayoutNode, TilingLayout};
use term_wm::runner::{run_app, HasWindowManager};
use term_wm::window::{rect_contains, WindowManager};

pub fn run(db_path: PathBuf, poll_ms: u64) -> io::Result<()> {
    let mut app = AppState {
        db_path,
        poll: Duration::from_millis(poll_ms),
        last_refresh: Instant::now()
            .checked_sub(Duration::from_secs(60))
            .unwrap_or_else(Instant::now),
        trials: Vec::new(),
        epoch_rows: BTreeMap::new(),
        last_error: None,
        windows: WindowManager::new(FocusTarget::Trials),
        trials_list: ListComponent::new("Trials"),
        charts_scroll: ScrollView::new(),
        details_scroll: ScrollView::new(),
        chart_zoom: 1.0,
        chart_view: ChartView::Summary,
        chart_selected: 0,
        metrics_len: 0,
        chart_mode: ChartMode::Metrics,
        params_list: ToggleListComponent::new("Hyperparameters"),
        params_x_offset: 0,
        metrics_list: ToggleListComponent::new("Metrics"),
        status_bar: StatusBar::new(),
        main_layout: TilingLayout::new(LayoutNode::split_resizable(
            Direction::Vertical,
            vec![Constraint::Min(1), Constraint::Length(1)],
            vec![
                LayoutNode::split(
                    Direction::Horizontal,
                    vec![Constraint::Percentage(40), Constraint::Percentage(60)],
                    vec![
                        LayoutNode::leaf(RegionId::Trials),
                        LayoutNode::leaf(RegionId::Charts),
                    ],
                ),
                LayoutNode::leaf(RegionId::Status),
            ],
            false,
        )),
        charts_layout: TilingLayout::new(LayoutNode::split(
            Direction::Vertical,
            vec![Constraint::Percentage(65), Constraint::Percentage(35)],
            vec![
                LayoutNode::leaf(RegionId::Charts),
                LayoutNode::leaf(RegionId::Details),
            ],
        )),
        details_layout: TilingLayout::new(LayoutNode::split(
            Direction::Horizontal,
            vec![Constraint::Percentage(55), Constraint::Percentage(45)],
            vec![
                LayoutNode::leaf(RegionId::ParamsInner),
                LayoutNode::leaf(RegionId::MetricsInner),
            ],
        )),
        main_area: Rect::default(),
        charts_area: Rect::default(),
        details_area: Rect::default(),
    };

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(
        &mut terminal,
        &mut app,
        &[
            RegionId::Trials,
            RegionId::Charts,
            RegionId::ParamsInner,
            RegionId::MetricsInner,
            RegionId::Details,
        ],
        map_focus_from_region,
        |_focus| None,
        Duration::from_millis(50),
        |frame, app| {
            if app.last_refresh.elapsed() >= app.poll {
                refresh_trials(app);
                app.last_refresh = Instant::now();
            }
            draw_ui(frame, app);
        },
        |event, app| handle_event(app, event),
        |event, _app| {
            matches!(
                event,
                Some(Event::Key(key)) if key.code == KeyCode::Char('q') || key.code == KeyCode::Esc
            )
        },
    );

    terminal::disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
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

type TrialEpochRows = BTreeMap<i64, Vec<TrialRow>>;
type TrialLoadResult = Result<(Vec<TrialRow>, TrialEpochRows), String>;

struct AppState {
    db_path: PathBuf,
    poll: Duration,
    last_refresh: Instant,
    trials: Vec<TrialRow>,
    epoch_rows: BTreeMap<i64, Vec<TrialRow>>,
    last_error: Option<String>,
    windows: WindowManager<FocusTarget, RegionId>,
    trials_list: ListComponent,
    charts_scroll: ScrollView,
    details_scroll: ScrollView,
    chart_zoom: f64,
    chart_view: ChartView,
    chart_selected: usize,
    metrics_len: usize,
    chart_mode: ChartMode,
    params_list: ToggleListComponent,
    params_x_offset: usize,
    metrics_list: ToggleListComponent,
    status_bar: StatusBar,
    main_layout: TilingLayout<RegionId>,
    charts_layout: TilingLayout<RegionId>,
    details_layout: TilingLayout<RegionId>,
    main_area: Rect,
    charts_area: Rect,
    details_area: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneFocus {
    Trials,
    Charts,
    Details,
}

const CHART_ITEM_HEIGHT: u16 = 7;
const PARAM_AXIS_WIDTH: u16 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
enum RegionId {
    Trials,
    Charts,
    Details,
    TrialsInner,
    ChartsInner,
    DetailsInner,
    ParamsInner,
    MetricsInner,
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FocusTarget {
    Trials,
    Charts,
    Details,
    DetailsParams,
    DetailsMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChartMode {
    Metrics,
    HyperParams,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChartView {
    Summary,
    Focused,
}

// ScrollState and region handling live in term_wm::window

impl HasWindowManager<FocusTarget, RegionId> for AppState {
    fn windows(&mut self) -> &mut WindowManager<FocusTarget, RegionId> {
        &mut self.windows
    }
}

fn map_focus_from_region(region: RegionId) -> FocusTarget {
    match region {
        RegionId::Trials => FocusTarget::Trials,
        RegionId::Charts => FocusTarget::Charts,
        RegionId::ParamsInner => FocusTarget::DetailsParams,
        RegionId::MetricsInner => FocusTarget::DetailsMetrics,
        RegionId::Details => FocusTarget::Details,
        _ => FocusTarget::Details,
    }
}

fn handle_event(app: &mut AppState, event: &Event) -> bool {
    match event {
        Event::Key(key) => {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    apply_focus_delta(app, -1);
                    true
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    apply_focus_delta(app, 1);
                    true
                }
                KeyCode::Char('h') => {
                    if pane_focus(app) == PaneFocus::Charts {
                        app.chart_mode = match app.chart_mode {
                            ChartMode::Metrics => ChartMode::HyperParams,
                            ChartMode::HyperParams => ChartMode::Metrics,
                        };
                        if app.chart_mode == ChartMode::HyperParams
                            && pane_focus(app) == PaneFocus::Details
                        {
                            app.windows.set_focus(FocusTarget::DetailsParams);
                        } else if app.chart_mode == ChartMode::Metrics
                            && matches!(
                                app.windows.focus(),
                                FocusTarget::DetailsParams | FocusTarget::DetailsMetrics
                            )
                        {
                            app.windows.set_focus(FocusTarget::Details);
                        }
                    }
                    true
                }
                KeyCode::Char('f') => {
                    if pane_focus(app) == PaneFocus::Charts
                        && app.chart_mode == ChartMode::Metrics
                    {
                        toggle_chart_view(app);
                        true
                    } else {
                        false
                    }
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if pane_focus(app) == PaneFocus::Charts
                        && app.chart_mode == ChartMode::Metrics
                    {
                        toggle_chart_view(app);
                        true
                    } else if pane_focus(app) == PaneFocus::Details
                        && app.chart_mode == ChartMode::HyperParams
                    {
                        match app.windows.focus() {
                            FocusTarget::DetailsParams => toggle_param_selected(app),
                            FocusTarget::DetailsMetrics => toggle_metric_selected(app),
                            _ => toggle_param_selected(app),
                        }
                        true
                    } else {
                        false
                    }
                }
                KeyCode::Char('+') | KeyCode::Char('=') => {
                    if pane_focus(app) == PaneFocus::Charts
                        && app.chart_mode == ChartMode::Metrics
                    {
                        zoom_charts(app, 0.8);
                        true
                    } else {
                        false
                    }
                }
                KeyCode::Char('-') => {
                    if pane_focus(app) == PaneFocus::Charts
                        && app.chart_mode == ChartMode::Metrics
                    {
                        zoom_charts(app, 1.25);
                        true
                    } else {
                        false
                    }
                }
                KeyCode::Char('0') => {
                    if pane_focus(app) == PaneFocus::Charts
                        && app.chart_mode == ChartMode::Metrics
                    {
                        reset_chart_zoom(app);
                        true
                    } else {
                        false
                    }
                }
                KeyCode::PageDown | KeyCode::Char(']') => {
                    apply_focus_delta(app, 5);
                    true
                }
                KeyCode::PageUp | KeyCode::Char('[') => {
                    apply_focus_delta(app, -5);
                    true
                }
                KeyCode::Right => {
                    if app.chart_mode == ChartMode::HyperParams
                        && pane_focus(app) == PaneFocus::Charts
                    {
                        pan_params(app, 1);
                        true
                    } else {
                        false
                    }
                }
                KeyCode::Left => {
                    if app.chart_mode == ChartMode::HyperParams
                        && pane_focus(app) == PaneFocus::Charts
                    {
                        pan_params(app, -1);
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            }
        }
        Event::Mouse(mouse) => {
            if app.details_layout.handle_event(event, app.details_area)
                || app.charts_layout.handle_event(event, app.charts_area)
                || app.main_layout.handle_event(event, app.main_area)
            {
                return true;
            }
            if app.trials_list.handle_event(event) {
                return true;
            }
            if app.chart_mode == ChartMode::HyperParams {
                if app.params_list.handle_event(event) || app.metrics_list.handle_event(event) {
                    return true;
                }
            }
            if app.details_scroll.handle_event(event).handled
                || app.charts_scroll.handle_event(event).handled
            {
                return true;
            }
            match mouse.kind {
            MouseEventKind::ScrollUp => {
                let delta = -1;
                if let Some(pane) = pane_for_mouse(app, mouse.column, mouse.row) {
                    if mouse.modifiers.contains(KeyModifiers::CONTROL)
                        && pane == PaneFocus::Charts
                        && app.chart_mode == ChartMode::Metrics
                    {
                        zoom_charts(app, 0.8);
                    } else {
                        if pane == PaneFocus::Details && app.chart_mode == ChartMode::HyperParams {
                            update_details_focus_for_mouse(app, mouse.column, mouse.row);
                        }
                        apply_delta_for_pane(app, pane, delta);
                    }
                } else {
                    apply_focus_delta(app, delta);
                }
                true
            }
            MouseEventKind::ScrollDown => {
                let delta = 1;
                if let Some(pane) = pane_for_mouse(app, mouse.column, mouse.row) {
                    if mouse.modifiers.contains(KeyModifiers::CONTROL)
                        && pane == PaneFocus::Charts
                        && app.chart_mode == ChartMode::Metrics
                    {
                        zoom_charts(app, 1.25);
                    } else {
                        if pane == PaneFocus::Details && app.chart_mode == ChartMode::HyperParams {
                            update_details_focus_for_mouse(app, mouse.column, mouse.row);
                        }
                        apply_delta_for_pane(app, pane, delta);
                    }
                } else {
                    apply_focus_delta(app, delta);
                }
                true
            }
            MouseEventKind::ScrollLeft => {
                if app.chart_mode == ChartMode::HyperParams
                    && pane_for_mouse(app, mouse.column, mouse.row) == Some(PaneFocus::Charts)
                {
                    pan_params(app, -1);
                    true
                } else {
                    false
                }
            }
            MouseEventKind::ScrollRight => {
                if app.chart_mode == ChartMode::HyperParams
                    && pane_for_mouse(app, mouse.column, mouse.row) == Some(PaneFocus::Charts)
                {
                    pan_params(app, 1);
                    true
                } else {
                    false
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(pane) = pane_for_mouse(app, mouse.column, mouse.row) {
                    if pane == PaneFocus::Trials {
                        handle_trial_click(app, mouse.column, mouse.row);
                    } else if pane == PaneFocus::Charts {
                        if app.chart_mode == ChartMode::Metrics {
                            handle_chart_click(app, mouse.column, mouse.row);
                        }
                    } else if pane == PaneFocus::Details && app.chart_mode == ChartMode::HyperParams
                    {
                        update_details_focus_for_mouse(app, mouse.column, mouse.row);
                        handle_details_click(app, mouse.column, mouse.row);
                    }
                    true
                } else {
                    false
                }
            }
            _ => false,
            }
        }
        _ => false,
    }
}

fn apply_focus_delta(app: &mut AppState, delta: isize) {
    apply_delta_for_pane(app, pane_focus(app), delta);
}

fn focus_targets(app: &AppState) -> Vec<FocusTarget> {
    match app.chart_mode {
        ChartMode::HyperParams => vec![
            FocusTarget::Trials,
            FocusTarget::Charts,
            FocusTarget::DetailsParams,
            FocusTarget::DetailsMetrics,
        ],
        ChartMode::Metrics => vec![
            FocusTarget::Trials,
            FocusTarget::Charts,
            FocusTarget::Details,
        ],
    }
}

fn pane_focus(app: &AppState) -> PaneFocus {
    match app.windows.focus() {
        FocusTarget::Trials => PaneFocus::Trials,
        FocusTarget::Charts => PaneFocus::Charts,
        FocusTarget::Details | FocusTarget::DetailsParams | FocusTarget::DetailsMetrics => {
            PaneFocus::Details
        }
    }
}

fn apply_delta_for_pane(app: &mut AppState, pane: PaneFocus, delta: isize) {
    match pane {
        PaneFocus::Trials => move_trial_selection(app, delta),
        PaneFocus::Charts => {
            if app.chart_mode == ChartMode::HyperParams {
                pan_params(app, delta);
            } else if app.chart_view == ChartView::Focused {
                move_chart_selection(app, delta);
            } else {
                app.charts_scroll.bump(delta);
            }
        }
        PaneFocus::Details => {
            if app.chart_mode == ChartMode::HyperParams {
                match app.windows.focus() {
                    FocusTarget::DetailsMetrics => move_metric_selection(app, delta),
                    _ => move_param_selection(app, delta),
                }
            } else {
                app.details_scroll.bump(delta);
            }
        }
    }
}

fn zoom_charts(app: &mut AppState, factor: f64) {
    let next = (app.chart_zoom * factor).clamp(0.1, 1.0);
    app.chart_zoom = next;
}

fn reset_chart_zoom(app: &mut AppState) {
    app.chart_zoom = 1.0;
}

fn toggle_chart_view(app: &mut AppState) {
    app.chart_view = match app.chart_view {
        ChartView::Summary => {
            if app.metrics_len == 0 {
                ChartView::Summary
            } else {
                let offset = app.charts_scroll.offset();
                app.chart_selected = offset.min(app.metrics_len - 1);
                ChartView::Focused
            }
        }
        ChartView::Focused => {
            if app.metrics_len > 0 {
                app.charts_scroll
                    .set_offset(app.chart_selected.min(app.metrics_len - 1));
            }
            ChartView::Summary
        }
    };
}

fn move_chart_selection(app: &mut AppState, delta: isize) {
    if app.metrics_len == 0 {
        return;
    }
    let max = (app.metrics_len - 1) as isize;
    let current = app.chart_selected as isize;
    let next = (current + delta).clamp(0, max) as usize;
    app.chart_selected = next;
}

fn pan_params(app: &mut AppState, delta: isize) {
    let enabled_len = enabled_axis_count(app);
    if enabled_len == 0 {
        app.params_x_offset = 0;
        return;
    }
    let max = enabled_len.saturating_sub(1) as isize;
    let current = app.params_x_offset as isize;
    let next = (current + delta).clamp(0, max) as usize;
    app.params_x_offset = next;
}

fn move_param_selection(app: &mut AppState, delta: isize) {
    app.params_list.move_selection(delta);
}

fn move_metric_selection(app: &mut AppState, delta: isize) {
    app.metrics_list.move_selection(delta);
}

fn toggle_param_selected(app: &mut AppState) {
    let _ = app.params_list.toggle_selected();
}

fn toggle_metric_selected(app: &mut AppState) {
    let _ = app.metrics_list.toggle_selected();
}

fn update_details_focus_for_mouse(app: &mut AppState, column: u16, row: u16) {
    if rect_contains(app.windows.region(RegionId::ParamsInner), column, row) {
        app.windows.set_focus(FocusTarget::DetailsParams);
    } else if rect_contains(app.windows.region(RegionId::MetricsInner), column, row) {
        app.windows.set_focus(FocusTarget::DetailsMetrics);
    }
}

fn pane_for_mouse(app: &AppState, column: u16, row: u16) -> Option<PaneFocus> {
    if rect_contains(app.windows.region(RegionId::Trials), column, row) {
        return Some(PaneFocus::Trials);
    }
    if rect_contains(app.windows.region(RegionId::Charts), column, row) {
        return Some(PaneFocus::Charts);
    }
    if rect_contains(app.windows.region(RegionId::Details), column, row) {
        return Some(PaneFocus::Details);
    }
    None
}

fn handle_trial_click(app: &mut AppState, column: u16, row: u16) {
    let inner = app.windows.region(RegionId::TrialsInner);
    if inner.height == 0 || !rect_contains(inner, column, row) {
        return;
    }
    let offset_row = row.saturating_sub(inner.y) as usize;
    let index = app
        .trials_list
        .scroll_offset()
        .saturating_add(offset_row);
    if index >= app.trials.len() {
        return;
    }
    if index != app.trials_list.selected() {
        app.trials_list.set_selected(index);
        app.charts_scroll.reset();
        app.details_scroll.reset();
    }
}

fn handle_chart_click(app: &mut AppState, column: u16, row: u16) {
    if app.chart_view == ChartView::Focused {
        return;
    }
    let inner = app.windows.region(RegionId::ChartsInner);
    if inner.height == 0 || !rect_contains(inner, column, row) {
        return;
    }
    let offset_row = row.saturating_sub(inner.y) as usize;
    let index_in_view = offset_row / CHART_ITEM_HEIGHT as usize;
    let index = app
        .charts_scroll
        .offset()
        .saturating_add(index_in_view);
    if index >= app.metrics_len {
        return;
    }
    app.chart_selected = index;
    app.chart_view = ChartView::Focused;
}

fn handle_details_click(app: &mut AppState, column: u16, row: u16) {
    if matches!(app.windows.focus(), FocusTarget::DetailsParams) {
        let inner = app.windows.region(RegionId::ParamsInner);
        if inner.height == 0 || !rect_contains(inner, column, row) {
            return;
        }
        let offset_row = row.saturating_sub(inner.y) as usize;
        let index = app
            .params_list
            .scroll_offset()
            .saturating_add(offset_row);
        if index >= app.params_list.items().len() {
            return;
        }
        app.params_list.set_selected(index);
        let _ = app.params_list.toggle_selected();
    } else {
        let inner = app.windows.region(RegionId::MetricsInner);
        if inner.height == 0 || !rect_contains(inner, column, row) {
            return;
        }
        let offset_row = row.saturating_sub(inner.y) as usize;
        let index = app
            .metrics_list
            .scroll_offset()
            .saturating_add(offset_row);
        if index >= app.metrics_list.items().len() {
            return;
        }
        app.metrics_list.set_selected(index);
        let _ = app.metrics_list.toggle_selected();
    }
}

fn move_trial_selection(app: &mut AppState, delta: isize) {
    let before = app.trials_list.selected();
    app.trials_list.move_selection(delta);
    if app.trials_list.selected() != before {
        app.charts_scroll.reset();
        app.details_scroll.reset();
    }
}

fn refresh_trials(app: &mut AppState) {
    match load_trials(&app.db_path) {
        Ok((trials, epoch_rows)) => {
            app.trials = trials;
            app.epoch_rows = epoch_rows;
            let before = app.trials_list.selected();
            app.trials_list.set_items(build_trial_items(&app.trials));
            if app.trials.is_empty() {
                app.trials_list.set_selected(0);
            }
            if app.trials_list.selected() != before {
                app.charts_scroll.reset();
                app.details_scroll.reset();
            }
            sync_param_toggles(app);
            app.last_error = None;
        }
        Err(err) => {
            app.last_error = Some(err);
        }
    }
}

fn sync_param_toggles(app: &mut AppState) {
    let mut names = BTreeSet::new();
    let mut metric_names = BTreeSet::new();
    for trial in &app.trials {
        for key in trial.fields.keys() {
            if key.starts_with(HP_PREFIX) {
                names.insert(key.clone());
                continue;
            }
            if key.starts_with("metric.") && key.as_str() != FIELD_METRIC {
                metric_names.insert(key.clone());
                continue;
            }
            if key.as_str() == FIELD_SCORE {
                metric_names.insert(key.clone());
            }
        }
    }
    let mut existing = BTreeMap::new();
    for param in app.params_list.items() {
        existing.insert(param.id.clone(), param.checked);
    }
    let mut existing_metrics = BTreeMap::new();
    for metric in app.metrics_list.items() {
        existing_metrics.insert(metric.id.clone(), metric.checked);
    }
    let main_metric = app
        .trials
        .get(app.trials_list.selected())
        .and_then(|trial| trial.fields.get(FIELD_METRIC))
        .map(|value| format!("metric.{value}"));
    let mut params = Vec::with_capacity(names.len());
    for name in names {
        let enabled = existing.get(&name).copied().unwrap_or(true);
        params.push(ToggleItem {
            id: name.clone(),
            label: display_axis_name(&name),
            checked: enabled,
        });
    }
    let mut metrics = Vec::with_capacity(metric_names.len());
    for name in metric_names {
        let default_enabled = main_metric
            .as_ref()
            .map(|metric| metric == &name)
            .unwrap_or(name == FIELD_SCORE);
        let enabled = existing_metrics
            .get(&name)
            .copied()
            .unwrap_or(default_enabled);
        metrics.push(ToggleItem {
            id: name.clone(),
            label: display_axis_name(&name),
            checked: enabled,
        });
    }
    app.params_list.set_items(params);
    app.metrics_list.set_items(metrics);
    if app.params_x_offset > app.params_list.items().len().saturating_sub(1) {
        app.params_x_offset = 0;
    }
}

fn load_trials(path: &Path) -> TrialLoadResult {
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
    app.main_area = area;
    app.windows.set_focus_order(focus_targets(app));
    app.windows
        .register_tiling_layout(&app.main_layout, area);
    let trials_area = app.windows.region(RegionId::Trials);
    let charts_area = app.windows.region(RegionId::Charts);
    draw_trial_list(frame, app, trials_area);
    draw_metrics_overview(frame, app, charts_area);
    draw_status_bar(frame, app, app.windows.region(RegionId::Status));
}

fn draw_status_bar(frame: &mut Frame, app: &mut AppState, area: Rect) {
    let selected = if app.trials.is_empty() {
        "-".to_string()
    } else {
        format!("{}", app.trials_list.selected() + 1)
    };
    let mode = match app.chart_mode {
        ChartMode::Metrics => "metrics",
        ChartMode::HyperParams => "hyperparams",
    };
    let left = format!(
        "trials: {}  selected: {}  view: {}",
        app.trials.len(),
        selected,
        mode
    );
    let right = "Tab/Shift-Tab focus | h metrics/params | f focus | +/- zoom | q quit";
    app.status_bar.set_left(left);
    app.status_bar.set_right(right);
    app.status_bar.render(frame, area, false);
}

fn draw_trial_list(frame: &mut Frame, app: &mut AppState, area: Rect) {
    let focus = pane_focus(app) == PaneFocus::Trials;
    let inner = Block::default().borders(Borders::ALL).inner(area);
    app.windows.set_region(RegionId::TrialsInner, inner);
    app.trials_list.render(frame, area, focus);
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

fn build_trial_items(trials: &[TrialRow]) -> Vec<String> {
    trials
        .iter()
        .map(|trial| {
            let metric_text = metric_value_text(trial).unwrap_or_else(|| "-".to_string());
            format!(
                "trial {:>3}  {:<7}  {}",
                trial.trial_id, trial.status, metric_text
            )
        })
        .collect()
}

fn draw_metrics_overview(frame: &mut Frame, app: &mut AppState, area: Rect) {
    let title = match app.chart_mode {
        ChartMode::Metrics => {
            "Trial Metrics (Tab/Shift-Tab to switch focus, +/- to zoom, f to focus, h for params)"
        }
        ChartMode::HyperParams => "Hyperparameters (Tab/Shift-Tab to switch focus, h for metrics)",
    };
    let focus = pane_focus(app);
    let block = styled_block(title, focus == PaneFocus::Charts);
    frame.render_widget(&block, area);
    let inner = block.inner(area);
    if inner.height == 0 {
        app.charts_area = Rect::default();
        return;
    }
    app.charts_area = inner;

    app.windows
        .register_tiling_layout(&app.charts_layout, inner);
    app.windows.set_region(RegionId::ChartsInner, Rect::default());
    app.windows.set_region(RegionId::DetailsInner, Rect::default());
    app.windows.set_region(RegionId::ParamsInner, Rect::default());
    app.windows.set_region(RegionId::MetricsInner, Rect::default());
    app.metrics_len = 0;

    if let Some(err) = app.last_error.as_ref() {
        let text = vec![Line::from(Span::styled(
            err.clone(),
            Style::default().fg(Color::Red),
        ))];
        frame.render_widget(Paragraph::new(text), inner);
        return;
    }

    let Some(trial) = app.trials.get(app.trials_list.selected()) else {
        frame.render_widget(Paragraph::new("No trials loaded."), inner);
        return;
    };
    let trial = trial.clone();
    let epochs = app
        .epoch_rows
        .get(&trial.trial_id)
        .cloned()
        .unwrap_or_default();
    match app.chart_mode {
        ChartMode::Metrics => {
            if epochs.is_empty() {
                frame.render_widget(
                    Paragraph::new("No epoch metrics for selected trial."),
                    inner,
                );
                return;
            }
            let charts_area = app.windows.region(RegionId::Charts);
            let details_area = app.windows.region(RegionId::Details);
            draw_metric_charts(frame, app, &epochs, charts_area);
            draw_trial_details(frame, app, &trial, &epochs, details_area);
        }
        ChartMode::HyperParams => {
            let charts_area = app.windows.region(RegionId::Charts);
            let details_area = app.windows.region(RegionId::Details);
            draw_hyperparam_space(frame, app, charts_area);
            draw_param_toggles(frame, app, details_area);
        }
    }
}

fn draw_metric_charts(frame: &mut Frame, app: &mut AppState, epochs: &[TrialRow], area: Rect) {
    let metric_keys = collect_metric_keys_for_epochs(epochs);
    app.metrics_len = metric_keys.len();

    let focus = pane_focus(app);
    let title = match (focus, app.chart_view, app.metrics_len) {
        (PaneFocus::Charts, ChartView::Focused, total) if total > 0 => {
            let current = app.chart_selected.saturating_add(1);
            format!("Metric Curve {current}/{total} (focus view)")
        }
        (PaneFocus::Charts, ChartView::Focused, _) => "Metric Curve (focus view)".to_string(),
        (PaneFocus::Charts, ChartView::Summary, _) => "Metric Curves (focus)".to_string(),
        (_, ChartView::Focused, total) if total > 0 => {
            let current = app.chart_selected.saturating_add(1);
            format!("Metric Curve {current}/{total} (focus view)")
        }
        (_, ChartView::Focused, _) => "Metric Curve (focus view)".to_string(),
        _ => "Metric Curves".to_string(),
    };
    let block = styled_block(title, focus == PaneFocus::Charts);
    frame.render_widget(&block, area);
    let inner = block.inner(area);
    app.windows.set_region(RegionId::ChartsInner, inner);
    if inner.height == 0 {
        return;
    }
    if metric_keys.is_empty() {
        frame.render_widget(Paragraph::new("No numeric metric curves."), inner);
        return;
    }
    if app.chart_selected >= metric_keys.len() {
        app.chart_selected = metric_keys.len().saturating_sub(1);
    }
    let x_axis = select_x_axis_spec(epochs);

    match app.chart_view {
        ChartView::Summary => {
            let visible_items = (inner.height / CHART_ITEM_HEIGHT).max(1) as usize;
            let total_items = metric_keys.len();
            app.charts_scroll.update(inner, total_items, visible_items);

            for (row, key) in metric_keys
                .iter()
                .skip(app.charts_scroll.offset())
                .take(visible_items)
                .enumerate()
            {
                let y = inner.y + (row as u16 * CHART_ITEM_HEIGHT);
                let rect = Rect {
                    x: inner.x,
                    y,
                    width: inner.width,
                    height: CHART_ITEM_HEIGHT,
                };
                render_metric_chart(frame, epochs, key, rect, &x_axis, app.chart_zoom);
            }

            app.charts_scroll.render(frame);
        }
        ChartView::Focused => {
            let key = &metric_keys[app.chart_selected];
            render_metric_chart(frame, epochs, key, inner, &x_axis, app.chart_zoom);
        }
    }
}

fn draw_hyperparam_space(frame: &mut Frame, app: &mut AppState, area: Rect) {
    let focus = pane_focus(app);
    let title = match focus {
        PaneFocus::Charts => "Hyperparameter Space (loss color, focus, L/R pan axes)",
        _ => "Hyperparameter Space (loss color, L/R pan axes)",
    };
    let block = styled_block(title, focus == PaneFocus::Charts);
    frame.render_widget(&block, area);
    let inner = block.inner(area);
    app.windows.set_region(RegionId::ChartsInner, inner);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let enabled_axes = enabled_axes(app);
    let enabled: Vec<&AxisKey> = enabled_axes.iter().collect();
    if enabled.is_empty() {
        frame.render_widget(Paragraph::new("No axes enabled."), inner);
        return;
    }

    let max_visible = (inner.width / PARAM_AXIS_WIDTH).max(1) as usize;
    let enabled_len = enabled.len();
    if app.params_x_offset > enabled_len.saturating_sub(1) {
        app.params_x_offset = 0;
    }
    let start = app.params_x_offset.min(enabled_len.saturating_sub(1));
    let end = (start + max_visible).min(enabled_len);
    let visible = &enabled[start..end];
    let visible_len = visible.len();
    if visible_len == 0 {
        frame.render_widget(Paragraph::new("No parameters in view."), inner);
        return;
    }

    let domains = visible
        .iter()
        .map(|axis| param_domain(&app.trials, &axis.name))
        .collect::<Vec<_>>();
    let objective_range = objective_range(&app.trials);
    let x_max = if visible_len == 1 {
        1.0
    } else {
        (visible_len - 1) as f64
    };
    let max_labels = (inner.width / (PARAM_AXIS_WIDTH * 2)).max(1) as usize;
    let label_stride = visible_len.div_ceil(max_labels);
    let canvas = Canvas::default()
        .x_bounds([0.0, x_max])
        .y_bounds([0.0, 1.0])
        .paint(|ctx| {
            for (idx, axis) in visible.iter().enumerate() {
                let x = idx as f64;
                ctx.draw(&CanvasLine::new(x, 0.0, x, 1.0, Color::DarkGray));
                if idx % label_stride == 0 {
                    let label = short_axis_label(&axis.name, 10);
                    ctx.print(
                        x,
                        1.0,
                        Line::from(Span::styled(label, Style::default().fg(Color::White))),
                    );
                }
            }

            for (trial_idx, trial) in app.trials.iter().enumerate() {
                let color = if trial_idx == app.trials_list.selected() {
                    Color::Yellow
                } else {
                    color_for_objective(objective_value(trial), objective_range)
                };
                let mut last: Option<(f64, f64)> = None;
                for (idx, (axis, domain)) in visible.iter().zip(domains.iter()).enumerate() {
                    let Some(value) = trial.fields.get(&axis.name) else {
                        last = None;
                        continue;
                    };
                    let Some(y) = normalize_param_value(value, domain) else {
                        last = None;
                        continue;
                    };
                    let x = idx as f64;
                    if let Some((prev_x, prev_y)) = last {
                        ctx.draw(&CanvasLine::new(prev_x, prev_y, x, y, color));
                    }
                    last = Some((x, y));
                }
            }
        });
    frame.render_widget(canvas, inner);
}

fn render_metric_chart(
    frame: &mut Frame,
    epochs: &[TrialRow],
    key: &str,
    area: Rect,
    x_axis: &XAxisSpec,
    zoom: f64,
) {
    let series = metric_series_for_key(epochs, key, x_axis);
    let (min_x, max_x, min_y, max_y) = zoomed_series_bounds(&series, zoom);
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
        key.to_string()
    };
    let dataset = Dataset::default()
        .name(key.to_string())
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
    frame.render_widget(chart, area);
}

fn draw_trial_details(
    frame: &mut Frame,
    app: &mut AppState,
    trial: &TrialRow,
    epochs: &[TrialRow],
    area: Rect,
) {
    if app.chart_mode == ChartMode::HyperParams {
        draw_param_toggles(frame, app, area);
        return;
    }
    app.details_area = Rect::default();
    let focus = pane_focus(app);
    let title = match focus {
        PaneFocus::Details => "Trial Details (focus)",
        _ => "Trial Details",
    };
    let block = styled_block(title, focus == PaneFocus::Details);
    frame.render_widget(&block, area);
    let inner = block.inner(area);
    if inner.height == 0 {
        return;
    }

    let text = trial_detail_lines(trial, epochs);
    let total_lines = text.len();
    let visible_lines = inner.height as usize;
    app.details_scroll.update(inner, total_lines, visible_lines);
    let paragraph = Paragraph::new(text)
        .scroll((app.details_scroll.offset() as u16, 0));
    frame.render_widget(paragraph, inner);

    app.details_scroll.render(frame);
}

fn draw_param_toggles(frame: &mut Frame, app: &mut AppState, area: Rect) {
    app.details_area = area;
    app.windows
        .register_tiling_layout(&app.details_layout, area);
    let params_area = app.windows.region(RegionId::ParamsInner);
    let metrics_area = app.windows.region(RegionId::MetricsInner);

    app.windows.set_region(RegionId::DetailsInner, area);
    app.windows
        .set_region(RegionId::ParamsInner, Block::default().borders(Borders::ALL).inner(params_area));
    app.windows
        .set_region(RegionId::MetricsInner, Block::default().borders(Borders::ALL).inner(metrics_area));

    let params_focused = pane_focus(app) == PaneFocus::Details
        && matches!(app.windows.focus(), FocusTarget::DetailsParams);
    let metrics_focused = pane_focus(app) == PaneFocus::Details
        && matches!(app.windows.focus(), FocusTarget::DetailsMetrics);

    app.params_list.render(frame, params_area, params_focused);
    app.metrics_list.render(frame, metrics_area, metrics_focused);
}

fn styled_block<T: Into<Line<'static>>>(title: T, focused: bool) -> Block<'static> {
    let base = Block::default().borders(Borders::ALL).title(title);
    if focused {
        base.border_style(Style::default().fg(Color::Green))
    } else {
        base
    }
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

fn metric_series_for_key(epochs: &[TrialRow], key: &str, x_axis: &XAxisSpec) -> Vec<(f64, f64)> {
    let mut series = Vec::with_capacity(epochs.len());
    for (idx, epoch) in epochs.iter().enumerate() {
        let value = epoch.fields.get(key).and_then(|v| v.parse::<f64>().ok());
        let Some(value) = value else {
            continue;
        };
        let x = epoch_index(&epoch.fields, idx, x_axis);
        series.push((x, value));
    }
    series
}

fn epoch_index(fields: &BTreeMap<String, String>, fallback: usize, x_axis: &XAxisSpec) -> f64 {
    if let Some(key) = x_axis.key
        && let Some(value) = fields.get(key)
        && let Ok(parsed) = value.parse::<f64>()
    {
        return parsed;
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

fn zoomed_series_bounds(series: &[(f64, f64)], zoom: f64) -> (f64, f64, f64, f64) {
    let (full_min_x, full_max_x, full_min_y, full_max_y) = series_bounds(series);
    if series.is_empty() || zoom >= 1.0 {
        return (full_min_x, full_max_x, full_min_y, full_max_y);
    }
    let full_range = full_max_x - full_min_x;
    if full_range <= 0.0 {
        return (full_min_x, full_max_x, full_min_y, full_max_y);
    }
    let window = (full_range * zoom).max(1e-6);
    let mut max_x = full_max_x;
    let mut min_x = max_x - window;
    if min_x < full_min_x {
        min_x = full_min_x;
        max_x = (min_x + window).min(full_max_x);
    }

    if let Some((min_y, max_y)) = series_y_bounds_in_range(series, min_x, max_x) {
        (min_x, max_x, min_y, max_y)
    } else {
        (full_min_x, full_max_x, full_min_y, full_max_y)
    }
}

fn series_y_bounds_in_range(series: &[(f64, f64)], min_x: f64, max_x: f64) -> Option<(f64, f64)> {
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (x, y) in series {
        if *x < min_x || *x > max_x {
            continue;
        }
        min_y = min_y.min(*y);
        max_y = max_y.max(*y);
    }
    if min_y.is_finite() && max_y.is_finite() {
        if min_y == max_y {
            Some((min_y, max_y + 1.0))
        } else {
            Some((min_y, max_y))
        }
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy)]
struct XAxisSpec {
    key: Option<&'static str>,
    label: &'static str,
    unit: &'static str,
}

#[derive(Debug, Clone)]
enum ParamDomain {
    Numeric { min: f64, max: f64 },
    Categorical { categories: Vec<String> },
}

#[derive(Debug, Clone)]
struct AxisKey {
    name: String,
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
        if let Some(key) = candidate.key
            && epochs.iter().any(|epoch| {
                epoch
                    .fields
                    .get(key)
                    .and_then(|value| value.parse::<f64>().ok())
                    .is_some()
            })
        {
            return candidate;
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

fn objective_value(trial: &TrialRow) -> Option<f64> {
    if let Some((_, value)) = metric_for_trial(trial) {
        return Some(value);
    }
    trial
        .fields
        .get(FIELD_SCORE)
        .and_then(|v| v.parse::<f64>().ok())
}

fn objective_range(trials: &[TrialRow]) -> Option<(f64, f64)> {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut found = false;
    for trial in trials {
        let Some(value) = objective_value(trial) else {
            continue;
        };
        found = true;
        min = min.min(value);
        max = max.max(value);
    }
    if !found {
        return None;
    }
    if min == max {
        Some((min, max + 1.0))
    } else {
        Some((min, max))
    }
}

fn color_for_objective(value: Option<f64>, range: Option<(f64, f64)>) -> Color {
    let Some(value) = value else {
        return Color::DarkGray;
    };
    let Some((min, max)) = range else {
        return Color::DarkGray;
    };
    if max <= min {
        return Color::Cyan;
    }
    let t = ((value - min) / (max - min)).clamp(0.0, 1.0);
    let steps = [
        Color::Blue,
        Color::Cyan,
        Color::Green,
        Color::Yellow,
        Color::Red,
    ];
    let idx = ((steps.len() - 1) as f64 * t).round() as usize;
    steps[idx.min(steps.len() - 1)]
}

fn param_domain(trials: &[TrialRow], name: &str) -> ParamDomain {
    let mut raw_values = Vec::new();
    let mut numeric_values = Vec::new();
    let mut has_non_numeric = false;
    for trial in trials {
        let Some(value) = trial.fields.get(name) else {
            continue;
        };
        raw_values.push(value.clone());
        if let Ok(parsed) = value.parse::<f64>() {
            numeric_values.push(parsed);
        } else {
            has_non_numeric = true;
        }
    }

    if raw_values.is_empty() {
        return ParamDomain::Numeric { min: 0.0, max: 1.0 };
    }

    if !has_non_numeric && !numeric_values.is_empty() {
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for value in numeric_values {
            min = min.min(value);
            max = max.max(value);
        }
        if min == max {
            max = min + 1.0;
        }
        return ParamDomain::Numeric { min, max };
    }

    let mut set = BTreeSet::new();
    for value in raw_values {
        set.insert(value);
    }
    let categories = set.into_iter().collect::<Vec<_>>();
    ParamDomain::Categorical { categories }
}

fn normalize_param_value(value: &str, domain: &ParamDomain) -> Option<f64> {
    match domain {
        ParamDomain::Numeric { min, max } => {
            let parsed = value.parse::<f64>().ok()?;
            if max == min {
                Some(0.5)
            } else {
                Some((parsed - min) / (max - min))
            }
        }
        ParamDomain::Categorical { categories } => {
            if categories.is_empty() {
                return None;
            }
            let index = categories.iter().position(|item| item == value)?;
            if categories.len() == 1 {
                Some(0.5)
            } else {
                Some(index as f64 / (categories.len() - 1) as f64)
            }
        }
    }
}

fn truncate_label(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        return value.to_string();
    }
    if max_len <= 3 {
        return value.chars().take(max_len).collect();
    }
    let head: String = value.chars().take(max_len - 3).collect();
    format!("{head}...")
}

fn short_param_label(value: &str, max_len: usize) -> String {
    let trimmed = value.strip_prefix(HP_PREFIX).unwrap_or(value);
    let tail = trimmed.rsplit('.').next().unwrap_or(trimmed);
    let label = if tail.len() >= 3 { tail } else { trimmed };
    truncate_label(label, max_len)
}

fn short_axis_label(value: &str, max_len: usize) -> String {
    if value.starts_with(HP_PREFIX) {
        return short_param_label(value, max_len);
    }
    if let Some(stripped) = value.strip_prefix("metric.") {
        return truncate_label(stripped, max_len);
    }
    truncate_label(value, max_len)
}

fn display_axis_name(value: &str) -> String {
    if value.starts_with(HP_PREFIX) {
        return value.trim_start_matches(HP_PREFIX).to_string();
    }
    if let Some(stripped) = value.strip_prefix("metric.") {
        return stripped.to_string();
    }
    value.to_string()
}

fn enabled_axes(app: &AppState) -> Vec<AxisKey> {
    let mut axes = Vec::new();
    for param in app.params_list.items() {
        if param.checked {
            axes.push(AxisKey {
                name: param.id.clone(),
            });
        }
    }
    for metric in app.metrics_list.items() {
        if metric.checked {
            axes.push(AxisKey {
                name: metric.id.clone(),
            });
        }
    }
    axes
}

fn enabled_axis_count(app: &AppState) -> usize {
    app.params_list.items().iter().filter(|p| p.checked).count()
        + app.metrics_list.items().iter().filter(|m| m.checked).count()
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
                metric_fields
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
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
