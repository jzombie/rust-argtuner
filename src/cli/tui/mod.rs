use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::constants::{
    FIELD_METRIC, FIELD_SCORE, FIELD_TRIAL_BUDGET_EPOCHS, FIELD_TRIAL_CONFIG_ID, FIELD_TRIAL_RUNG,
    FIELD_TRIAL_TIME, HP_PREFIX,
};
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
use term_wm_core::impl_view_component;
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
        mk_trials_window(),
    )));
    let charts_key = inner.open_window(AppRootComponent::Custom(AppComponent::Charts(
        mk_charts_window(),
    )));
    let details_key = inner.open_window(AppRootComponent::Custom(AppComponent::Details(
        mk_text_window(),
    )));
    let params_key = inner.open_window(AppRootComponent::Custom(AppComponent::Params(
        mk_toggle_window("Hyperparameters"),
    )));
    let metrics_key = inner.open_window(AppRootComponent::Custom(AppComponent::Metrics(
        mk_toggle_window("Metrics"),
    )));
    let project_info_key = inner.open_window(AppRootComponent::Custom(AppComponent::ProjectInfo(
        mk_text_window(),
    )));
    let frontier_key = inner.open_window(AppRootComponent::Custom(AppComponent::Frontier(
        mk_text_window(),
    )));

    let mut step_subscriber = StepSubscriber::new();
    let _ = step_subscriber.connect(argtuner_common::STEP_PUBLISHER_PORT);

    let project_info_static = project_info_lines(&project, poll.as_millis());

    let mut app = AppState {
        inner,
        db_path,
        poll,
        trials: Arc::new(Vec::new()),
        epoch_rows: Arc::new(BTreeMap::new()),
        step_rows: Arc::new(BTreeMap::new()),
        pending_live_steps: Arc::new(BTreeMap::new()),
        pending_version: 0,
        persisted_step_hashes: BTreeMap::new(),
        step_subscriber,
        last_error: None,
        chart_mode: ChartMode::Metrics,
        last_selected_trial: usize::MAX,
        next_poll_at: std::time::Instant::now(),
        data_version: 0,
        max_epoch_row_id: 0,
        max_step_row_id: 0,
        last_pushed_version: u64::MAX,
        last_pushed_pending: u64::MAX,
        last_pushed_selection: usize::MAX,
        last_pushed_mode: ChartMode::Metrics,
        last_pushed_source: ChartSource::Steps,
        last_pushed_axes: Vec::new(),
        last_pushed_error: None,
        last_pushed_count: usize::MAX,
        last_frontier_in_progress: false,
        cached_multi_objective: false,
        last_title_trial: None,
        last_title_mode: ChartMode::Metrics,
        last_title_source: ChartSource::Steps,
        last_title_view: ChartView::Summary,
        last_title_selected: usize::MAX,
        last_title_metrics_len: usize::MAX,
        last_title_focused: false,
        last_title_multi: false,
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

    // Initial synchronous load so the first paint already shows data.
    // Without this the event loop's pre-loop render paints empty panes and
    // the UI sits on "No trials loaded" until the first poll's full
    // history load (seconds on a large DB) completes.
    app.refresh_trials();
    app.next_poll_at = std::time::Instant::now() + app.poll;

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrialRow {
    trial_id: i64,
    status: String,
    elapsed_ms: i64,
    error: Option<String>,
    fields: BTreeMap<String, String>,
}

type TrialStepRows = BTreeMap<i64, Vec<TrialRow>>;
/// One parsed chart point: arrival-position `x`, metric value `y`.
/// Same layout as the `(f64, f64)` tuple it replaces — `Copy`, no
/// indirection, no perf difference. Named fields so call sites read as
/// `point.x` / `point.y` instead of `.0` / `.1`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ChartPoint {
    x: f64,
    y: f64,
}
/// One parsed metric curve: shared points so every frame reads them
/// without cloning.
type SharedPoints = Arc<Vec<ChartPoint>>;
/// All parsed curves for the selected chain, sorted by metric key.
type SharedSeriesList = Arc<Vec<(String, SharedPoints)>>;
/// One delta poll: reloaded trial rows, new epoch/step rows, the new
/// `row_id` high-water marks, and whether the DB file was replaced or
/// truncated under us (file max below the caller's cursor, or table gone
/// while the cursor is nonzero) — in which case the caller must drop its
/// maps and treat the deltas as a fresh full load.
struct TrialsPoll {
    trials: Vec<TrialRow>,
    epoch_delta: BTreeMap<i64, Vec<TrialRow>>,
    step_delta: TrialStepRows,
    max_epoch: i64,
    max_step: i64,
    epoch_reset: bool,
    step_reset: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChartMode {
    Metrics,
    HyperParams,
}

/// Which record stream the metric charts sweep. Step rows carry per-step
/// fields (`model.step_end.*`); epoch rows carry per-epoch fields
/// (`model.epoch_end.*`). Shared `metric.*` keys may exist on either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChartSource {
    Steps,
    Epochs,
}

impl ChartSource {
    fn label(self) -> &'static str {
        match self {
            ChartSource::Steps => "steps",
            ChartSource::Epochs => "epochs",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChartView {
    Summary,
    Focused,
}

struct ChartsView {
    trials: Arc<Vec<TrialRow>>,
    epoch_rows: Arc<BTreeMap<i64, Vec<TrialRow>>>,
    step_rows: Arc<BTreeMap<i64, Vec<TrialRow>>>,
    /// Unreconciled live TCP rows (no DB twin visible yet). Shared by `Arc`:
    /// small by construction, concatenated with persisted rows at
    /// cache-build time. Never mixed into `step_rows`, so identical
    /// consecutive steps are never dropped and TCP rows can never duplicate
    /// persisted ones.
    live_overlay: Arc<BTreeMap<i64, Vec<TrialRow>>>,
    chart_mode: ChartMode,
    chart_source: ChartSource,
    chart_view: ChartView,
    chart_selected: usize,
    metrics_len: usize,
    chart_zoom: f64,
    last_error: Option<String>,
    selected_trial_idx: usize,
    params_x_offset: usize,
    enabled_axes: Vec<AxisKey>,
    scroll_handle: Option<ScrollHandle>,
    /// Snapshot versions from the last `AppState` push
    /// `(data_version, pending_version)`. The 60fps render builds its cache
    /// fingerprints from these — no other per-frame state lives here.
    snapshot_versions: (u64, u64),
    /// All render caches, grouped so `ChartsView` stays lean. Every field is
    /// derived data, rebuildable from a fingerprint mismatch.
    cache: ChartCache,
}

/// Derived render data for `ChartsView`, kept in one child struct instead of
/// a sprawl of parallel cache fields. `rows`/`series` rebuild on the full
/// snapshot fingerprint; the hyperparameter-space entries rebuild on
/// `(data_version, enabled_axes)`.
#[derive(Debug, Default)]
struct ChartCache {
    /// Key for `rows` + `series`: (selected trial, source, data, pending).
    fingerprint: Option<(Option<i64>, ChartSource, u64, u64)>,
    /// Selected chain's concatenated rows (persisted + live overlay).
    rows: Arc<Vec<TrialRow>>,
    /// Parsed metric series swept from `rows`: `(key, points)` sorted by key.
    /// `f64` parsing happens once per fingerprint change here — never per
    /// frame. Each series is separately `Arc`d so frames share it.
    series: SharedSeriesList,
    /// Snapshot version the HP entries below were built from.
    hp_version: u64,
    /// Enabled-axis list the HP entries were built from, in order.
    hp_axes: Vec<AxisKey>,
    /// Per-axis domains parallel to `hp_axes`.
    hp_domains: Vec<ParamDomain>,
    /// Objective range across trials.
    hp_obj_range: Option<(f64, f64)>,
}

impl ChartCache {
    fn rows_valid(&self, fingerprint: &(Option<i64>, ChartSource, u64, u64)) -> bool {
        self.fingerprint.as_ref() == Some(fingerprint)
    }

    fn hp_valid(&self, version: u64, axes: &[AxisKey]) -> bool {
        self.hp_version == version && self.hp_axes == axes
    }
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
        // Record-source toggle (steps vs epochs) — chart-layer key like the
        // other chart keys, so it only fires while the Charts window holds
        // focus. Selection resets: the two streams expose different keys.
        if kb.matches(TermWmAction::Custom(2), key) {
            self.chart_source = match self.chart_source {
                ChartSource::Steps => ChartSource::Epochs,
                ChartSource::Epochs => ChartSource::Steps,
            };
            self.chart_selected = 0;
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

// Per-window views: each pane is a thin struct owning its stateful component,
// exposing it through a `view()` that returns the child directly.
// `impl_view_component!`'s `child:` form forwards the component lifecycle to
// the per-frame view and delegates `desired_height` + selection/hitbox to the
// child field, so variable-height content keeps its layout contract and the
// copy pipeline (which reads selection off the focused window root) keeps
// working.

struct TrialsWindow {
    sv: ScrollViewComponent<ListComponent>,
}

impl TrialsWindow {
    fn view(&mut self) -> impl Component<TermWmAction> + '_ {
        &mut self.sv
    }
}

impl_view_component!(TrialsWindow, child: sv);

struct ChartsWindow {
    sv: ScrollViewComponent<ChartsView>,
}

impl ChartsWindow {
    fn view(&mut self) -> impl Component<TermWmAction> + '_ {
        &mut self.sv
    }
}

impl_view_component!(ChartsWindow, child: sv);

struct TextWindow {
    sv: ScrollViewComponent<TextRendererComponent>,
}

impl TextWindow {
    fn view(&mut self) -> impl Component<TermWmAction> + '_ {
        &mut self.sv
    }
}

impl_view_component!(TextWindow, child: sv);

struct ToggleWindow {
    list: ToggleListComponent,
}

impl ToggleWindow {
    fn view(&mut self) -> impl Component<TermWmAction> + '_ {
        &mut self.list
    }
}

impl_view_component!(ToggleWindow, child: list);

enum AppComponent {
    Trials(TrialsWindow),
    Charts(ChartsWindow),
    Details(TextWindow),
    Params(ToggleWindow),
    Metrics(ToggleWindow),
    ProjectInfo(TextWindow),
    Frontier(TextWindow),
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
    /// Shared snapshots. Components hold `Arc` clones, so pushing data is
    /// O(1); deltas merge in place via `Arc::make_mut`.
    trials: Arc<Vec<TrialRow>>,
    epoch_rows: Arc<BTreeMap<i64, Vec<TrialRow>>>,
    step_rows: Arc<BTreeMap<i64, Vec<TrialRow>>>,
    /// Next wall-clock time the palette-tick hook runs `refresh_trials`.
    /// Initialized to now so the first loop iteration loads immediately.
    next_poll_at: std::time::Instant,
    /// Live TCP rows with no DB twin visible yet. Kept OUT of `step_rows` so
    /// the strict `trial.time` DB twin can never permanently duplicate them,
    /// and identical consecutive steps are never dropped. Reconciled against
    /// persisted multiset counts every poll; concatenated with persisted rows
    /// only at chart cache-build time. Shared by `Arc`: pushes clone the
    /// handle (O(1)).
    pending_live_steps: Arc<BTreeMap<i64, Vec<TrialRow>>>,
    /// Bumped whenever the pending buffer mutates. Part of the chart cache
    /// fingerprint and the push gate.
    pending_version: u64,
    /// Per-trial multiset of persisted step hashes (excluding `trial.time`),
    /// counting twins not yet matched to a pending row. A `HashSet` would
    /// collapse repeats: one persisted plateau value would evict every later
    /// identical pending step before its own twin arrives. Counts make each
    /// persisted row consumable exactly once, in arrival order.
    persisted_step_hashes: BTreeMap<i64, BTreeMap<u64, usize>>,
    step_subscriber: StepSubscriber,
    last_error: Option<String>,
    chart_mode: ChartMode,
    last_selected_trial: usize,
    /// Monotonic counter bumped whenever a poll actually delivers new or
    /// changed persisted rows. The 60fps `render()` only pushes data into
    /// components when this (or selection/mode/source/pending) moved.
    data_version: u64,
    /// Highest `row_id` already merged for the append-only tables. Polls
    /// fetch only `row_id > max_*`, so per-poll work stays proportional to
    /// new rows instead of re-parsing the whole step history. Reset to zero
    /// (with the maps cleared) if the DB file is replaced/truncated.
    max_epoch_row_id: i64,
    max_step_row_id: i64,
    /// Last signals consumed by `push_data_to_components`. When the current
    /// signals all match, the 60fps frame skips the push entirely.
    last_pushed_version: u64,
    last_pushed_pending: u64,
    last_pushed_selection: usize,
    last_pushed_mode: ChartMode,
    last_pushed_source: ChartSource,
    last_pushed_axes: Vec<AxisKey>,
    last_pushed_error: Option<String>,
    last_pushed_count: usize,
    /// Last `run_in_progress()` value rendered into the frontier window.
    /// The frontier text rebuilds only when trials change or this flips.
    last_frontier_in_progress: bool,
    /// Whether the current snapshot is multi-objective (any `score.*` field).
    /// Refreshed only when `trials` changes; the per-frame title/scan paths
    /// read this instead of re-scanning every trial's fields at 60Hz.
    cached_multi_objective: bool,
    /// Last signals consumed by `update_titles`, which runs AFTER
    /// `render_app` so `metrics_len` is never one frame stale. Idle frames
    /// allocate nothing here.
    last_title_trial: Option<i64>,
    last_title_mode: ChartMode,
    last_title_source: ChartSource,
    last_title_view: ChartView,
    last_title_selected: usize,
    last_title_metrics_len: usize,
    last_title_focused: bool,
    last_title_multi: bool,
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
            Some(AppRootComponent::Custom(AppComponent::Trials(win))) => Some(&mut win.sv),
            _ => None,
        }
    }

    fn charts_sv(&mut self) -> Option<&mut ScrollViewComponent<ChartsView>> {
        match self.inner.wm().component_for_key_mut(self.charts_key) {
            Some(AppRootComponent::Custom(AppComponent::Charts(win))) => Some(&mut win.sv),
            _ => None,
        }
    }

    fn details_sv(&mut self) -> Option<&mut ScrollViewComponent<TextRendererComponent>> {
        match self.inner.wm().component_for_key_mut(self.details_key) {
            Some(AppRootComponent::Custom(AppComponent::Details(win))) => Some(&mut win.sv),
            _ => None,
        }
    }

    fn params_list(&mut self) -> Option<&mut ToggleListComponent> {
        match self.inner.wm().component_for_key_mut(self.params_key) {
            Some(AppRootComponent::Custom(AppComponent::Params(win))) => Some(&mut win.list),
            _ => None,
        }
    }

    fn metrics_list(&mut self) -> Option<&mut ToggleListComponent> {
        match self.inner.wm().component_for_key_mut(self.metrics_key) {
            Some(AppRootComponent::Custom(AppComponent::Metrics(win))) => Some(&mut win.list),
            _ => None,
        }
    }

    fn project_info_sv(&mut self) -> Option<&mut ScrollViewComponent<TextRendererComponent>> {
        match self.inner.wm().component_for_key_mut(self.project_info_key) {
            Some(AppRootComponent::Custom(AppComponent::ProjectInfo(win))) => Some(&mut win.sv),
            _ => None,
        }
    }

    fn frontier_sv(&mut self) -> Option<&mut ScrollViewComponent<TextRendererComponent>> {
        match self.inner.wm().component_for_key_mut(self.frontier_key) {
            Some(AppRootComponent::Custom(AppComponent::Frontier(win))) => Some(&mut win.sv),
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

    /// Push snapshot data into the window components, but only when one of
    /// the cheap per-frame signals moved. The 60fps `render()` calls this
    /// every frame; without the gate each frame paid for full clones of the
    /// step/epoch maps plus detail/frontier rebuilds — the sustained CPU
    /// burn. Polls bump `data_version`/`pending_version`, so idle frames now
    /// do a handful of borrows and return. Snapshot sharing is `Arc` (O(1));
    /// only the selected trial's detail text and epoch slice are rebuilt,
    /// and only on a version/selection/mode/source change.
    ///
    /// The axis gate compares `Vec<AxisKey>` directly: one small allocation
    /// per frame for the toggle walk, zero string re-allocations, and no
    /// component push unless the set actually moved.
    fn push_data_to_components(&mut self) {
        let selected = self.selected_trial_idx();
        let mode = self.chart_mode;
        let source = match self.charts_sv() {
            Some(sv) => sv.content.borrow().chart_source,
            None => ChartSource::Steps,
        };
        let axes = self.enabled_axes();

        if self.last_pushed_version == self.data_version
            && self.last_pushed_pending == self.pending_version
            && self.last_pushed_selection == selected
            && self.last_pushed_mode == mode
            && self.last_pushed_source == source
            && self.last_pushed_axes == axes
            && self.last_pushed_error == self.last_error
            && self.last_pushed_count == self.trials.len()
        {
            return;
        }
        self.last_pushed_version = self.data_version;
        self.last_pushed_pending = self.pending_version;
        self.last_pushed_selection = selected;
        self.last_pushed_mode = mode;
        self.last_pushed_source = source;
        self.last_pushed_axes = axes.clone();
        self.last_pushed_error = self.last_error.clone();
        self.last_pushed_count = self.trials.len();

        let last_error = self.last_error.clone();
        let data_version = self.data_version;
        let pending_version = self.pending_version;
        // Clone the `Arc` handles (O(1)) BEFORE borrowing any component:
        // `charts_sv()` holds `&mut self` while the borrow lives.
        let trials = Arc::clone(&self.trials);
        let epoch_rows = Arc::clone(&self.epoch_rows);
        let step_rows = Arc::clone(&self.step_rows);
        let live_overlay = Arc::clone(&self.pending_live_steps);
        let count = self.trials.len();

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

        // Detail text built from the local handles BEFORE borrowing any
        // component (`details_sv()` holds `&mut self` while live).
        let detail_text = {
            let idx = selected.min(trials.len().saturating_sub(1));
            match trials.get(idx) {
                Some(trial) => {
                    let rows = epoch_rows.get(&trial.trial_id).cloned().unwrap_or_default();
                    Text::from(trial_detail_lines(trial, &rows, &trials))
                }
                None => Text::from(vec![Line::from("No trial selected.")]),
            }
        };

        if let Some(sv) = self.charts_sv() {
            let mut c = sv.content.borrow_mut();
            c.trials = trials;
            c.epoch_rows = epoch_rows;
            c.step_rows = step_rows;
            c.live_overlay = live_overlay;
            c.chart_mode = mode;
            c.last_error = last_error.clone();
            c.selected_trial_idx = selected;
            // Moved, not cloned: `axes` is not read after this block.
            c.enabled_axes = axes;
            c.snapshot_versions = (data_version, pending_version);
        }
        if let Some(sv) = self.details_sv() {
            sv.content.borrow_mut().set_text(detail_text);
        }
        if count != self.project_info_count {
            self.project_info_count = count;
            // Clone the cached static lines BEFORE borrowing the component.
            let mut lines = self.project_info_static.clone();
            lines.push(Line::from(format!("Trial Count: {count}")));
            if let Some(sv) = self.project_info_sv() {
                sv.content.borrow_mut().set_text(Text::from(lines));
            }
        }
    }

    /// Window titles, refreshed AFTER `render_app` so `metrics_len` (set
    /// during chart render) is never one frame stale. Dirty-checked on
    /// (trial, mode, source, view, selection, metrics, focus, objective
    /// count): idle frames allocate nothing here.
    fn update_titles(&mut self) {
        let selected = self.selected_trial_idx();
        let mode = self.chart_mode;
        let charts_focused = self.inner.wm().focused_window() == self.charts_key;
        let (source, view, chart_selected, metrics_len) = match self.charts_sv() {
            Some(sv) => {
                let c = sv.content.borrow();
                (
                    c.chart_source,
                    c.chart_view,
                    c.chart_selected,
                    c.metrics_len,
                )
            }
            None => (ChartSource::Steps, ChartView::Summary, 0, 0),
        };
        let trial_id = self.trials.get(selected).map(|t| t.trial_id);
        let multi = self.cached_multi_objective;

        if self.last_title_trial == trial_id
            && self.last_title_mode == mode
            && self.last_title_source == source
            && self.last_title_view == view
            && self.last_title_selected == selected
            && self.last_title_metrics_len == metrics_len
            && self.last_title_focused == charts_focused
            && self.last_title_multi == multi
        {
            return;
        }
        self.last_title_trial = trial_id;
        self.last_title_mode = mode;
        self.last_title_source = source;
        self.last_title_view = view;
        self.last_title_selected = selected;
        self.last_title_metrics_len = metrics_len;
        self.last_title_focused = charts_focused;
        self.last_title_multi = multi;

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
            charts_window_title(
                mode,
                source,
                view,
                chart_selected,
                metrics_len,
                charts_focused,
                trial_id,
            ),
        );
        wm.set_window_title(
            self.frontier_key,
            if multi {
                "Pareto Frontier"
            } else {
                "Best Trials"
            },
        );
    }

    fn refresh_trials(&mut self) {
        let mut saw_activity = false;
        // Net-change snapshot: `pending_version` bumps at most once below,
        // only if the buffer actually differs when the tick ends. The common
        // tick (TCP frame + SQLite rows landing together, reconciled away in
        // the same pass) leaves the version untouched — no redundant pushes
        // or chart cache rebuilds.
        let pending_before = Arc::clone(&self.pending_live_steps);
        // Live step rows arriving over TCP since the last poll. They land in
        // the ephemeral pending buffer — NEVER in `step_rows` — so the
        // writer-stamped `trial.time` twin in SQLite can never permanently
        // duplicate them, and identical consecutive steps are never dropped.
        const MAX_PENDING_PER_TRIAL: usize = 2000;
        while let Some(line) = self.step_subscriber.try_recv() {
            saw_activity = true;
            if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line)
                && let (Some(trial_id), Some(steps)) = (
                    msg.get("trial_id").and_then(|v| v.as_i64()),
                    msg.get("steps").and_then(|v| v.as_array()),
                )
            {
                let pending = Arc::make_mut(&mut self.pending_live_steps);
                let entry = pending.entry(trial_id).or_default();
                for fields in steps.iter().filter_map(|s| s.as_object()) {
                    let mut map = BTreeMap::new();
                    for (k, v) in fields {
                        map.insert(k.clone(), v.as_str().unwrap_or("").to_string());
                    }
                    entry.push(TrialRow {
                        trial_id,
                        status: "running".to_string(),
                        elapsed_ms: 0,
                        error: None,
                        fields: map,
                    });
                }
                if entry.len() > MAX_PENDING_PER_TRIAL {
                    let drain = entry.len() - MAX_PENDING_PER_TRIAL;
                    entry.drain(..drain);
                }
            }
        }
        let after_epoch = self.max_epoch_row_id;
        let after_step = self.max_step_row_id;
        match load_trials_after(&self.db_path, after_epoch, after_step) {
            Ok(poll) => {
                // Single-connection reset detection: the file was replaced or
                // truncated under us. Drop the world and treat this poll's
                // deltas as a fresh full load — otherwise every new row is
                // silently skipped by a stale cursor.
                if poll.epoch_reset || poll.step_reset {
                    if poll.epoch_reset {
                        self.epoch_rows = Arc::new(BTreeMap::new());
                        self.max_epoch_row_id = 0;
                    }
                    if poll.step_reset {
                        self.step_rows = Arc::new(BTreeMap::new());
                        self.persisted_step_hashes.clear();
                        self.max_step_row_id = 0;
                    }
                    if !self.pending_live_steps.is_empty() {
                        self.pending_live_steps = Arc::new(BTreeMap::new());
                    }
                    self.data_version = self.data_version.wrapping_add(1);
                }
                let prev_sel = match self.trials_sv() {
                    Some(sv) => sv.content.borrow().selected(),
                    None => 0,
                };
                let prev_trial_id = self.trials.get(prev_sel).map(|t| t.trial_id);
                let trials_changed = poll.trials[..] != self.trials[..];
                if trials_changed {
                    self.trials = Arc::new(poll.trials);
                    self.cached_multi_objective = is_multi_objective(&self.trials);
                }
                // Append-only tables: merge the delta so per-poll work is
                // proportional to new rows, not the full history. `make_mut`
                // clones only on a shared `Arc` (i.e. once per dirty poll),
                // while every 60fps push shares with a single `Arc::clone`.
                let mut deltas_changed = false;
                if poll.epoch_delta.values().any(|rows| !rows.is_empty()) {
                    deltas_changed = true;
                    let maps = Arc::make_mut(&mut self.epoch_rows);
                    for (trial_id, rows) in poll.epoch_delta {
                        maps.entry(trial_id).or_default().extend(rows);
                    }
                }
                if poll.step_delta.values().any(|rows| !rows.is_empty()) {
                    deltas_changed = true;
                    let maps = Arc::make_mut(&mut self.step_rows);
                    for (trial_id, rows) in poll.step_delta {
                        // Multiset: each persisted row is consumable exactly
                        // once, so N identical pending steps need N twins.
                        let counts = self.persisted_step_hashes.entry(trial_id).or_default();
                        for row in &rows {
                            *counts.entry(step_row_hash(&row.fields)).or_insert(0) += 1;
                        }
                        maps.entry(trial_id).or_default().extend(rows);
                    }
                }
                self.max_epoch_row_id = poll.max_epoch;
                self.max_step_row_id = poll.max_step;
                // Reconcile: consume one persisted twin per pending row, in
                // arrival order. Only pending rows are ever dropped —
                // persisted history is append-only and untouched.
                if !self.pending_live_steps.is_empty() {
                    let pending = Arc::make_mut(&mut self.pending_live_steps);
                    reconcile_pending(pending, &mut self.persisted_step_hashes);
                }
                if self.pending_live_steps != pending_before {
                    self.pending_version = self.pending_version.wrapping_add(1);
                }
                // First poll (or post-reset) full load: index counts for every
                // persisted row exactly once, as each row passes through here.
                // (Covered above: deltas carry all rows when marks are zero.)
                if trials_changed || deltas_changed {
                    self.data_version = self.data_version.wrapping_add(1);
                }
                // Activity = fresh TCP frames, a `running` trial row, or newly
                // persisted rows. Never the append-only history scan: step and
                // epoch rows keep `status = "running"` forever by design, so
                // scanning them would latch "Run in progress" permanently.
                if self.trials.iter().any(|t| t.status == "running") || deltas_changed {
                    saw_activity = true;
                }
                if saw_activity {
                    self.last_activity = Some(std::time::Instant::now());
                }
                let in_progress = self.run_in_progress();
                if trials_changed {
                    let items = build_trial_items(&self.trials);
                    if let Some(sv) = self.trials_sv() {
                        sv.content.borrow_mut().update_items(items);
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
                }
                if trials_changed || in_progress != self.last_frontier_in_progress {
                    self.last_frontier_in_progress = in_progress;
                    let frontier = frontier_lines(&self.trials, in_progress);
                    if let Some(sv) = self.frontier_sv() {
                        sv.content.borrow_mut().set_text(Text::from(frontier));
                    }
                }
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
        // Titles after the draw so `metrics_len` (set during chart render)
        // is never one frame stale.
        self.update_titles();
    }

    /// The SQLite poll, driven off the palette-tick hook instead of a
    /// repeating scheduler task — deliberately. Any outstanding
    /// `TaskScheduler` entry makes `has_pending()` true, which pins the
    /// runner's power profile at Streaming (16ms poll, ~60fps full redraws)
    /// for the lifetime of the process. The palette hook instead only
    /// clamps the idle sleep to the next deadline, so the loop sits in
    /// PowerSaver and wakes on input or once per `--poll-ms`.
    fn poll_palette_tick(&mut self) -> bool {
        let now = std::time::Instant::now();
        if now < self.next_poll_at {
            return false;
        }
        self.refresh_trials();
        // Stamp AFTER the poll completes so a slow poll can never produce a
        // zero-sleep spin: the loop always sleeps at least `poll` between
        // consecutive SQLite passes, even at the 16ms floor.
        self.next_poll_at = std::time::Instant::now() + self.poll;
        // One frame per poll unconditionally: cheap (push/titles are gated),
        // and it guarantees the run-state header flip is never stranded.
        true
    }

    fn palette_tick_deadline(&self) -> Option<Duration> {
        Some(
            self.next_poll_at
                .saturating_duration_since(std::time::Instant::now()),
        )
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
    chart_source: ChartSource,
    chart_view: ChartView,
    chart_selected: usize,
    metrics_len: usize,
    charts_focused: bool,
    trial_id: Option<i64>,
) -> String {
    // The source suffix answers "step or epoch?": step rows sweep
    // `model.step_end.*`, epoch rows sweep `model.epoch_end.*`.
    let src = chart_source.label();
    match chart_mode {
        ChartMode::Metrics if charts_focused && chart_view == ChartView::Focused => {
            let total = metrics_len;
            let current = chart_selected.saturating_add(1);
            match trial_id {
                Some(id) => format!("Trial {id} - Metric Curve {current}/{total} [{src}]"),
                None => format!("Metric Curve {current}/{total} [{src}]"),
            }
        }
        ChartMode::Metrics => match trial_id {
            Some(id) => format!("Trial {id} - Metric Curves [{src}]"),
            None => format!("Metric Curves [{src}]"),
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
    let source = kb
        .combos_for(TermWmAction::Custom(2))
        .first()
        .cloned()
        .unwrap_or_default();
    format!(
        "[{zoom_in}] zoom in    [{zoom_out}] zoom out    [{reset}] reset    [{list}] list view    [{source}] steps/epochs"
    )
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
    // Chart record-source toggle (steps vs epochs).
    kb.add(
        TermWmAction::Custom(2),
        KeyCombo::new(KeyCode::Char('e'), KeyModifiers::NONE),
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

fn mk_trials_window() -> TrialsWindow {
    let mut sv = ScrollViewComponent::new(ListComponent::new("Trials"));
    sv.set_keyboard_mode(ScrollKeyMode::None);
    TrialsWindow { sv }
}

fn mk_charts_window() -> ChartsWindow {
    let mut sv = ScrollViewComponent::new(ChartsView {
        trials: Arc::new(Vec::new()),
        epoch_rows: Arc::new(BTreeMap::new()),
        step_rows: Arc::new(BTreeMap::new()),
        live_overlay: Arc::new(BTreeMap::new()),
        chart_mode: ChartMode::Metrics,
        chart_source: ChartSource::Steps,
        chart_view: ChartView::Summary,
        chart_selected: 0,
        metrics_len: 0,
        chart_zoom: 1.0,
        last_error: None,
        selected_trial_idx: 0,
        params_x_offset: 0,
        enabled_axes: Vec::new(),
        scroll_handle: None,
        snapshot_versions: (0, 0),
        cache: ChartCache::default(),
    });
    // PageUp/PageDown/Home/End scroll the chart list (the wrapper owns the
    // viewport). Up/Down fall through to ChartsView for selection moves.
    sv.set_keyboard_mode(ScrollKeyMode::PaginationOnly);
    ChartsWindow { sv }
}

fn mk_text_window() -> TextWindow {
    let mut sv = ScrollViewComponent::new(TextRendererComponent::new());
    // Keep one field per line (no reflow) so the key = value layout is preserved;
    // long values scroll horizontally if wider than the window.
    sv.content.borrow_mut().set_wrap(false);
    // Drag-to-select + copy-on-release via term-wm's selection/clipboard pipeline.
    sv.content.borrow_mut().set_selection_enabled(true);
    // Full keyboard scroll: Up/Down/PageUp/PageDown/Home/End all scroll the
    // details viewport. TextRendererComponent has no key handling of its own.
    sv.set_keyboard_mode(ScrollKeyMode::Full);
    TextWindow { sv }
}

fn mk_toggle_window(title: &str) -> ToggleWindow {
    ToggleWindow {
        list: ToggleListComponent::new(title),
    }
}

fn sync_param_toggles(app: &mut AppState) {
    let mut names = BTreeSet::new();
    let mut metric_names = BTreeSet::new();
    for trial in app.trials.iter() {
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

/// Delta poll: `trial_records` is tiny (one row per trial, upserted in
/// place) so it reloads fully, but the append-only epoch/step tables fetch
/// only `row_id > max_*` — per-poll work stays proportional to new rows
/// instead of re-parsing the whole step history every `--poll-ms`.
///
/// Reset detection happens on this same connection: a file max below the
/// caller's cursor (or a missing table with a nonzero cursor) means the DB
/// file was replaced or truncated, so the effective cursor resets to zero
/// and the caller must drop its maps and treat the deltas as a full load.
fn load_trials_after(
    path: &Path,
    after_epoch_id: i64,
    after_step_id: i64,
) -> Result<TrialsPoll, String> {
    let conn = open_connection(path)?;
    let db_epoch = db_max_row_id(&conn, "trial_epoch_records")?;
    let db_step = db_max_row_id(&conn, "trial_step_records")?;
    let epoch_reset =
        db_epoch.is_none() && after_epoch_id > 0 || db_epoch.is_some_and(|m| m < after_epoch_id);
    let step_reset =
        db_step.is_none() && after_step_id > 0 || db_step.is_some_and(|m| m < after_step_id);
    let (eff_epoch, eff_step) = (
        if epoch_reset { 0 } else { after_epoch_id },
        if step_reset { 0 } else { after_step_id },
    );
    let trials = load_trial_rows(&conn, "trial_records")?;
    let (epoch_delta, max_epoch) = load_epoch_rows_after(&conn, eff_epoch)?;
    let (step_delta, max_step) = load_step_rows_after(&conn, eff_step)?;
    Ok(TrialsPoll {
        trials,
        epoch_delta,
        step_delta,
        max_epoch,
        max_step,
        epoch_reset,
        step_reset,
    })
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

/// Fetch only epoch rows with `row_id > after`, returning the new high-water
/// mark alongside. Callers merge the delta into their existing map.
fn load_epoch_rows_after(
    conn: &Connection,
    after: i64,
) -> Result<(BTreeMap<i64, Vec<TrialRow>>, i64), String> {
    let mut stmt = match conn.prepare(
        r#"
        SELECT row_id, trial_id, status, elapsed_ms, error, fields_json
        FROM trial_epoch_records
        WHERE row_id > ?1
        ORDER BY row_id ASC
        "#,
    ) {
        Ok(stmt) => stmt,
        Err(err) => {
            if err
                .to_string()
                .contains("no such table: trial_epoch_records")
            {
                return Ok((BTreeMap::new(), after));
            }
            return Err(err.to_string());
        }
    };
    let rows = stmt
        .query_map([after], |row| {
            let fields_json: String = row.get(5)?;
            let fields: BTreeMap<String, String> =
                serde_json::from_str(&fields_json).map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(err),
                    )
                })?;
            // `row_id` stays in the loader cursor only — never in `TrialRow`
            // — so high-water marks can't leak into equality or display.
            let row_id: i64 = row.get(0)?;
            let trial = TrialRow {
                trial_id: row.get(1)?,
                status: row.get(2)?,
                elapsed_ms: row.get(3)?,
                error: row.get(4)?,
                fields,
            };
            Ok((row_id, trial))
        })
        .map_err(|err| err.to_string())?;

    let mut by_trial: BTreeMap<i64, Vec<TrialRow>> = BTreeMap::new();
    let mut max_id = after;
    for row in rows {
        let (row_id, row) = row.map_err(|err| err.to_string())?;
        max_id = max_id.max(row_id);
        by_trial.entry(row.trial_id).or_default().push(row);
    }
    Ok((by_trial, max_id))
}

/// Fetch only step rows with `row_id > after`, returning the new high-water
/// mark alongside. Callers merge the delta into their existing map.
fn load_step_rows_after(conn: &Connection, after: i64) -> Result<(TrialStepRows, i64), String> {
    let mut stmt = match conn.prepare(
        r#"
        SELECT row_id, trial_id, status, elapsed_ms, error, fields_json
        FROM trial_step_records
        WHERE row_id > ?1
        ORDER BY row_id ASC
        "#,
    ) {
        Ok(stmt) => stmt,
        Err(err) => {
            if err
                .to_string()
                .contains("no such table: trial_step_records")
            {
                return Ok((BTreeMap::new(), after));
            }
            return Err(err.to_string());
        }
    };
    let rows = stmt
        .query_map([after], |row| {
            let fields_json: String = row.get(5)?;
            let fields: BTreeMap<String, String> =
                serde_json::from_str(&fields_json).map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(err),
                    )
                })?;
            let row_id: i64 = row.get(0)?;
            let trial = TrialRow {
                trial_id: row.get(1)?,
                status: row.get(2)?,
                elapsed_ms: row.get(3)?,
                error: row.get(4)?,
                fields,
            };
            Ok((row_id, trial))
        })
        .map_err(|err| err.to_string())?;

    let mut by_trial: BTreeMap<i64, Vec<TrialRow>> = BTreeMap::new();
    let mut max_id = after;
    for row in rows {
        let (row_id, row) = row.map_err(|err| err.to_string())?;
        max_id = max_id.max(row_id);
        by_trial.entry(row.trial_id).or_default().push(row);
    }
    Ok((by_trial, max_id))
}

// TODO: SQL queries should not be here
fn open_connection(path: &Path) -> Result<Connection, String> {
    if !path.exists() {
        return Err(format!("missing db: {}", path.display()));
    }

    // Plain read-only open (no URI): no percent-escaping hazards, and the
    // reader follows `-wal` state instead of `immutable=1` freezing it.
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|err| format!("db open failed (read-only) for {}: {err}", path.display()))?;
    conn.busy_timeout(Duration::from_millis(50))
        .map_err(|err| format!("db busy_timeout failed for {}: {err}", path.display()))?;
    Ok(conn)
}

/// Current `MAX(row_id)` per append-only table, or `None` when the table does
/// not exist yet (project created, no run recorded).
fn db_max_row_id(conn: &Connection, table: &str) -> Result<Option<i64>, String> {
    let sql = format!("SELECT COALESCE(MAX(row_id), 0) FROM {table}");
    match conn.query_row(&sql, [], |row| row.get::<_, i64>(0)) {
        Ok(max) => Ok(Some(max)),
        Err(err) => {
            if err.to_string().contains("no such table") {
                Ok(None)
            } else {
                Err(err.to_string())
            }
        }
    }
}

/// Normalized field hash for step-twin reconciliation: `trial.time` is
/// writer-stamped AFTER the TCP push, so twins differ by exactly that key.
/// Hashing the rest identifies twins without false-dropping identical
/// consecutive steps (only pending rows are ever dropped, never persisted
/// ones).
fn step_row_hash(fields: &BTreeMap<String, String>) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    for (key, value) in fields
        .iter()
        .filter(|(k, _)| k.as_str() != FIELD_TRIAL_TIME)
    {
        key.hash(&mut hasher);
        value.hash(&mut hasher);
    }
    hasher.finish()
}

/// Drop pending rows whose persisted twin has arrived, consuming one multiset
/// count per row in arrival order. N identical pending steps need N persisted
/// twins: with zero or one twin persisted, at least one pending copy survives
/// (its own twin is still in flight). Only pending rows are ever removed.
fn reconcile_pending(
    pending: &mut BTreeMap<i64, Vec<TrialRow>>,
    counts: &mut BTreeMap<i64, BTreeMap<u64, usize>>,
) {
    let mut drained = Vec::new();
    for (trial_id, rows) in pending.iter_mut() {
        if let Some(trial_counts) = counts.get_mut(trial_id) {
            rows.retain(|row| {
                let hash = step_row_hash(&row.fields);
                match trial_counts.get_mut(&hash) {
                    Some(left) if *left > 0 => {
                        *left -= 1;
                        if *left == 0 {
                            trial_counts.remove(&hash);
                        }
                        false
                    }
                    _ => true,
                }
            });
        }
        if rows.is_empty() {
            drained.push(*trial_id);
        }
    }
    for trial_id in drained {
        pending.remove(&trial_id);
    }
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
            // Lineage annotation: which hyperparameter config this trial
            // belongs to and which rung it ran. Promotion chains read as
            // cfg3: t3(r0) → t5(r1) → t8(r2). Order untouched (selection
            // indexes self.trials positionally).
            let cfg = trial
                .fields
                .get(FIELD_TRIAL_CONFIG_ID)
                .map(String::as_str)
                .unwrap_or("?");
            let rung = trial
                .fields
                .get(FIELD_TRIAL_RUNG)
                .map(String::as_str)
                .unwrap_or("?");
            let budget = trial
                .fields
                .get(FIELD_TRIAL_BUDGET_EPOCHS)
                .map(String::as_str)
                .unwrap_or("?");
            format!(
                "trial {:>3}  {:<7}  {:<5}  cfg={:<4} r{rung}/{budget}  {}",
                trial.trial_id, trial.status, tag, cfg, metric_text
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
    let mut params: Vec<String> = trial
        .fields
        .iter()
        .filter(|(k, _)| k.starts_with("hp."))
        .map(|(k, v)| format!("    {:<20} {}", k.trim_start_matches("hp."), v))
        .collect();
    if params.is_empty() {
        return params;
    }
    let mut out = vec!["  Hyperparameters:".to_string()];
    out.append(&mut params);
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
    // Lineage: same-config trials up to the selected trial's rung,
    // ordered oldest-first, so a promoted trial's curve continues its
    // ancestors instead of restarting at x=0 — and never leaks in later
    // rungs the selected trial hasn't reached. Rows keep arrival-position
    // x across the concatenation (rungs are chronological), so curves
    // still never fold. Falls back to the selected trial alone when it
    // has no config key or no siblings.
    // Cached on (selected trial, source, data_version, pending_version):
    // rebuilding the chain concatenation plus the metric-key sweep every
    // 60fps frame was the per-frame O(steps x metrics) burn. Idle frames
    // reuse the cache. The cache lives behind `Arc`: readers clone the
    // handles (O(1)) and a panic mid-render can never strand an emptied
    // cache under a stale fingerprint.
    let fingerprint = (
        Some(trial.trial_id),
        charts.chart_source,
        charts.snapshot_versions.0,
        charts.snapshot_versions.1,
    );
    if !charts.cache.rows_valid(&fingerprint) {
        let selected_rung: i64 = trial
            .fields
            .get(FIELD_TRIAL_RUNG)
            .and_then(|r| r.parse::<i64>().ok())
            .unwrap_or(i64::MAX);
        let mut chain: Vec<&TrialRow> = charts
            .trials
            .iter()
            .filter(|t| {
                trial
                    .fields
                    .get(FIELD_TRIAL_CONFIG_ID)
                    .is_some_and(|c| t.fields.get(FIELD_TRIAL_CONFIG_ID) == Some(c))
                    && t.fields
                        .get(FIELD_TRIAL_RUNG)
                        .and_then(|r| r.parse::<i64>().ok())
                        .is_some_and(|r| r <= selected_rung)
            })
            .collect();
        chain.sort_by_key(|t| {
            t.fields
                .get(FIELD_TRIAL_RUNG)
                .and_then(|r| r.parse::<i64>().ok())
                .unwrap_or(i64::MAX)
        });
        if chain.is_empty() {
            chain.push(trial);
        }
        // Live step rows are the finest-grained live data (persisted live
        // while the trial runs); the source toggle picks which stream the
        // charts sweep, falling back to the other when the selected one is
        // empty (e.g. epochs for a trial that emitted no steps). Never
        // merged: shared `metric.*` keys hold different values per stream
        // (step loss vs epoch val_loss). Selection applies per chain link,
        // then links concatenate in rung order. Unreconciled TCP rows ride
        // in `live_overlay` behind the persisted rows of their own trial.
        let mut owned: Vec<TrialRow> = Vec::new();
        for link in &chain {
            let empty: Vec<TrialRow> = Vec::new();
            let steps = charts.step_rows.get(&link.trial_id).unwrap_or(&empty);
            let epochs = charts.epoch_rows.get(&link.trial_id).unwrap_or(&empty);
            let live = charts.live_overlay.get(&link.trial_id).unwrap_or(&empty);
            match charts.chart_source {
                ChartSource::Steps if !steps.is_empty() || !live.is_empty() => {
                    owned.extend(steps.iter().cloned());
                    owned.extend(live.iter().cloned());
                }
                ChartSource::Epochs if !epochs.is_empty() => {
                    owned.extend(epochs.iter().cloned());
                }
                _ => {
                    if !steps.is_empty() || !live.is_empty() {
                        owned.extend(steps.iter().cloned());
                        owned.extend(live.iter().cloned());
                    } else {
                        owned.extend(epochs.iter().cloned());
                    }
                }
            }
        }
        // Parse every numeric `metric.*` series once per fingerprint change.
        // Per-frame parsing (string `f64` parses plus a `Vec` per chart) was
        // the residual 60Hz allocation burn whenever input kept the loop in
        // Streaming.
        let series = collect_metric_keys(&owned)
            .into_iter()
            .map(|key| {
                let points = Arc::new(metric_series_for_key(&owned, &key));
                (key, points)
            })
            .collect::<Vec<_>>();
        charts.cache.rows = Arc::new(owned);
        charts.cache.series = Arc::new(series);
        charts.cache.fingerprint = Some(fingerprint);
    }
    if charts.cache.rows.is_empty() {
        Paragraph::new("No epoch or step metrics for selected trial.")
            .render(area, &mut backend.buffer);
        return;
    }
    // x is always the row's arrival position (1-based). Rows arrive in
    // chronological order, so the curve can never fold back on itself, flip
    // mid-run, or depend on field names in any way.
    let series_list = Arc::clone(&charts.cache.series);
    render_metric_charts_cached(backend, charts, &series_list, area, ctx);
}

fn render_metric_charts_cached(
    backend: &mut RatatuiBackend,
    charts: &mut ChartsView,
    series_list: &[(String, SharedPoints)],
    area: Rect,
    ctx: &ComponentContext,
) {
    charts.metrics_len = series_list.len();
    if series_list.is_empty() {
        Paragraph::new("No numeric metric curves.").render(area, &mut backend.buffer);
        return;
    }
    if charts.chart_selected >= series_list.len() {
        charts.chart_selected = series_list.len().saturating_sub(1);
    }

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
            let total_items = series_list.len();
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
            let last_chart = (first_chart + max_visible + 2).min(series_list.len());

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
                let (key, series) = &series_list[abs_idx];
                render_metric_chart(backend, series, key, rect, charts.chart_zoom);
            }
        }
        ChartView::Focused => {
            if let Some(handle) = ctx.scroll_handle() {
                handle.set_content_size(chart_area.width as usize, chart_area.height as usize);
            }
            let (key, series) = &series_list[charts.chart_selected];
            render_metric_chart(backend, series, key, chart_area, charts.chart_zoom);
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
    // Cache per-axis domains + objective range: rebuilding them parses every
    // trial's fields on every frame. Keyed by snapshot version + the exact
    // enabled-axis list; per-trial colors and the selection highlight below
    // stay live (cheap float lookups, no parsing).
    if !charts
        .cache
        .hp_valid(charts.snapshot_versions.0, &charts.enabled_axes)
    {
        let domains = charts
            .enabled_axes
            .iter()
            .map(|axis| param_domain(&charts.trials, &axis.name))
            .collect::<Vec<_>>();
        let obj_range = objective_range(&charts.trials);
        charts.cache.hp_version = charts.snapshot_versions.0;
        charts.cache.hp_axes = charts.enabled_axes.clone();
        charts.cache.hp_domains = domains;
        charts.cache.hp_obj_range = obj_range;
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
    let domains = &charts.cache.hp_domains[start..end];
    let objective_range = charts.cache.hp_obj_range;
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

/// X-bounds for a metric chart: the full arrival-position range, or the
/// trailing `zoom` fraction of it when zoomed. Degenerate single-point
/// ranges expand to width 1.0 BEFORE zooming, and zooming clamps against the
/// expanded full bounds — so a zoomed 1-point series keeps `[x, x + 1.0]`
/// and never collapses to zero width for Ratatui `Chart`.
fn chart_x_bounds(series: &[ChartPoint], zoom: f64) -> (f64, f64) {
    let (full_min_x, mut full_max_x) = match (series.first(), series.last()) {
        (Some(first), Some(last)) => (first.x, last.x),
        _ => (0.0, 1.0),
    };
    if full_max_x == full_min_x {
        full_max_x = full_min_x + 1.0;
    }
    if series.is_empty() || zoom >= 1.0 {
        return (full_min_x, full_max_x);
    }
    let window = ((full_max_x - full_min_x) * zoom).max(1e-6);
    let mut max_x = full_max_x;
    let mut min_x = max_x - window;
    if min_x < full_min_x {
        min_x = full_min_x;
        max_x = (min_x + window).min(full_max_x);
    }
    (min_x, max_x)
}

fn render_metric_chart(
    backend: &mut RatatuiBackend,
    series: &[ChartPoint],
    key: &str,
    area: Rect,
    zoom: f64,
) {
    // x is the arrival position (ascending), so the zoom window is found with
    // two binary searches — O(log n), not the old O(n) full-series scan.
    let (min_x, max_x) = chart_x_bounds(series, zoom);
    let start = series.partition_point(|point| point.x < min_x);
    let end = series.partition_point(|point| point.x <= max_x);
    let window = if start < end {
        &series[start..end]
    } else {
        series
    };
    // Decimate to the Braille grid: at most ~2 points per horizontal dot
    // column. `Chart::render` then draws O(width) segments no matter how
    // long the step history grows. Min/max bucketing preserves the window
    // extrema exactly, so y bounds match the undecimated curve.
    let max_points = ((area.width as usize) * 2).max(8);
    let drawn: Vec<ChartPoint>;
    let view: &[ChartPoint] = if window.len() > max_points {
        drawn = decimate_min_max(window, max_points);
        &drawn
    } else {
        window
    };
    let (mut min_y, mut max_y) = view
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), point| {
            (lo.min(point.y), hi.max(point.y))
        });
    if !min_y.is_finite() || !max_y.is_finite() {
        (min_y, max_y) = (0.0, 1.0);
    } else if min_y == max_y {
        max_y = min_y + 1.0;
    }
    let x_labels = axis_labels(min_x, max_x);
    let y_labels = axis_labels(min_y, max_y);
    let y_title = metric_axis_title(key);
    let title = if let Some(last) = series.last().map(|point| point.y) {
        format!("{key}  last={last:.4}")
    } else {
        key.to_string()
    };
    // Ratatui's `Dataset::data` takes `&[(f64, f64)]`, so the decimated view
    // is mapped at the boundary. Width-bounded (<= ~2 points per Braille
    // column), so this copy is trivially small — the full history is never
    // converted.
    let plot: Vec<(f64, f64)> = view.iter().map(|point| (point.x, point.y)).collect();
    let dataset = Dataset::default()
        .name(key.to_string())
        .graph_type(GraphType::Line)
        .marker(Marker::Braille)
        .style(Style::default().fg(Color::Cyan))
        .data(&plot);
    let chart = Chart::new(vec![dataset])
        .block(Block::default().borders(Borders::ALL).title(title))
        .x_axis(
            Axis::default()
                .title("record")
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

/// Reduce `window` to at most `max_points` points while preserving its y
/// extrema: one (min, max) pair per index bucket, ordered by x so the line
/// never folds. Requires `max_points >= 2`.
fn decimate_min_max(window: &[ChartPoint], max_points: usize) -> Vec<ChartPoint> {
    if window.len() <= max_points || max_points < 2 {
        return window.to_vec();
    }
    let buckets = max_points / 2;
    let mut out = Vec::with_capacity(buckets * 2);
    let n = window.len();
    for bucket in 0..buckets {
        let start = bucket * n / buckets;
        let end = ((bucket + 1) * n / buckets).max(start + 1).min(n);
        let slice = &window[start..end];
        let (mut lo, mut hi) = (0usize, 0usize);
        for (idx, point) in slice.iter().enumerate() {
            if point.y < slice[lo].y {
                lo = idx;
            }
            if point.y > slice[hi].y {
                hi = idx;
            }
        }
        if lo == hi {
            out.push(slice[lo]);
        } else if lo < hi {
            out.push(slice[lo]);
            out.push(slice[hi]);
        } else {
            out.push(slice[hi]);
            out.push(slice[lo]);
        }
    }
    out
}

/// Every numeric `metric.*` field gets its own chart. That is the entire
/// rule: no variance tests, no exclusions, no name lists. A constant field
/// draws a flat line, a counter draws a staircase — both are data, both plot.
fn collect_metric_keys(rows: &[TrialRow]) -> Vec<String> {
    let mut keys = BTreeMap::new();
    for row in rows {
        for (key, value) in &row.fields {
            if !key.starts_with("metric.") || key.as_str() == "metric" {
                continue;
            }
            if value.parse::<f64>().is_ok() {
                keys.entry(key.clone()).or_insert(());
            }
        }
    }
    keys.into_keys().collect()
}

fn metric_series_for_key(rows: &[TrialRow], key: &str) -> Vec<ChartPoint> {
    let mut series = Vec::with_capacity(rows.len());
    for (idx, row) in rows.iter().enumerate() {
        let value = row.fields.get(key).and_then(|v| v.parse::<f64>().ok());
        let Some(value) = value else {
            continue;
        };
        series.push(ChartPoint {
            x: (idx + 1) as f64,
            y: value,
        });
    }
    series
}

#[derive(Debug, Clone)]
enum ParamDomain {
    Numeric { min: f64, max: f64 },
    Categorical { categories: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AxisKey {
    name: String,
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

fn trial_detail_lines(
    trial: &TrialRow,
    epochs: &[TrialRow],
    all: &[TrialRow],
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(format!("trial_id: {}", trial.trial_id)));
    lines.push(Line::from(format!("status: {}", trial.status)));
    // Promotion chain: siblings sharing this trial's config, ordered by
    // rung, so a resumed trial shows where it came from and where it sits.
    if let Some(cfg) = trial.fields.get(FIELD_TRIAL_CONFIG_ID) {
        let mut chain: Vec<&TrialRow> = all
            .iter()
            .filter(|t| t.fields.get(FIELD_TRIAL_CONFIG_ID) == Some(cfg))
            .collect();
        chain.sort_by_key(|t| {
            t.fields
                .get(FIELD_TRIAL_RUNG)
                .and_then(|r| r.parse::<i64>().ok())
                .unwrap_or(i64::MAX)
        });
        if chain.len() > 1 {
            let parts: Vec<String> = chain
                .iter()
                .map(|t| {
                    let r = t
                        .fields
                        .get(FIELD_TRIAL_RUNG)
                        .map(String::as_str)
                        .unwrap_or("?");
                    let here = if t.trial_id == trial.trial_id {
                        " ← selected"
                    } else {
                        ""
                    };
                    format!("t{}(r{}, {}){}", t.trial_id, r, t.status, here)
                })
                .collect();
            lines.push(Line::from(format!(
                "chain(cfg {cfg}): {}",
                parts.join(" → ")
            )));
        }
    }
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

#[cfg(test)]
mod view_tests {
    use super::*;

    /// Render a window component into an in-memory buffer and return its text.
    fn render_content<C: Component<TermWmAction>>(
        window: &mut C,
        width: u16,
        height: u16,
    ) -> String {
        let area = WmRect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let buffer =
            ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, width, height));
        let mut backend =
            RatatuiBackend::new_simple(buffer, ratatui::layout::Rect::new(0, 0, width, height));
        let ctx = ComponentContext::new(true).with_screen_area(area);
        let mut registry = HitboxRegistry::new();
        window.render(&mut backend, area, &ctx, &mut registry);
        backend
            .buffer
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn window_roots_delegate_child_height() {
        // Every child reports 0 (stretch); the window roots must delegate that
        // contract to the child rather than hardcoding their own scalar.
        assert_eq!(mk_trials_window().desired_height(30), 0);
        assert_eq!(mk_charts_window().desired_height(30), 0);
        assert_eq!(mk_text_window().desired_height(30), 0);
        assert_eq!(mk_toggle_window("Params").desired_height(30), 0);
    }

    #[test]
    fn view_windows_render_injected_children() {
        let mut trials = mk_trials_window();
        trials
            .sv
            .content
            .borrow_mut()
            .update_items(vec!["trial 7".to_string()]);
        let out = render_content(&mut trials, 30, 8);
        assert!(out.contains("trial 7"), "list item rendered: {out:?}");

        let mut charts = mk_charts_window();
        let out = render_content(&mut charts, 40, 15);
        assert!(
            out.contains("No trials loaded."),
            "charts placeholder: {out:?}"
        );

        let mut toggle = mk_toggle_window("Hyperparameters");
        toggle.list.set_items(vec![ToggleItem {
            id: "lr".into(),
            label: "lr".into(),
            checked: true,
        }]);
        let out = render_content(&mut toggle, 30, 8);
        assert!(out.contains("lr"), "toggle item rendered: {out:?}");
    }

    #[test]
    fn view_window_forwards_selection_metadata() {
        // The copy pipeline reads selection state off the focused window root;
        // the window bridge must forward it to the scroll view content.
        let text = mk_text_window();
        text.sv.content.borrow_mut().set_selection_enabled(true);
        let root = text.selection_status();
        let child = text.sv.selection_status();
        assert_eq!(root.active, child.active);
        assert_eq!(root.dragging, child.dragging);
        assert_eq!(text.selection_text(), text.sv.selection_text());
    }

    fn axis_test_row(pairs: &[(&str, &str)]) -> TrialRow {
        TrialRow {
            trial_id: 0,
            status: "running".to_string(),
            elapsed_ms: 0,
            error: None,
            fields: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn series_uses_arrival_position_as_x() {
        // x is always 1-based arrival position: it cannot collapse, flip, or
        // depend on field names. Counter resets just draw over earlier x.
        let rows = vec![
            axis_test_row(&[("metric.step", "3"), ("metric.loss", "0.9")]),
            axis_test_row(&[("metric.step", "1"), ("metric.loss", "0.8")]),
            axis_test_row(&[("metric.step", "2"), ("metric.loss", "0.7")]),
        ];
        assert_eq!(
            metric_series_for_key(&rows, "metric.loss"),
            vec![
                ChartPoint { x: 1.0, y: 0.9 },
                ChartPoint { x: 2.0, y: 0.8 },
                ChartPoint { x: 3.0, y: 0.7 },
            ]
        );
    }

    #[test]
    fn series_skips_rows_missing_the_key() {
        let rows = vec![
            axis_test_row(&[("metric.loss", "0.9")]),
            axis_test_row(&[("metric.other", "1.0")]),
            axis_test_row(&[("metric.loss", "0.7")]),
        ];
        // Position counts rows, not points: the curve keeps true spacing.
        assert_eq!(
            metric_series_for_key(&rows, "metric.loss"),
            vec![ChartPoint { x: 1.0, y: 0.9 }, ChartPoint { x: 3.0, y: 0.7 },]
        );
    }

    #[test]
    fn chart_keys_include_every_numeric_metric_field() {
        // No filtering: constant lr and counters chart alongside loss.
        // What you see is everything the trial emitted, nothing curated.
        let rows = vec![
            axis_test_row(&[
                ("metric.loss", "0.9"),
                ("metric.lr", "0.00005"),
                ("metric.step", "1"),
                ("hp.lr", "0.00005"),
                ("note", "hello"),
            ]),
            axis_test_row(&[
                ("metric.loss", "0.8"),
                ("metric.lr", "0.00005"),
                ("metric.step", "2"),
                ("hp.lr", "0.00005"),
                ("note", "world"),
            ]),
        ];
        assert_eq!(
            collect_metric_keys(&rows),
            vec![
                "metric.loss".to_string(),
                "metric.lr".to_string(),
                "metric.step".to_string()
            ]
        );
    }

    #[test]
    fn step_hash_matches_db_twin_modulo_trial_time() {
        // The writer stamps `trial.time` into the DB row AFTER the TCP push,
        // so a live row and its persisted twin differ by exactly that key.
        // The reconciler must recognize them as the same step.
        let mut pending = BTreeMap::new();
        pending.insert("metric.loss".to_string(), "0.5".to_string());
        pending.insert("metric.step".to_string(), "7".to_string());
        let mut twin = pending.clone();
        twin.insert(
            FIELD_TRIAL_TIME.to_string(),
            "2026-01-01T00:00:00Z".to_string(),
        );
        assert_eq!(step_row_hash(&pending), step_row_hash(&twin));
    }

    #[test]
    fn step_hash_distinguishes_different_steps() {
        // Identical consecutive steps must NOT collapse: distinct payloads
        // hash distinctly, so reconciliation only ever drops a pending row
        // with a true persisted twin — persisted history is never deduped.
        let mut a = BTreeMap::new();
        a.insert("metric.loss".to_string(), "0.5".to_string());
        let mut b = a.clone();
        b.insert("metric.loss".to_string(), "0.4".to_string());
        assert_ne!(step_row_hash(&a), step_row_hash(&b));
    }

    fn pending_row(trial_id: i64, loss: &str) -> TrialRow {
        TrialRow {
            trial_id,
            status: "running".to_string(),
            elapsed_ms: 0,
            error: None,
            fields: BTreeMap::from([("metric.loss".to_string(), loss.to_string())]),
        }
    }

    #[test]
    fn reconcile_keeps_identical_pending_steps_until_each_twin_arrives() {
        // Two identical pending steps, no twins persisted: both survive.
        let mut pending = BTreeMap::from([(1, vec![pending_row(1, "0.5"), pending_row(1, "0.5")])]);
        let mut counts: BTreeMap<i64, BTreeMap<u64, usize>> = BTreeMap::new();
        reconcile_pending(&mut pending, &mut counts);
        assert_eq!(pending.get(&1).map(Vec::len), Some(2));

        // One twin persisted: exactly one pending copy is evicted, the other
        // (whose twin is still in flight) survives. A `HashSet` would drop
        // both here — the multiset collapse bug.
        let hash = step_row_hash(&pending_row(1, "0.5").fields);
        counts.entry(1).or_default().insert(hash, 1);
        reconcile_pending(&mut pending, &mut counts);
        assert_eq!(pending.get(&1).map(Vec::len), Some(1));

        // Second twin persisted: the last copy is evicted.
        counts.entry(1).or_default().insert(hash, 1);
        reconcile_pending(&mut pending, &mut counts);
        assert!(!pending.contains_key(&1));
    }

    #[test]
    fn zoomed_single_point_series_keeps_non_degenerate_x_bounds() {
        // A 1-point series expands to width 1.0; zooming must clamp against
        // the expanded bounds, never collapse back to zero width.
        let one = vec![ChartPoint { x: 1.0, y: 5.0 }];
        let (min_x, max_x) = chart_x_bounds(&one, 0.5);
        assert!(max_x > min_x, "zoomed 1-point bounds [{min_x}, {max_x}]");
        let (min_x, max_x) = chart_x_bounds(&one, 1.0);
        assert_eq!((min_x, max_x), (1.0, 2.0));
        // Empty and multi-point series behave as before.
        assert_eq!(chart_x_bounds(&[], 0.5), (0.0, 1.0));
        let many = vec![
            ChartPoint { x: 1.0, y: 1.0 },
            ChartPoint { x: 2.0, y: 2.0 },
            ChartPoint { x: 3.0, y: 3.0 },
            ChartPoint { x: 4.0, y: 4.0 },
        ];
        assert_eq!(chart_x_bounds(&many, 1.0), (1.0, 4.0));
        let (min_x, max_x) = chart_x_bounds(&many, 0.5);
        assert!(max_x > min_x && min_x >= 1.0 && max_x <= 4.0);
    }

    #[test]
    fn decimate_preserves_extrema_within_point_budget() {
        // 1000-point curve with spikes at known positions.
        let mut series = Vec::new();
        for i in 0..1000 {
            let y = if i == 123 {
                999.0
            } else if i == 876 {
                -999.0
            } else {
                (i as f64 / 100.0).sin()
            };
            series.push(ChartPoint { x: i as f64, y });
        }
        let out = decimate_min_max(&series, 64);
        assert!(out.len() <= 64, "budget respected: {}", out.len());
        // Extrema survive decimation.
        assert!(out.iter().any(|point| point.y == 999.0));
        assert!(out.iter().any(|point| point.y == -999.0));
        // x order preserved so the line never folds.
        assert!(out.windows(2).all(|w| w[0].x <= w[1].x));
        // Small windows pass through untouched.
        assert_eq!(decimate_min_max(&series[..10], 64), series[..10]);
    }
}
