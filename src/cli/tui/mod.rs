use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::constants::{FIELD_METRIC, FIELD_SCORE, HP_PREFIX};
use crate::project::Project;
use crate::trial::store::StepSubscriber;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Axis, Block, Borders, Chart, Dataset, GraphType, Paragraph, Widget,
    canvas::{Canvas, Line as CanvasLine},
};
use rusqlite::{Connection, OpenFlags};
use term_wm::component_context::ScrollHandle;
use term_wm::components::AppRootComponent;
use term_wm::events::{Event, KeyCode, KeyEvent, KeyKind, KeyModifiers, MouseButton};
use term_wm::helpers::{downcast_ratatui, layout_rect_to_clipped_rect};
use term_wm::io::RenderTarget;
use term_wm::keybindings::{KeyBindings, KeyCombo};
use term_wm::prelude::{Component, ComponentContext, EventResult, TermWmAction};
use term_wm::runner::{WindowManagerHost, run_with_defaults};
use term_wm::term_wm_app::TermWmApp;
use term_wm::window::{WindowKey, WindowManager, WindowState};
use term_wm::wm_config::WmConfig;
use term_wm::{
    AppContext, ListComponent, Rect as WmRect, ScrollKeyMode, ScrollViewComponent,
    TextRendererComponent, ToggleItem, ToggleListComponent,
};
use term_wm_console::RatatuiBackend;
use term_wm_console::RenderBackend;
use term_wm_console::console_event_source::ConsoleEventSource;
use term_wm_console::console_render_target::ConsoleRenderTarget;
use term_wm_core::hitbox_registry::HitboxRegistry;
use term_wm_core::impl_component_delegate;
use term_wm_core::task_scheduler::{AppTask, TaskHandle};
use term_wm_ui_facade::{LayerComponent, OverlayComponent};

pub fn run(project: Project, poll_ms: u64) -> io::Result<()> {
    let poll = Duration::from_millis(poll_ms.max(16));
    let db_path = project.trials_db_path();
    let config = WmConfig {
        keybindings: argtuner_keybindings(),
        ..Default::default()
    };
    let mut inner = TermWmApp::<AppComponent>::new_with_config(
        AppContext::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")).with_hostname(
            &hostname::get()
                .ok()
                .and_then(|s| s.into_string().ok())
                .unwrap_or_else(|| "unknown-host".to_string()),
        ),
        config,
    );

    let trials_key = inner.open_window(AppRootComponent::Custom(AppComponent::Trials(
        mk_trials_sv(),
    )));
    let charts_key = inner.open_window(AppRootComponent::Custom(AppComponent::Charts(
        mk_charts_sv(),
    )));
    let details_key = inner.open_window(AppRootComponent::Custom(AppComponent::Details(
        mk_details_sv(),
    )));
    let params_key = inner.open_window(AppRootComponent::Custom(AppComponent::Params(
        ToggleListComponent::new("Hyperparameters"),
    )));
    let metrics_key = inner.open_window(AppRootComponent::Custom(AppComponent::Metrics(
        ToggleListComponent::new("Metrics"),
    )));
    let project_info_key = inner.open_window(AppRootComponent::Custom(AppComponent::ProjectInfo(
        mk_project_info_sv(),
    )));
    let frontier_key = inner.open_window(AppRootComponent::Custom(AppComponent::Frontier(
        mk_project_info_sv(),
    )));

    let mut step_subscriber = StepSubscriber::new();
    let _ = step_subscriber.connect(argtuner_common::STEP_PUBLISHER_PORT);

    let project_info_static = project_info_lines(&project, poll.as_millis());

    let mut app = AppState {
        inner,
        db_path,
        poll,
        trials: Vec::new(),
        epoch_rows: BTreeMap::new(),
        step_rows: BTreeMap::new(),
        step_subscriber,
        last_error: None,
        chart_mode: ChartMode::Metrics,
        last_selected_trial: usize::MAX,
        trials_key,
        charts_key,
        details_key,
        params_key,
        metrics_key,
        project_info_key,
        frontier_key,
        last_activity: None,
        project_info_static,
        project_info_count: usize::MAX,
    };

    let wm = app.inner.wm();
    for (k, t) in [
        (app.trials_key, "Trials"),
        (app.charts_key, "Charts"),
        (app.details_key, "Trial Details"),
        (app.params_key, "Hyperparameters"),
        (app.metrics_key, "Metrics"),
        (app.project_info_key, "Project Info"),
        (app.frontier_key, "Pareto Frontier"),
    ] {
        wm.set_window_title(k, t);
        // The Watch TUI's panes are fixed views — never closable via the
        // chrome ✕, palette, or any close path.
        wm.set_closable(k, false);
    }
    app.apply_chart_mode();

    let mut output = ConsoleRenderTarget::new()?;
    let mut input = ConsoleEventSource::new();
    output.enter()?;
    let result = run_with_defaults(&mut output, &mut input, &mut app);
    output.exit()?;
    result
}

/// How long of silence (no step frames, no `running` rows) before the run is
/// declared complete in the frontier/best-trials header. Also the hysteresis
/// that bridges inter-trial scheduling gaps.
const IN_PROGRESS_HOLD: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
struct TrialRow {
    trial_id: i64,
    status: String,
    elapsed_ms: i64,
    error: Option<String>,
    fields: BTreeMap<String, String>,
}

type TrialEpochRows = BTreeMap<i64, Vec<TrialRow>>;
type TrialStepRows = BTreeMap<i64, Vec<TrialRow>>;
type TrialLoadResult = Result<(Vec<TrialRow>, TrialEpochRows, TrialStepRows), String>;

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

struct ChartsView {
    trials: Vec<TrialRow>,
    epoch_rows: BTreeMap<i64, Vec<TrialRow>>,
    chart_mode: ChartMode,
    chart_view: ChartView,
    chart_selected: usize,
    metrics_len: usize,
    chart_zoom: f64,
    last_error: Option<String>,
    selected_trial_idx: usize,
    params_x_offset: usize,
    enabled_axes: Vec<AxisKey>,
    scroll_handle: Option<ScrollHandle>,
}

impl Component<TermWmAction> for ChartsView {
    fn render(
        &mut self,
        backend: &mut dyn RenderBackend,
        area: WmRect,
        ctx: &ComponentContext,
        _registry: &mut HitboxRegistry,
    ) {
        self.scroll_handle = ctx.scroll_handle();
        let area = layout_rect_to_clipped_rect(area);
        let backend = downcast_ratatui(backend);
        render_charts_content(backend, self, area, ctx);
    }

    fn on_key(&mut self, event: &Event, ctx: &ComponentContext) -> EventResult<TermWmAction> {
        let Event::Key(key) = event else {
            return EventResult::Ignored;
        };
        if key.kind != KeyKind::Press {
            return EventResult::Ignored;
        }
        let kb = &ctx.config().keybindings;
        let chart_actions = [
            TermWmAction::ZoomIn,
            TermWmAction::ZoomOut,
            TermWmAction::ResetZoom,
            TermWmAction::CycleViewMode,
            TermWmAction::PanLeft,
            TermWmAction::PanRight,
            TermWmAction::MenuUp,
            TermWmAction::MenuDown,
            TermWmAction::ScrollPageUp,
            TermWmAction::ScrollPageDown,
            TermWmAction::ScrollHome,
            TermWmAction::ScrollEnd,
        ];
        for action in chart_actions {
            if kb.matches(action.clone(), key)
                && action_allowed_in(self.chart_mode, self.chart_view, &action)
            {
                return EventResult::Action(action);
            }
        }
        EventResult::Ignored
    }

    fn update(
        &mut self,
        action: TermWmAction,
        ctx: &ComponentContext,
        _actions: &mut VecDeque<(WindowKey, TermWmAction)>,
    ) {
        if !action_allowed_in(self.chart_mode, self.chart_view, &action) {
            return;
        }
        match action {
            TermWmAction::ZoomIn => self.chart_zoom = (self.chart_zoom * 0.8).clamp(0.1, 1.0),
            TermWmAction::ZoomOut => self.chart_zoom = (self.chart_zoom * 1.25).clamp(0.1, 1.0),
            TermWmAction::ResetZoom => self.chart_zoom = 1.0,
            TermWmAction::CycleViewMode => self.toggle_chart_view(ctx),
            TermWmAction::PanLeft => self.pan_params(-1),
            TermWmAction::PanRight => self.pan_params(1),
            TermWmAction::MenuUp => self.move_chart_selection(-1),
            TermWmAction::MenuDown => self.move_chart_selection(1),
            TermWmAction::ScrollPageUp => self.move_chart_selection(-5),
            TermWmAction::ScrollPageDown => self.move_chart_selection(5),
            TermWmAction::ScrollHome => self.chart_selected = 0,
            TermWmAction::ScrollEnd => {
                self.chart_selected = self.metrics_len.saturating_sub(1);
            }
            _ => {}
        }
    }

    fn on_mouse_press(
        &mut self,
        _local_x: u16,
        local_y: u16,
        _button: MouseButton,
        _modifiers: KeyModifiers,
        ctx: &ComponentContext,
    ) -> EventResult<TermWmAction> {
        if self.chart_view == ChartView::Summary && self.chart_mode == ChartMode::Metrics {
            let vp = ctx.viewport();
            let click_y = vp.offset_y + local_y as usize;
            let index = click_y / CHART_ITEM_HEIGHT as usize;
            if index < self.metrics_len {
                self.chart_selected = index;
                self.chart_view = ChartView::Focused;
                return EventResult::Consumed;
            }
        }
        EventResult::Ignored
    }
}

fn action_allowed_in(chart_mode: ChartMode, chart_view: ChartView, action: &TermWmAction) -> bool {
    match action {
        TermWmAction::ZoomIn
        | TermWmAction::ZoomOut
        | TermWmAction::ResetZoom
        | TermWmAction::CycleViewMode => chart_mode == ChartMode::Metrics,
        TermWmAction::PanLeft | TermWmAction::PanRight => chart_mode == ChartMode::HyperParams,
        TermWmAction::MenuUp
        | TermWmAction::MenuDown
        | TermWmAction::ScrollPageUp
        | TermWmAction::ScrollPageDown
        | TermWmAction::ScrollHome
        | TermWmAction::ScrollEnd => chart_view == ChartView::Focused,
        _ => false,
    }
}

impl ChartsView {
    fn toggle_chart_view(&mut self, _ctx: &ComponentContext) {
        self.chart_view = match self.chart_view {
            ChartView::Summary => {
                if self.metrics_len == 0 {
                    ChartView::Summary
                } else {
                    if let Some(h) = &self.scroll_handle {
                        let offset = h.scroll.borrow().offset_y;
                        self.chart_selected =
                            (offset / CHART_ITEM_HEIGHT as usize).min(self.metrics_len - 1);
                    }
                    ChartView::Focused
                }
            }
            ChartView::Focused => {
                if self.metrics_len > 0 {
                    let rows =
                        self.chart_selected.min(self.metrics_len - 1) * CHART_ITEM_HEIGHT as usize;
                    if let Some(h) = &self.scroll_handle {
                        h.scroll_vertical_to(rows);
                    }
                }
                ChartView::Summary
            }
        };
    }

    fn move_chart_selection(&mut self, delta: isize) {
        if self.metrics_len == 0 {
            return;
        }
        let max = (self.metrics_len - 1) as isize;
        let next = (self.chart_selected as isize + delta).clamp(0, max) as usize;
        self.chart_selected = next;
    }

    fn pan_params(&mut self, delta: isize) {
        let enabled_len = self.enabled_axes.len();
        if enabled_len == 0 {
            self.params_x_offset = 0;
            return;
        }
        let max = (enabled_len - 1) as isize;
        let next = (self.params_x_offset as isize + delta).clamp(0, max) as usize;
        self.params_x_offset = next;
    }
}

enum AppComponent {
    Trials(ScrollViewComponent<ListComponent>),
    Charts(ScrollViewComponent<ChartsView>),
    Details(ScrollViewComponent<TextRendererComponent>),
    Params(ToggleListComponent),
    Metrics(ToggleListComponent),
    ProjectInfo(ScrollViewComponent<TextRendererComponent>),
    Frontier(ScrollViewComponent<TextRendererComponent>),
}

impl_component_delegate!(AppComponent {
    Trials,
    Charts,
    Details,
    Params,
    Metrics,
    ProjectInfo,
    Frontier
});

struct AppState {
    inner: TermWmApp<AppComponent>,
    db_path: PathBuf,
    poll: Duration,
    trials: Vec<TrialRow>,
    epoch_rows: BTreeMap<i64, Vec<TrialRow>>,
    step_rows: BTreeMap<i64, Vec<TrialRow>>,
    step_subscriber: StepSubscriber,
    last_error: Option<String>,
    chart_mode: ChartMode,
    last_selected_trial: usize,
    trials_key: WindowKey,
    charts_key: WindowKey,
    details_key: WindowKey,
    params_key: WindowKey,
    metrics_key: WindowKey,
    project_info_key: WindowKey,
    frontier_key: WindowKey,
    /// Last time live activity (step frames or `running` rows) was observed;
    /// drives the run-in-progress header with [`IN_PROGRESS_HOLD`] hysteresis.
    last_activity: Option<std::time::Instant>,
    /// Static Project Info lines (paths + poll) built once at startup.
    project_info_static: Vec<Line<'static>>,
    /// Last trial count rendered into the Project Info window.
    project_info_count: usize,
}

impl AppState {
    fn trials_sv(&mut self) -> Option<&mut ScrollViewComponent<ListComponent>> {
        match self.inner.wm().component_for_key_mut(self.trials_key) {
            Some(AppRootComponent::Custom(AppComponent::Trials(sv))) => Some(sv),
            _ => None,
        }
    }

    fn charts_sv(&mut self) -> Option<&mut ScrollViewComponent<ChartsView>> {
        match self.inner.wm().component_for_key_mut(self.charts_key) {
            Some(AppRootComponent::Custom(AppComponent::Charts(sv))) => Some(sv),
            _ => None,
        }
    }

    fn details_sv(&mut self) -> Option<&mut ScrollViewComponent<TextRendererComponent>> {
        match self.inner.wm().component_for_key_mut(self.details_key) {
            Some(AppRootComponent::Custom(AppComponent::Details(sv))) => Some(sv),
            _ => None,
        }
    }

    fn params_list(&mut self) -> Option<&mut ToggleListComponent> {
        match self.inner.wm().component_for_key_mut(self.params_key) {
            Some(AppRootComponent::Custom(AppComponent::Params(l))) => Some(l),
            _ => None,
        }
    }

    fn metrics_list(&mut self) -> Option<&mut ToggleListComponent> {
        match self.inner.wm().component_for_key_mut(self.metrics_key) {
            Some(AppRootComponent::Custom(AppComponent::Metrics(l))) => Some(l),
            _ => None,
        }
    }

    fn project_info_sv(&mut self) -> Option<&mut ScrollViewComponent<TextRendererComponent>> {
        match self.inner.wm().component_for_key_mut(self.project_info_key) {
            Some(AppRootComponent::Custom(AppComponent::ProjectInfo(sv))) => Some(sv),
            _ => None,
        }
    }

    fn frontier_sv(&mut self) -> Option<&mut ScrollViewComponent<TextRendererComponent>> {
        match self.inner.wm().component_for_key_mut(self.frontier_key) {
            Some(AppRootComponent::Custom(AppComponent::Frontier(sv))) => Some(sv),
            _ => None,
        }
    }

    /// Whether a tuner run appears active: live activity was observed and the
    /// last observation is within [`IN_PROGRESS_HOLD`]. The hold doubles as
    /// hysteresis so inter-trial gaps don't flicker the header.
    fn run_in_progress(&self) -> bool {
        self.last_activity
            .is_some_and(|last| last.elapsed() < IN_PROGRESS_HOLD)
    }

    fn apply_chart_mode(&mut self) {
        let wm = self.inner.wm();
        match self.chart_mode {
            ChartMode::Metrics => {
                wm.transition_window(self.params_key, WindowState::Unmapped);
                wm.transition_window(self.metrics_key, WindowState::Unmapped);
                wm.transition_window(self.details_key, WindowState::Mapped);
            }
            ChartMode::HyperParams => {
                wm.transition_window(self.details_key, WindowState::Unmapped);
                wm.transition_window(self.params_key, WindowState::Mapped);
                wm.transition_window(self.metrics_key, WindowState::Mapped);
            }
        }
        wm.mark_layout_dirty();
    }

    fn selected_trial_idx(&mut self) -> usize {
        match self.trials_sv() {
            Some(sv) => sv.content.borrow().selected(),
            None => 0,
        }
    }

    fn enabled_axes(&mut self) -> Vec<AxisKey> {
        let mut axes = Vec::new();
        if let Some(list) = self.params_list() {
            for item in list.items() {
                if item.checked {
                    axes.push(AxisKey {
                        name: item.id.clone(),
                    });
                }
            }
        }
        if let Some(list) = self.metrics_list() {
            for item in list.items() {
                if item.checked {
                    axes.push(AxisKey {
                        name: item.id.clone(),
                    });
                }
            }
        }
        axes
    }

    fn push_data_to_components(&mut self) {
        let trials = self.trials.clone();
        let epochs = self.epoch_rows.clone();
        let last_error = self.last_error.clone();
        let selected = self.selected_trial_idx();
        let axes = self.enabled_axes();
        let mode = self.chart_mode;

        // Scroll charts/details back to top when the selected trial changes.
        if self.last_selected_trial != selected {
            self.last_selected_trial = selected;
            if let Some(sv) = self.charts_sv() {
                let h = sv.scroll_handle();
                h.scroll_vertical_to(0);
            }
            if let Some(sv) = self.details_sv() {
                let h = sv.scroll_handle();
                h.scroll_vertical_to(0);
            }
        }

        if let Some(sv) = self.charts_sv() {
            let mut c = sv.content.borrow_mut();
            c.trials = trials.clone();
            c.epoch_rows = epochs.clone();
            c.chart_mode = mode;
            c.last_error = last_error.clone();
            c.selected_trial_idx = selected;
            c.enabled_axes = axes.clone();
        }
        if let Some(sv) = self.details_sv() {
            let idx = selected.min(trials.len().saturating_sub(1));
            let text = match trials.get(idx) {
                Some(trial) => {
                    let epoch_rows = epochs.get(&trial.trial_id).cloned().unwrap_or_default();
                    Text::from(trial_detail_lines(trial, &epoch_rows))
                }
                None => Text::from(vec![Line::from("No trial selected.")]),
            };
            sv.content.borrow_mut().set_text(text);
        }
        let count = self.trials.len();
        if count != self.project_info_count {
            self.project_info_count = count;
            // Clone the cached static lines BEFORE borrowing the component.
            let mut lines = self.project_info_static.clone();
            lines.push(Line::from(format!("Trial Count: {count}")));
            if let Some(sv) = self.project_info_sv() {
                sv.content.borrow_mut().set_text(Text::from(lines));
            }
        }

        // Titles — standalone block AFTER all component borrows are dropped
        // (each set_window_title needs &mut WindowManager).
        let charts_focused = self.inner.wm().focused_window() == self.charts_key;
        let (cv, cs, ml) = match self.charts_sv() {
            Some(sv) => {
                let c = sv.content.borrow();
                (c.chart_view, c.chart_selected, c.metrics_len)
            }
            None => (ChartView::Summary, 0, 0),
        };
        let trial_id = trials.get(selected).map(|t| t.trial_id);
        let wm = self.inner.wm();
        wm.set_window_title(self.trials_key, "Trials");
        wm.set_window_title(
            self.details_key,
            trial_id.map_or_else(
                || "Trial Details".to_string(),
                |id| format!("Trial {id} Details"),
            ),
        );
        wm.set_window_title(
            self.charts_key,
            charts_window_title(mode, cv, cs, ml, charts_focused, trial_id),
        );
        wm.set_window_title(
            self.frontier_key,
            if is_multi_objective(&self.trials) {
                "Pareto Frontier"
            } else {
                "Best Trials"
            },
        );
    }

    fn refresh_trials(&mut self) {
        let mut saw_activity = false;
        while let Some(line) = self.step_subscriber.try_recv() {
            saw_activity = true;
            if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line)
                && let (Some(trial_id), Some(steps)) = (
                    msg.get("trial_id").and_then(|v| v.as_i64()),
                    msg.get("steps").and_then(|v| v.as_array()),
                )
            {
                let rows: Vec<TrialRow> = steps
                    .iter()
                    .filter_map(|s| s.as_object())
                    .map(|fields| {
                        let mut map = BTreeMap::new();
                        for (k, v) in fields {
                            map.insert(k.clone(), v.as_str().unwrap_or("").to_string());
                        }
                        TrialRow {
                            trial_id,
                            status: "running".to_string(),
                            elapsed_ms: 0,
                            error: None,
                            fields: map,
                        }
                    })
                    .collect();
                self.step_rows.entry(trial_id).or_default().extend(rows);
            }
        }
        match load_trials(&self.db_path) {
            Ok((trials, epoch_rows, step_rows)) => {
                let prev_sel = match self.trials_sv() {
                    Some(sv) => sv.content.borrow().selected(),
                    None => 0,
                };
                let prev_trial_id = self.trials.get(prev_sel).map(|t| t.trial_id);
                self.trials = trials;
                self.epoch_rows = epoch_rows;
                for (trial_id, rows) in step_rows {
                    self.step_rows.entry(trial_id).or_default().extend(rows);
                }
                if self.trials.iter().any(|t| t.status == "running")
                    || self
                        .epoch_rows
                        .values()
                        .flatten()
                        .any(|r| r.status == "running")
                    || self
                        .step_rows
                        .values()
                        .flatten()
                        .any(|r| r.status == "running")
                {
                    saw_activity = true;
                }
                if saw_activity {
                    self.last_activity = Some(std::time::Instant::now());
                }
                let in_progress = self.run_in_progress();
                let items = build_trial_items(&self.trials);
                if let Some(sv) = self.trials_sv() {
                    sv.content.borrow_mut().update_items(items);
                }
                let frontier = frontier_lines(&self.trials, in_progress);
                if let Some(sv) = self.frontier_sv() {
                    sv.content.borrow_mut().set_text(Text::from(frontier));
                }
                if let Some(tid) = prev_trial_id
                    && let Some(pos) = self.trials.iter().position(|t| t.trial_id == tid)
                    && let Some(sv) = self.trials_sv()
                {
                    let sel = sv.content.borrow().selected();
                    let delta = pos as isize - sel as isize;
                    sv.content.borrow_mut().move_selection(delta);
                }
                sync_param_toggles(self);
                self.last_error = None;
            }
            Err(err) => {
                self.last_error = Some(err);
            }
        }
    }
}

impl WindowManagerHost<AppRootComponent<AppComponent>, LayerComponent, OverlayComponent>
    for AppState
{
    fn wm(
        &mut self,
    ) -> &mut WindowManager<AppRootComponent<AppComponent>, LayerComponent, OverlayComponent> {
        self.inner.wm()
    }

    fn handle_app_event(&mut self, event: &Event) -> bool {
        // filter keeps this a single if-let: no let-chains (pre-1.88 stable)
        // and no `clippy::collapsible_if` from nesting an if inside an if-let.
        // App shortcuts apply only while an app-owned (Custom) window is
        // focused; core windows (Terminal, Debug Log, …) own their own input.
        if let Some(key) = pressed_key(event).filter(|_| self.inner.focused_is_custom()) {
            let kb = self.inner.wm().keybindings();
            if kb.matches(TermWmAction::Quit, key) {
                self.open_exit_confirm();
                return true;
            }
            if kb.matches(TermWmAction::Custom(1), key) {
                self.chart_mode = match self.chart_mode {
                    ChartMode::Metrics => ChartMode::HyperParams,
                    ChartMode::HyperParams => ChartMode::Metrics,
                };
                self.apply_chart_mode();
                return true;
            }
        }
        self.inner.handle_app_event(event)
    }

    fn render(&mut self, backend: &mut dyn RenderBackend) {
        self.push_data_to_components();
        self.inner.render_app(backend);
    }

    /// Schedule the recurring SQLite refresh on the app-task scheduler. Called
    /// by `run_with_defaults` before the event loop starts. `fire_immediate`
    /// runs the first poll right away so the initial load doesn't wait a full
    /// `--poll-ms` interval.
    fn on_app_scheduler_ready(&mut self, handle: TaskHandle<AppTask<Self>>) {
        let _ = handle.schedule_repeating(
            self.poll,
            true,
            AppTask::new(|app: &mut Self| {
                app.refresh_trials();
                tracing::info!(
                    "refresh trials poll tick: {} trials, {} epoch rows",
                    app.trials.len(),
                    app.epoch_rows.len()
                );
            }),
        );
    }

    fn toggle_debug_window(&mut self) {
        self.inner.toggle_debug_window();
    }

    fn wm_new_terminal(&mut self) -> std::io::Result<()> {
        self.inner.wm_new_terminal()
    }

    fn open_exit_confirm(&mut self) {
        self.inner.open_exit_confirm();
    }

    fn open_command_palette(&mut self) {
        self.inner.open_command_palette();
    }

    fn open_help_overlay(&mut self) {
        self.inner.open_help_overlay();
    }

    fn quit_requested(&self) -> bool {
        self.inner.quit_requested()
    }
}

fn charts_window_title(
    chart_mode: ChartMode,
    chart_view: ChartView,
    chart_selected: usize,
    metrics_len: usize,
    charts_focused: bool,
    trial_id: Option<i64>,
) -> String {
    match chart_mode {
        ChartMode::Metrics if charts_focused && chart_view == ChartView::Focused => {
            let total = metrics_len;
            let current = chart_selected.saturating_add(1);
            match trial_id {
                Some(id) => format!("Trial {id} - Metric Curve {current}/{total}"),
                None => format!("Metric Curve {current}/{total}"),
            }
        }
        ChartMode::Metrics => match trial_id {
            Some(id) => format!("Trial {id} - Metric Curves"),
            None => "Metric Curves".to_string(),
        },
        ChartMode::HyperParams if charts_focused => match trial_id {
            Some(id) => format!("Trial {id} - Hyperparameter Space"),
            None => "Hyperparameter Space".to_string(),
        },
        ChartMode::HyperParams => match trial_id {
            Some(id) => format!("Trial {id} - Hyperparameter Space"),
            None => "Hyperparameter Space".to_string(),
        },
    }
}

const CHART_ITEM_HEIGHT: u16 = 7;
const PARAM_AXIS_WIDTH: u16 = 6;

/// Inline zoom-out footer for the Charts window, derived from the live
/// keybindings so the shown keys always match the user's config.
fn chart_keybindings_hint(kb: &KeyBindings) -> String {
    let zoom_in = kb
        .combos_for(TermWmAction::ZoomIn)
        .first()
        .cloned()
        .unwrap_or_default();
    let zoom_out = kb
        .combos_for(TermWmAction::ZoomOut)
        .first()
        .cloned()
        .unwrap_or_default();
    let reset = kb
        .combos_for(TermWmAction::ResetZoom)
        .first()
        .cloned()
        .unwrap_or_default();
    let list = kb
        .combos_for(TermWmAction::CycleViewMode)
        .first()
        .cloned()
        .unwrap_or_default();
    format!("[{zoom_in}] zoom in    [{zoom_out}] zoom out    [{reset}] reset    [{list}] list view")
}

/// Returns the pressed key for key events; None for repeat/release or
/// non-key events. Single match-guard form keeps this stable on pre-1.88
/// toolchains and avoids `clippy::collapsible_if`.
fn pressed_key(event: &Event) -> Option<&KeyEvent> {
    match event {
        Event::Key(key) if key.kind == KeyKind::Press => Some(key),
        _ => None,
    }
}

fn argtuner_keybindings() -> KeyBindings {
    let mut kb = KeyBindings::default();
    // App-level: quit + mode toggle.
    kb.add(
        TermWmAction::Quit,
        KeyCombo::new(KeyCode::Char('q'), KeyModifiers::NONE),
    );
    kb.add(
        TermWmAction::Custom(1),
        KeyCombo::new(KeyCode::Char('h'), KeyModifiers::NONE),
    );
    // Chart view toggle (Metrics mode).
    kb.add(
        TermWmAction::CycleViewMode,
        KeyCombo::new(KeyCode::Char('f'), KeyModifiers::NONE),
    );
    kb.add(
        TermWmAction::CycleViewMode,
        KeyCombo::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    kb.add(
        TermWmAction::CycleViewMode,
        KeyCombo::new(KeyCode::Char(' '), KeyModifiers::NONE),
    );
    // Zoom.
    kb.add(
        TermWmAction::ZoomIn,
        KeyCombo::new(KeyCode::Char('+'), KeyModifiers::NONE),
    );
    kb.add(
        TermWmAction::ZoomIn,
        KeyCombo::new(KeyCode::Char('='), KeyModifiers::NONE),
    );
    kb.add(
        TermWmAction::ZoomOut,
        KeyCombo::new(KeyCode::Char('-'), KeyModifiers::NONE),
    );
    kb.add(
        TermWmAction::ResetZoom,
        KeyCombo::new(KeyCode::Char('0'), KeyModifiers::NONE),
    );
    // Pan params (HyperParams mode).
    kb.add(
        TermWmAction::PanLeft,
        KeyCombo::new(KeyCode::Left, KeyModifiers::NONE),
    );
    kb.add(
        TermWmAction::PanRight,
        KeyCombo::new(KeyCode::Right, KeyModifiers::NONE),
    );
    // Chart selection movement (Focused view).
    kb.add(
        TermWmAction::MenuUp,
        KeyCombo::new(KeyCode::Up, KeyModifiers::NONE),
    );
    kb.add(
        TermWmAction::MenuUp,
        KeyCombo::new(KeyCode::Char('k'), KeyModifiers::NONE),
    );
    kb.add(
        TermWmAction::MenuDown,
        KeyCombo::new(KeyCode::Down, KeyModifiers::NONE),
    );
    kb.add(
        TermWmAction::MenuDown,
        KeyCombo::new(KeyCode::Char('j'), KeyModifiers::NONE),
    );
    kb.add(
        TermWmAction::ScrollPageUp,
        KeyCombo::new(KeyCode::PageUp, KeyModifiers::NONE),
    );
    kb.add(
        TermWmAction::ScrollPageDown,
        KeyCombo::new(KeyCode::PageDown, KeyModifiers::NONE),
    );
    kb.add(
        TermWmAction::ScrollHome,
        KeyCombo::new(KeyCode::Home, KeyModifiers::NONE),
    );
    kb.add(
        TermWmAction::ScrollEnd,
        KeyCombo::new(KeyCode::End, KeyModifiers::NONE),
    );
    kb
}

fn mk_trials_sv() -> ScrollViewComponent<ListComponent> {
    let mut sv = ScrollViewComponent::new(ListComponent::new("Trials"));
    sv.set_keyboard_mode(ScrollKeyMode::None);
    sv
}

fn mk_charts_sv() -> ScrollViewComponent<ChartsView> {
    let mut sv = ScrollViewComponent::new(ChartsView {
        trials: Vec::new(),
        epoch_rows: BTreeMap::new(),
        chart_mode: ChartMode::Metrics,
        chart_view: ChartView::Summary,
        chart_selected: 0,
        metrics_len: 0,
        chart_zoom: 1.0,
        last_error: None,
        selected_trial_idx: 0,
        params_x_offset: 0,
        enabled_axes: Vec::new(),
        scroll_handle: None,
    });
    // PageUp/PageDown/Home/End scroll the chart list (the wrapper owns the
    // viewport). Up/Down fall through to ChartsView for selection moves.
    sv.set_keyboard_mode(ScrollKeyMode::PaginationOnly);
    sv
}

fn mk_details_sv() -> ScrollViewComponent<TextRendererComponent> {
    let mut sv = ScrollViewComponent::new(TextRendererComponent::new());
    // Keep one field per line (no reflow) so the key = value layout is preserved;
    // long values scroll horizontally if wider than the window.
    sv.content.borrow_mut().set_wrap(false);
    // Drag-to-select + copy-on-release via term-wm's selection/clipboard pipeline.
    sv.content.borrow_mut().set_selection_enabled(true);
    // Full keyboard scroll: Up/Down/PageUp/PageDown/Home/End all scroll the
    // details viewport. TextRendererComponent has no key handling of its own.
    sv.set_keyboard_mode(ScrollKeyMode::Full);
    sv
}

fn mk_project_info_sv() -> ScrollViewComponent<TextRendererComponent> {
    let mut sv = ScrollViewComponent::new(TextRendererComponent::new());
    // Same presentation as Trial Details: one `key = value` line each, no
    // reflow, full keyboard scroll.
    sv.content.borrow_mut().set_wrap(false);
    sv.content.borrow_mut().set_selection_enabled(true);
    sv.set_keyboard_mode(ScrollKeyMode::Full);
    sv
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
    if let Some(list) = app.params_list() {
        for param in list.items() {
            existing.insert(param.id.clone(), param.checked);
        }
    }
    let mut existing_metrics = BTreeMap::new();
    if let Some(list) = app.metrics_list() {
        for metric in list.items() {
            existing_metrics.insert(metric.id.clone(), metric.checked);
        }
    }
    let selected_trial = app.selected_trial_idx();
    let main_metric = app
        .trials
        .get(selected_trial)
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
    if let Some(list) = app.params_list() {
        list.set_items(params);
    }
    if let Some(list) = app.metrics_list() {
        list.set_items(metrics);
    }
}

fn load_trials(path: &Path) -> TrialLoadResult {
    let conn = open_connection(path)?;
    let trials = load_trial_rows(&conn, "trial_records")?;
    let epoch_rows = load_epoch_rows(&conn)?;
    let step_rows = load_step_rows(&conn)?;
    Ok((trials, epoch_rows, step_rows))
}

// TODO: SQL queries should not be here
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

// TODO: SQL queries should not be here
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

// TODO: SQL queries should not be here
fn load_step_rows(conn: &Connection) -> Result<TrialStepRows, String> {
    let mut stmt = match conn.prepare(
        r#"
        SELECT trial_id, status, elapsed_ms, error, fields_json
        FROM trial_step_records
        ORDER BY trial_id ASC, row_id ASC
        "#,
    ) {
        Ok(stmt) => stmt,
        Err(err) => {
            if err
                .to_string()
                .contains("no such table: trial_step_records")
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

// TODO: SQL queries should not be here
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
    // Multi-objective: show every numeric metric.* value; otherwise the
    // resolved primary metric.
    let mut metrics: Vec<(String, f64)> = trial
        .fields
        .iter()
        .filter(|(k, _)| k.starts_with("metric.") && k.as_str() != "metric")
        .filter_map(|(k, v)| v.parse::<f64>().ok().map(|value| (k.clone(), value)))
        .collect();
    metrics.sort_by(|a, b| a.0.cmp(&b.0));
    if !metrics.is_empty() {
        return Some(
            metrics
                .iter()
                .map(|(label, value)| format!("{label}={value:.4}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    metric_for_trial(trial).map(|(label, value)| format!("{label}={value:.4}"))
}

/// Whether any trial carries per-objective `score.<name>` fields, i.e. this is
/// a multi-objective run (single-objective runs persist only `score`).
fn is_multi_objective(trials: &[TrialRow]) -> bool {
    trials
        .iter()
        .any(|t| t.fields.keys().any(|k| k.starts_with("score.")))
}

fn build_trial_items(trials: &[TrialRow]) -> Vec<String> {
    let multi = is_multi_objective(trials);
    let nd: std::collections::HashSet<i64> = if multi {
        nondominated_trial_ids(trials)
    } else {
        std::collections::HashSet::new()
    };
    trials
        .iter()
        .map(|trial| {
            let metric_text = metric_value_text(trial).unwrap_or_else(|| "-".to_string());
            let tag = if nd.contains(&trial.trial_id) {
                "[nd]"
            } else {
                ""
            };
            format!(
                "trial {:>3}  {:<7}  {:<5}  {}",
                trial.trial_id, trial.status, tag, metric_text
            )
        })
        .collect()
}

/// Trial ids on the non-dominated front, computed from the signed `score.<name>`
/// fields. Only completed `ok` trials are eligible. Meaningful for
/// multi-objective runs only.
fn nondominated_trial_ids(trials: &[TrialRow]) -> std::collections::HashSet<i64> {
    let mut vectors: Vec<(i64, Vec<f64>)> = Vec::new();
    for trial in trials {
        if trial.status != "ok" {
            continue;
        }
        if let Some(scores) = trial_signed_scores(trial) {
            vectors.push((trial.trial_id, scores));
        }
    }
    if vectors.is_empty() {
        return std::collections::HashSet::new();
    }
    let normalized: Vec<Vec<f64>> = vectors.iter().map(|(_, s)| s.clone()).collect();
    let fronts = crate::sampler::pareto::fast_nondominated_sort(&normalized);
    fronts
        .first()
        .map(|front| front.iter().map(|&i| vectors[i].0).collect())
        .unwrap_or_default()
}

/// Signed score vector for a trial, in a deterministic (sorted) objective
/// order derived from `score.<name>` fields; falls back to the single `score`
/// column.
fn trial_signed_scores(trial: &TrialRow) -> Option<Vec<f64>> {
    let mut keys: Vec<String> = trial
        .fields
        .keys()
        .filter(|k| k.starts_with("score."))
        .cloned()
        .collect();
    keys.sort();
    if keys.is_empty() {
        return trial
            .fields
            .get("score")
            .and_then(|v| v.parse::<f64>().ok())
            .map(|score| vec![score]);
    }
    Some(
        keys.iter()
            .map(|key| {
                trial
                    .fields
                    .get(key)
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(f64::INFINITY)
            })
            .collect(),
    )
}

fn hparam_lines(trial: &TrialRow) -> Vec<String> {
    let mut hparams: Vec<String> = trial
        .fields
        .iter()
        .filter(|(k, _)| k.starts_with("hp."))
        .map(|(k, v)| format!("    {:<20} {}", k.trim_start_matches("hp."), v))
        .collect();
    if hparams.is_empty() {
        return hparams;
    }
    let mut out = vec!["  Hyperparameters:".to_string()];
    out.append(&mut hparams);
    out
}

/// Lines for the adaptive frontier/best-trials panel: a run-state header, then
/// the non-dominated frontier (multi-objective) or the top trials by score
/// (single-objective), mirroring the end-of-run summary live.
fn frontier_lines(trials: &[TrialRow], in_progress: bool) -> Vec<Line<'static>> {
    let status = if in_progress {
        "Run in progress — results are live"
    } else {
        "Run complete — final results"
    };
    let mut lines = vec![Line::from(status)];
    let ok: Vec<&TrialRow> = trials.iter().filter(|t| t.status == "ok").collect();
    if ok.is_empty() {
        lines.push(Line::from("(no completed trials yet)"));
        return lines;
    }
    if !is_multi_objective(trials) {
        // Single-objective: top trials by the stored score (lower is better),
        // matching the CLI's end-of-run `print_top_trials` table.
        let mut ranked: Vec<(&TrialRow, f64)> = ok
            .iter()
            .filter_map(|t| {
                t.fields
                    .get("score")
                    .and_then(|v| v.parse::<f64>().ok())
                    .map(|score| (*t, score))
            })
            .collect();
        ranked.sort_by(|a, b| a.1.total_cmp(&b.1));
        let top = ranked.len().min(5);
        lines.push(Line::from(format!(
            "Best trials (top {top} of {}):",
            ok.len()
        )));
        for (trial, score) in ranked.into_iter().take(top) {
            let metric = trial
                .fields
                .get("metric")
                .and_then(|name| trial.fields.get(&format!("metric.{name}")))
                .and_then(|v| v.parse::<f64>().ok());
            let metric_part = metric
                .map(|m| format!("  metric={m:.4}"))
                .unwrap_or_default();
            lines.push(Line::from(format!(
                "Trial {}  score={score:.4}{metric_part}",
                trial.trial_id
            )));
            for line in hparam_lines(trial) {
                lines.push(Line::from(line));
            }
            lines.push(Line::from(""));
        }
        return lines;
    }
    let vectors: Vec<(&TrialRow, Vec<f64>)> = ok
        .iter()
        .filter_map(|t| trial_signed_scores(t).map(|s| (*t, s)))
        .collect();
    if vectors.is_empty() {
        lines.push(Line::from("Pareto frontier: none"));
        return lines;
    }
    let normalized: Vec<Vec<f64>> = vectors.iter().map(|(_, s)| s.clone()).collect();
    let fronts = crate::sampler::pareto::fast_nondominated_sort(&normalized);
    let front = fronts.first().cloned().unwrap_or_default();
    // Objective labels from the first trial's sorted score.* keys.
    let labels: Vec<String> = {
        let mut keys: Vec<String> = vectors[0]
            .0
            .fields
            .keys()
            .filter(|k| k.starts_with("score."))
            .map(|k| k.trim_start_matches("score.").to_string())
            .collect();
        keys.sort();
        keys
    };
    lines.push(Line::from(format!(
        "Pareto frontier ({} of {} trials):",
        front.len(),
        vectors.len()
    )));
    for idx in front {
        let (trial, signed) = &vectors[idx];
        let parts: Vec<String> = labels
            .iter()
            .zip(signed.iter())
            .map(|(name, value)| {
                let raw = trial
                    .fields
                    .get(&format!("metric.{name}"))
                    .and_then(|v| v.parse::<f64>().ok());
                format!("{name}={:.4}", raw.unwrap_or(*value))
            })
            .collect();
        lines.push(Line::from(format!(
            "Trial {}  {}",
            trial.trial_id,
            parts.join(" ")
        )));
        for line in hparam_lines(trial) {
            lines.push(Line::from(line));
        }
        lines.push(Line::from(""));
    }
    lines
}

fn render_charts_content(
    backend: &mut RatatuiBackend,
    charts: &mut ChartsView,
    area: Rect,
    ctx: &ComponentContext,
) {
    if charts.chart_mode == ChartMode::HyperParams {
        render_hyperparam_space(backend, charts, area);
        return;
    }
    if let Some(err) = charts.last_error.as_ref() {
        Paragraph::new(err.clone())
            .style(Style::default().fg(Color::Red))
            .render(area, &mut backend.buffer);
        return;
    }
    let Some(trial) = charts.trials.get(charts.selected_trial_idx) else {
        Paragraph::new("No trials loaded.").render(area, &mut backend.buffer);
        return;
    };
    let epochs = charts
        .epoch_rows
        .get(&trial.trial_id)
        .cloned()
        .unwrap_or_default();
    if epochs.is_empty() {
        Paragraph::new("No epoch metrics for selected trial.").render(area, &mut backend.buffer);
        return;
    }
    render_metric_charts(backend, charts, &epochs, area, ctx);
}

fn render_metric_charts(
    backend: &mut RatatuiBackend,
    charts: &mut ChartsView,
    epochs: &[TrialRow],
    area: Rect,
    ctx: &ComponentContext,
) {
    let metric_keys = collect_metric_keys_for_epochs(epochs);
    charts.metrics_len = metric_keys.len();
    if metric_keys.is_empty() {
        Paragraph::new("No numeric metric curves.").render(area, &mut backend.buffer);
        return;
    }
    if charts.chart_selected >= metric_keys.len() {
        charts.chart_selected = metric_keys.len().saturating_sub(1);
    }
    let x_axis = select_x_axis_spec(epochs);

    // Always reserve the bottom row for the config-derived keybinding hint so
    // the zoom/view keys are discoverable. The keys only act while the Charts
    // window is focused (the WM enforces that on ChartsView::on_key).
    let hint = chart_keybindings_hint(&ctx.config().keybindings);
    let chart_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height.saturating_sub(1),
    };

    match charts.chart_view {
        ChartView::Summary => {
            let total_items = metric_keys.len();
            let content_h = total_items * CHART_ITEM_HEIGHT as usize;
            if let Some(handle) = ctx.scroll_handle() {
                handle.set_content_size(chart_area.width as usize, content_h);
            }
            let offset_y = ctx.viewport().offset_y;
            let vp_h = chart_area.height as usize;
            let chart_item_h = CHART_ITEM_HEIGHT as usize;
            let area_y = chart_area.y as i32;
            let area_bottom = (chart_area.y + chart_area.height) as i32;

            let first_chart = (offset_y / chart_item_h).saturating_sub(1);
            let max_visible = vp_h.div_ceil(CHART_ITEM_HEIGHT as usize);
            let last_chart = (first_chart + max_visible + 2).min(metric_keys.len());

            #[allow(clippy::needless_range_loop)]
            for abs_idx in first_chart..last_chart {
                let chart_top = area_y + (abs_idx * chart_item_h) as i32 - offset_y as i32;
                let chart_bot = chart_top + CHART_ITEM_HEIGHT as i32;
                if chart_bot <= area_y || chart_top >= area_bottom {
                    continue;
                }
                let y = chart_top.max(area_y) as u16;
                let h = (chart_bot.min(area_bottom) - y as i32) as u16;
                let rect = Rect {
                    x: chart_area.x,
                    y,
                    width: chart_area.width,
                    height: h,
                };
                render_metric_chart(
                    backend,
                    epochs,
                    &metric_keys[abs_idx],
                    rect,
                    &x_axis,
                    charts.chart_zoom,
                );
            }
        }
        ChartView::Focused => {
            if let Some(handle) = ctx.scroll_handle() {
                handle.set_content_size(chart_area.width as usize, chart_area.height as usize);
            }
            let key = &metric_keys[charts.chart_selected];
            render_metric_chart(backend, epochs, key, chart_area, &x_axis, charts.chart_zoom);
        }
    }

    Paragraph::new(hint)
        .style(Style::default().fg(Color::DarkGray))
        .render(
            Rect {
                x: area.x,
                y: area.y.saturating_add(chart_area.height),
                width: area.width,
                height: 1,
            },
            &mut backend.buffer,
        );
}

fn render_hyperparam_space(backend: &mut RatatuiBackend, charts: &mut ChartsView, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let trials = &charts.trials;
    let selected = charts.selected_trial_idx;
    let enabled_axes: Vec<&AxisKey> = charts.enabled_axes.iter().collect();
    if enabled_axes.is_empty() {
        Paragraph::new("No axes enabled.").render(area, &mut backend.buffer);
        return;
    }
    let max_visible = (area.width / PARAM_AXIS_WIDTH).max(1) as usize;
    let enabled_len = enabled_axes.len();
    let start = charts.params_x_offset.min(enabled_len.saturating_sub(1));
    let end = (start + max_visible).min(enabled_len);
    let visible = &enabled_axes[start..end];
    let visible_len = visible.len();
    if visible_len == 0 {
        Paragraph::new("No parameters in view.").render(area, &mut backend.buffer);
        return;
    }
    let domains = visible
        .iter()
        .map(|axis| param_domain(trials, &axis.name))
        .collect::<Vec<_>>();
    let objective_range = objective_range(trials);
    let x_max = if visible_len == 1 {
        1.0
    } else {
        (visible_len - 1) as f64
    };
    let max_labels = (area.width / (PARAM_AXIS_WIDTH * 2)).max(1) as usize;
    let label_stride = visible_len.div_ceil(max_labels);
    let canvas = Canvas::default()
        .x_bounds([0.0, x_max])
        .y_bounds([0.0, 1.0])
        .paint(|ctx| {
            for (idx, axis) in visible.iter().enumerate() {
                let x = idx as f64;
                ctx.draw(&CanvasLine::new(x, 0.0, x, 1.0, Color::DarkGray));
                if idx % label_stride == 0 {
                    ctx.print(
                        x,
                        1.0,
                        Line::from(Span::styled(
                            short_axis_label(&axis.name, 10),
                            Style::default().fg(Color::White),
                        )),
                    );
                }
            }
            for (trial_idx, trial) in trials.iter().enumerate() {
                let color = if trial_idx == selected {
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
    canvas.render(area, &mut backend.buffer);
}

fn render_metric_chart(
    backend: &mut RatatuiBackend,
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
    chart.render(area, &mut backend.buffer);
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

fn project_info_lines(project: &Project, poll_ms: u128) -> Vec<Line<'static>> {
    vec![
        Line::from(format!("Project Root: {}", project.root().display())),
        Line::from(format!(
            "Config File: {}",
            project.unified_config_path().display()
        )),
        Line::from(format!("Trials CSV: {}", project.trials_path().display())),
        Line::from(format!(
            "Trials SQLite: {}",
            project.trials_db_path().display()
        )),
        Line::from(format!("Artifacts: {}", project.artifacts_dir().display())),
        Line::from(format!("Poll Interval: {poll_ms}ms")),
    ]
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

#[cfg(test)]
mod key_tests {
    use super::*;

    fn key(code: KeyCode, kind: KeyKind) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE, kind))
    }

    #[test]
    fn pressed_key_returns_only_press_events() {
        let press = key(KeyCode::Char('q'), KeyKind::Press);
        assert_eq!(
            pressed_key(&press).map(|k| k.code),
            Some(KeyCode::Char('q'))
        );
        assert_eq!(
            pressed_key(&press).map(|k| k.modifiers),
            Some(KeyModifiers::NONE)
        );
    }

    #[test]
    fn pressed_key_ignores_repeat_and_release() {
        assert!(pressed_key(&key(KeyCode::Char('q'), KeyKind::Repeat)).is_none());
        assert!(pressed_key(&key(KeyCode::Char('q'), KeyKind::Release)).is_none());
    }

    #[test]
    fn pressed_key_ignores_non_key_events() {
        let mouse = Event::Mouse(term_wm::events::MouseEvent {
            kind: term_wm::events::MouseEventKind::Press(term_wm::events::MouseButton::Left),
            row: 0,
            column: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert!(pressed_key(&mouse).is_none());
    }
}

#[cfg(test)]
mod frontier_tests {
    use super::*;

    fn trial(id: i64, loss: &str, latency: &str) -> TrialRow {
        TrialRow {
            trial_id: id,
            status: "ok".to_string(),
            elapsed_ms: 0,
            error: None,
            fields: BTreeMap::from([
                ("hp.dummy".to_string(), id.to_string()),
                ("metric.loss".to_string(), loss.to_string()),
                ("metric.latency_ms".to_string(), latency.to_string()),
                ("score.loss".to_string(), loss.to_string()),
                ("score.latency_ms".to_string(), latency.to_string()),
            ]),
        }
    }

    #[test]
    fn nondominated_ids_exclude_dominated_trials() {
        // trial 2 (loss=3, latency=4) is dominated by trial 0 (1, 3).
        let trials = vec![trial(0, "1", "3"), trial(1, "2", "1"), trial(2, "3", "4")];
        let nd = nondominated_trial_ids(&trials);
        assert!(nd.contains(&0));
        assert!(nd.contains(&1));
        assert!(!nd.contains(&2));
    }

    #[test]
    fn frontier_lines_list_only_non_dominated() {
        let trials = vec![trial(0, "1", "3"), trial(1, "2", "1"), trial(2, "3", "4")];
        let lines = frontier_lines(&trials, false);
        let text = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Run complete — final results"), "{text}");
        assert!(text.contains("Pareto frontier (2 of 3 trials)"), "{text}");
        assert!(text.contains("Trial 0"), "{text}");
        assert!(text.contains("Trial 1"), "{text}");
        assert!(!text.contains("Trial 2"), "{text}");
        assert!(text.contains("loss="), "{text}");
    }

    #[test]
    fn frontier_lines_marks_run_in_progress() {
        let mut trials = vec![trial(0, "1", "3"), trial(1, "2", "1")];
        trials[1].status = "running".to_string();
        let lines = frontier_lines(&trials, true);
        let text = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Run in progress"), "{text}");
        assert!(text.contains("Pareto frontier (1 of 1 trials)"), "{text}");
    }

    #[test]
    fn frontier_lines_single_objective_lists_best_trials() {
        let mk = |id: i64, score: &str, metric: &str| TrialRow {
            trial_id: id,
            status: "ok".to_string(),
            elapsed_ms: 0,
            error: None,
            fields: BTreeMap::from([
                ("hp.x".to_string(), id.to_string()),
                ("metric".to_string(), "loss".to_string()),
                ("metric.loss".to_string(), metric.to_string()),
                ("score".to_string(), score.to_string()),
            ]),
        };
        let trials = vec![
            mk(0, "0.5", "0.5"),
            mk(1, "0.9", "0.9"),
            mk(2, "0.2", "0.2"),
        ];
        let lines = frontier_lines(&trials, false);
        let text = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Best trials (top 3 of 3)"), "{text}");
        // Trial 2 has the best (lowest) score -> listed first.
        assert!(text.starts_with("Run complete"), "{text}");
        let first_trial_idx = text.find("Trial").expect("has a trial line");
        assert!(
            text[first_trial_idx..].starts_with("Trial 2"),
            "best trial first: {text}"
        );
        assert!(text.contains("score=0.2000"), "{text}");
        assert!(text.contains("Hyperparameters"), "{text}");
    }

    #[test]
    fn build_trial_items_tags_non_dominated_only_in_multi_objective() {
        let multi = vec![trial(0, "1", "3"), trial(1, "2", "1"), trial(2, "3", "4")];
        let items = build_trial_items(&multi);
        assert!(items[0].contains("[nd]"), "{}", items[0]);
        assert!(items[1].contains("[nd]"), "{}", items[1]);
        assert!(!items[2].contains("[nd]"), "{}", items[2]);

        // Single-objective rows (only `score`) must not be tagged `[nd]`.
        let single = vec![TrialRow {
            trial_id: 0,
            status: "ok".to_string(),
            elapsed_ms: 0,
            error: None,
            fields: BTreeMap::from([("score".to_string(), "0.5".to_string())]),
        }];
        let items = build_trial_items(&single);
        assert!(!items[0].contains("[nd]"), "{}", items[0]);
    }
}
