# Plan: Migrate argtuner TUI to the new term-wm API (delegation-first)

## Goal
Make `src/tui/mod.rs` compile against the current term-wm HEAD (`54552e49`), following the
**exact pattern of term-wm's example app** `term-wm/examples/dual_image.rs` +
`term-wm/src/main.rs`. **Do not reimplement anything term-wm provides** — use `TermWmApp`,
let term-wm do window mgmt / layout / rendering / keyboard+mouse routing / exit-confirm.

## Verified API facts (read from source, NOT assumptions)
- `ScrollViewComponent<C>`: `set_keyboard_mode(ScrollKeyMode)` (scroll_view.rs:249), field `content: RefCell<C>` (scroll_view.rs:231), `scroll_handle()` (scroll_view.rs:257). `ScrollKeyMode::{None,PaginationOnly,Full}` (scroll_view.rs:218).
- `TermWmApp::<C>::embedded(app_ctx)` exists (term_wm_app.rs:419; forwards to `embedded_custom`, :160). `.open_window(&mut self, component: AppRootComponent<C>) -> WindowKey` (:332), `.wm()` (:337), `.set_window_title(key, title)` (:354), `.render_app(backend)` (:387), `.quit_requested()` (:326).
- `WindowManager<C,L,O>`: `create_window(component)` / `open_window(component)` (mod.rs:412/433), `component_for_key_mut(key) -> Option<&mut C>` (:2282), `transition_window(key, WindowState)` (:492), `mark_layout_dirty()` (:902), `focused_window()`/`set_focus(key)` (focus.rs), `region(key) -> Rect` (layout.rs:44), `set_window_title` (layout.rs:562). `LayoutNode::split(dir, children)` (no weights arg), `LayoutNode::leaf(id)`, `TilingLayout::new(root)`.
- `Component<TermWmAction>`: required `render(&mut self, backend: &mut dyn RenderBackend, area: LayoutRect, ctx: &ComponentContext, registry: &mut HitboxRegistry)`. Widgets render via `widget.render(area, &mut ratatui_buffer)` after `helpers::downcast_ratatui(backend)` → `.buffer`.
- Events: `term_wm::events::{Event, KeyCode, KeyModifiers{shift,control,alt}, MouseButton, MouseEventKind::Press(..)/Scroll*, KeyKind}` (no crossterm). `ListComponent`/`ToggleListComponent` already handle their own keys (on_key → MenuUp/Down etc.) and mouse.
- **The runner does NOT call `layout_for_windows`/`enumerate_windows` anymore** — the WM auto-tiles mapped windows (`register_managed_layout`). Custom layout code must be dropped (term-wm owns layout now).
- `term-wm` facade does NOT re-export `ConsoleRenderTarget`/`ConsoleEventSource`/`LayerComponent`/`OverlayComponent`. They live in `term-wm-console` / `term-wm-ui-facade` crates → direct deps required.

## Files to modify
- `Cargo.toml` — add 3 path deps (`term-wm-core`, `term-wm-console`, `term-wm-ui-facade`).
- `src/tui/mod.rs` — rewrite term-wm integration.

---

## Step 1 — Cargo.toml
Add to `[workspace.dependencies]` and `[dependencies]`:
```toml
term-wm-core = { path = "../term-wm/crates/term-wm-core" }
term-wm-console = { path = "../term-wm/crates/term-wm-console" }
term-wm-ui-facade = { path = "../term-wm/crates/term-wm-ui-facade" }
```
`term-wm-core` is required to reach the `impl_component_delegate!` macro
(`term_wm_core::impl_component_delegate`) — the facade's `pub use term_wm_core::*`
re-exports items but NOT `#[macro_export]` macros.

---

## Step 2 — `src/tui/mod.rs`: use the TermWmApp pattern

### 2a. Imports
```rust
use term_wm::components::AppRootComponent;
use term_wm::events::{Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use term_wm::helpers::{downcast_ratatui, layout_rect_to_clipped_rect};
use term_wm::io::{EventSource, RenderTarget};
use term_wm::layout::rect_contains;
use term_wm::prelude::{Component, ComponentContext, EventResult, TermWmAction};
use term_wm::runner::{WindowManagerHost, run_with_defaults};
use term_wm::term_wm_app::TermWmApp;
use term_wm::window::{WindowKey, WindowManager, WindowState};
use term_wm::{AppContext, ListComponent, Rect, ScrollKeyMode, ScrollViewComponent, ToggleItem, ToggleListComponent};
use term_wm_console::console_event_source::ConsoleEventSource;
use term_wm_console::console_render_target::ConsoleRenderTarget;
use term_wm_console::RenderBackend;
use term_wm_core::hitbox_registry::HitboxRegistry;
use term_wm_ui_facade::{LayerComponent, OverlayComponent};
```
Note: `Rect` (the component `area` type) is `term_wm::Rect` = core `Rect` =
`term_wm_layout_engine::LayoutRect` (core/lib.rs:36) — NOT `ratatui::layout::Rect`.
`RenderBackend` comes from `term_wm_console` (re-export of `term_wm_render::RenderBackend`).
Remove all `crossterm::event::*`, `UiFrame`, `WindowProvider`, `run_window_app`,
`WindowManagerExt`, `OverlayId` imports.

### 2b. One component enum (WM is generic over one C) — EXHAUSTIVE delegation
```rust
use term_wm_core::impl_component_delegate;

enum AppComponent {
    Trials(ScrollViewComponent<ListComponent>),
    Charts(ScrollViewComponent<ChartsView>),
    Details(ScrollViewComponent<DetailsView>),
    Params(ToggleListComponent),
    Metrics(ToggleListComponent),
}
impl_component_delegate!(AppComponent { Trials, Charts, Details, Params, Metrics });
```
The macro (macros.rs:97, non-generic arm) generates a full `Component<TermWmAction>` impl
delegating **every** method (`render`, `handle_events`, `update`, `on_key`,
`on_mouse_press/release/drag/scroll/move`, `init`, `on_mount`, `hitbox_id`, `destroy`,
`clear_selection`, `selection_status`, `selection_text`, `desired_height`,
`take_pending_title`, `take_alternate_screen_transition`, `take_teardown_parts`,
`set_selection_enabled`, `paste`) to the inner variant. Do NOT hand-write partial
delegation — that would silently break hit-testing and clipboard/selection propagation.
All variants (`ScrollViewComponent<ListComponent>` etc.) already implement
`Component<TermWmAction>`, so the macro compiles.

### 2c. Custom components implement the new trait (render-only + chart keys)
`ChartsView` / `DetailsView` become `Component<TermWmAction>`. Use `term_wm::Rect`
(= `LayoutRect`, the trait's `area` type) consistently:
```rust
impl Component<TermWmAction> for ChartsView {
    fn render(&mut self, backend: &mut dyn RenderBackend, area: term_wm::Rect,
              ctx: &ComponentContext, _reg: &mut HitboxRegistry) {
        let area = layout_rect_to_clipped_rect(area);  // LayoutRect -> ratatui Rect
        let backend = downcast_ratatui(backend);
        render_charts_content(backend, self, area, ctx);   // now writes to backend.buffer
    }
    // chart-specific keys/mouse (zoom, pan, view toggle) live HERE via on_key/on_mouse_press,
    // since ListComponent/ToggleListComponent already handle their own input.
}
```
All `render_*` helpers change `frame: &mut UiFrame` → `&mut RatatuiBackend` (or `RenderBackend`)
and replace `frame.render_widget(w, a)` with `w.render(a, &mut backend.buffer)`
(`use ratatui::widgets::Widget;`). `ctx.viewport_handle()` → `ctx.scroll_handle()`; `ctx.viewport()` unchanged.

### 2d. AppState = thin host wrapping TermWmApp (pattern from term-wm/src/main.rs)
```rust
struct AppState {
    inner: TermWmApp<AppComponent>,
    db_path, poll, last_refresh,
    trials, epoch_rows, step_rows, step_subscriber, last_error,
    chart_zoom, chart_view, chart_selected, metrics_len, chart_mode, params_x_offset,
    trials_key, charts_key, details_key, params_key, metrics_key,
}
```
Accessors (data injection only):
```rust
fn trials_sv(&mut self) -> Option<&mut ScrollViewComponent<ListComponent>> {
    match self.inner.wm().component_for_key_mut(self.trials_key) {
        Some(AppRootComponent::Custom(AppComponent::Trials(sv))) => Some(sv),
        _ => None,
    }
}
// charts_sv, details_sv, params_list, metrics_list analogously
```

### 2e. Single `WindowManagerHost` impl — delegate, don't reimplement
```rust
impl WindowManagerHost<AppRootComponent<AppComponent>, LayerComponent, OverlayComponent>
    for AppState
{
    fn wm(&mut self) -> &mut WindowManager<AppRootComponent<AppComponent>, LayerComponent, OverlayComponent> {
        self.inner.wm()
    }
    // Delegate app-level events to TermWmApp (it records last_key for the system
    // panel and drives overlay/focus state). Return whatever it returns.
    fn handle_app_event(&mut self, event: &Event) -> bool {
        self.inner.handle_app_event(event)
    }
    fn render(&mut self, backend: &mut dyn RenderBackend) {
        // 1) argtuner-only data work: time-gated DB poll + push data into components.
        //    Runs BEFORE render_app and borrows are scoped per-statement (clone data,
        //    borrow_mut() in a short-lived temporary, drop before calling render_app).
        //    This is the per-frame app hook (term-wm's runner fires it on the frame
        //    pacer even when idle via request_redraw — runner.rs:342/696).
        if self.last_refresh.elapsed() >= self.poll { refresh_trials(self); self.last_refresh = Instant::now(); }
        self.push_data_to_components();   // set_items / assign chart data via accessors
        // 2) delegate all window/layout/chrome rendering to term-wm.
        //    `render_app` is an INHERENT method on TermWmApp<C> (term_wm_app.rs:387),
        //    not a trait method — direct call is correct.
        self.inner.render_app(backend);
    }
    fn open_exit_confirm(&mut self) { self.inner.open_exit_confirm(); }
    fn quit_requested(&self) -> bool { self.inner.quit_requested() }
}
```
Drop: `windows()`, `enumerate_windows`, `window_component`, custom `layout_for_windows`,
custom `open_exit_confirm` overlay plumbing, `WindowProvider`. term-wm handles those.

### 2f. `run()` — mirror the example app
```rust
pub fn run(db_path: PathBuf, poll_ms: u64) -> io::Result<()> {
    let mut inner = TermWmApp::<AppComponent>::embedded(
        AppContext::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")));
    let trials_key  = inner.open_window(AppRootComponent::Custom(AppComponent::Trials(  mk_trials_sv())));
    let charts_key  = inner.open_window(AppRootComponent::Custom(AppComponent::Charts(  mk_charts_sv())));
    let details_key = inner.open_window(AppRootComponent::Custom(AppComponent::Details( mk_details_sv())));
    let params_key  = inner.open_window(AppRootComponent::Custom(AppComponent::Params(  ToggleListComponent::new("Hyperparameters"))));
    let metrics_key = inner.open_window(AppRootComponent::Custom(AppComponent::Metrics( ToggleListComponent::new("Metrics"))));
    for (k, t) in [(trials_key,"Trials"), (charts_key,"Charts"), (details_key,"Trial Details"),
                   (params_key,"Hyperparameters"), (metrics_key,"Metrics")] {
        inner.set_window_title(k, t);
    }
    let mut app = AppState { inner, /* data defaults */ };
    let mut output = ConsoleRenderTarget::new()?;
    let mut input = ConsoleEventSource::new();
    output.enter()?;
    let result = run_with_defaults(&mut output, &mut input, &mut app);
    output.exit()?;
    result
}
```
`TermWmApp::open_window` takes `AppRootComponent<C>` (term_wm_app.rs:332), so every window
is wrapped in `AppRootComponent::Custom(...)` — exactly like the `dual_image.rs` example.
Scroll views wrap their content with `set_keyboard_mode(ScrollKeyMode::None)` where the
inner component owns keys (list up/down), mirroring the old `set_keyboard_enabled(false)`.

### 2g. What argtuner KEEPS doing (term-wm doesn't do these)
- `refresh_trials` / sqlite loading / `StepSubscriber` polling — argtuner data layer, unchanged, but invoked from `render()` instead of `enumerate_windows`.
- `push_data_to_components`: clone trials/epoch_rows → `charts_sv().content.borrow_mut()` fields; `set_items` on trials/params/metrics lists; compute titles → `wm().set_window_title(...)`.
- `ChartsView`/`DetailsView` chart rendering + their own zoom/pan/toggle keys + mouse clicks.
- Mode toggle (`Metrics` vs `HyperParams`): show/hide params/metrics windows via `self.inner.wm().transition_window(key, WindowState::Mapped/Unmapped)` + `mark_layout_dirty()`. This is *calling* term-wm, not reimplementing it.

### 2h. What argtuner DROPS (term-wm now owns)
- `WindowManager::new_embedded`, `WindowProvider`, `run_window_app`, `UiFrame`,
  `WindowManagerExt`, `OverlayId`, custom tiling `layout_for_windows`, custom
  keyboard/mouse dispatch for lists/scrolling/focus, manual scroll-view `viewport_handle`.

---

## Verification
1. `cargo build` — clean compile (success criterion).
2. `cargo clippy --all-targets` — no new warnings.
3. Smoke test: `cargo run -- watch --project <project_dir>` → panes render, list keys move
   selection, wheel scrolls, `q`/binding opens exit-confirm overlay (term-wm built-in),
   `Enter` confirms / `Esc` cancels, mode toggle maps/unmaps params+metrics panes.
4. `grep` confirm no remaining references to removed symbols (`WindowProvider`, `UiFrame`,
   `run_window_app`, `WindowManagerExt`, `OverlayId`, `set_keyboard_enabled`).

---

## Review disposition

### Round 1 (code-review-graph "FAIL") — API claims verified false
- **"Hallucinated" `ScrollKeyMode`, `embedded_custom`, `component_for_key_mut`, `region`,
  `transition_window`, `mark_layout_dirty`, `content: RefCell`** — REJECTED. All exist in
  source at HEAD `54552e49` (local checkout matches cited zip commit `504062f7`).
- **`TermWmApp::open_window` takes `AppRootComponent<C>`** — CONFIRMED; plan wraps in
  `AppRootComponent::Custom(...)`.
- **"Unnecessary deps"** — REJECTED; facade does not re-export console/facade types.
- **"Forward `on_key`/`on_mouse_*` individually"** — PARTIALLY; replaced by the exhaustive
  `impl_component_delegate!` macro (cleaner and complete).
- **Title bug (`charts_key` → "Metrics")** — ACCEPTED; fixed to "Charts".
- **DB polling in `render()`** — kept, time-gated, pre-`render_app`; the runner fires
  `render()` on the frame pacer even when idle (runner.rs:342/696).
- **`RefCell` borrow panic** — addressed: all `borrow_mut()` calls are short-lived
  temporaries before `render_app`.

### Round 2 (code-review-graph "AMEND") — dispositions
1. **`open_window` must take `AppRootComponent<C>`** — ACCEPTED (§2f now wraps every window
   in `AppRootComponent::Custom(...)`).
2. **`handle_app_event` must not bypass `TermWmApp`** — ACCEPTED (§2e delegates
   `handle_app_event` to `self.inner.handle_app_event`, preserving `last_key` recording).
3. **"`render_app` is not an inherent method"** — REJECTED. `TermWmApp<C>::render_app` is an
   inherent `pub fn` (term_wm_app.rs:387), so `self.inner.render_app(backend)` is valid.
4. **"`term_wm::Rect` is `ratatui::layout::Rect`; `LayoutRect` won't compile"** — PARTIALLY.
   `term_wm::Rect` IS `LayoutRect` (core/lib.rs:36), the trait's `area` type; there is no
   `ratatui::Rect` mismatch. Still, the plan now uses `term_wm::Rect` consistently and
   converts to ratatui `Rect` via `layout_rect_to_clipped_rect` at the component boundary.
5. **Incomplete manual delegation breaks hitbox/clipboard propagation** — ACCEPTED (§2b now
   uses the exhaustive `impl_component_delegate!` macro, adding `term-wm-core` as a dep).

---

## AMENDMENT — Replace hardcoded keys with the term-wm keybinding registry

Status: applied on top of the migration above; `src/tui/mod.rs` currently compiles but still
hardcodes raw `KeyCode` matches. This amendment fixes that.

### Problem
- `ChartsView::on_key` matches `KeyCode::Char('f')`, `'+'`, `'-'`, `'0'`, `Left`/`Right`,
  `Up`/`Down`, `PageUp`/`PageDown`, `j`/`k` directly (src/tui/mod.rs:151-201).
- `AppState::handle_app_event` matches `KeyCode::Char('q')` and `'h'` directly (src/tui/mod.rs:531-551).

This bypasses `WmConfig.keybindings`, so keys are not rebindable and don't appear in
bottom-panel hints. Per review: adopt the framework's keybinding subsystem, don't parse raw
keys in components.

### Key facts (verified in source)
- **Custom keybindings ARE addable**: `KeyBindings` is public — `new()` + `add(action, combo)`
  (keybindings.rs:126-134), injected via `AppBuilder::keybindings()` (config.rs:73) → stored in
  `WmConfig.keybindings` (wm_config.rs:104). The command palette and menu already build custom
  `KeyBindings` this way.
- **`TermWmAction` is a closed enum** (actions.rs:37-134): no zoom/pan/view-toggle/mode-toggle
  variants; `Callback(fn())` is a plain fn pointer (cannot capture state). So chart keys must map
  to **existing** `TermWmAction` variants.
- **Framework contract** (runner.rs): a component's `on_key` returns
  `EventResult::Action(TermWmAction::X)` (runner.rs:240-255); the fallthrough arm of
  `dispatch_action` (runner.rs:151-156) delivers the action back to the focused window's
  component `update()`.
- **`ScrollViewComponent::update`** (scroll_view.rs:597-620) consumes only
  `ScrollView`/`ScrollToTop`/`ScrollToBottom`; **every other action forwards to inner content
  `update()`** — so chart actions reach `ChartsView::update` through the wrapper.
- Existing components follow the pattern to mirror: `ListComponent::on_key` (list.rs:105-125)
  matches via `KeyBindings` and returns `EventResult::Action(...)`; its `update()`
  (list.rs:127-140) mutates selection. `ScrollViewComponent::on_key` uses
  `ctx.config().keybindings` (scroll_view.rs:379). `ctx.config().keybindings` is the
  config-respecting accessor (component_context.rs:289).

### Approach
Mirror `ListComponent`/`ToggleListComponent`: resolve keys through a `KeyBindings` registry in
`on_key`, return `EventResult::Action(...)`, and mutate state in `update()`. Register
argtuner's chart/mode keys in a custom `KeyBindings` injected via `AppBuilder::keybindings()`.

#### 1. Build `argtuner_keybindings()` (new fn in src/tui/mod.rs)
Start from `KeyBindings::new()` and `.add(action, combo)` for each mapping. Bind on
`KeyCombo::new(KeyCode, KeyModifiers)`; add shifted variants where two codes were accepted
(e.g. `Char('+')` and `Char('=')`).

#### 2. Chart key → action mapping (reuse existing `TermWmAction` variants)
| Current key | New action | Handled in `ChartsView::update()` |
|---|---|---|
| `Up` / `k` (charts, Focused) | `MenuUp` | move chart selection -1 |
| `Down` / `j` (charts, Focused) | `MenuDown` | move chart selection +1 |
| `PageUp` (Focused) | `ScrollPageUp` | move chart selection -5 |
| `PageDown` (Focused) | `ScrollPageDown` | move chart selection +5 |
| `Home` / `End` | `ScrollHome` / `ScrollEnd` | select first / last metric |
| `Space` / `Enter` / `f` (Metrics) | `ToggleSelection` | toggle Summary ↔ Focused |
| `Left` / `Right` (HyperParams) | `ConfirmLeft` / `ConfirmRight` | pan params ±1 |

> **Constraint**: map zoom/reset/pan ONLY onto variants the wrapper forwards (its `_` arm).
> `ScrollView`/`ScrollToTop`/`ScrollToBottom` are consumed by the wrapper, so they must NOT be
> used for chart zoom/reset. If no forwarded variant adequately expresses zoom-in/out/reset, the
> cleanest fix is to add app-agnostic variants to term-wm's `TermWmAction` (user owns that repo)
> rather than abusing existing scroll actions — decide at implementation time.

#### 3. `ChartsView::on_key` (src/tui/mod.rs:151-201)
Replace the raw `match key.code` with `let kb = &ctx.config().keybindings;` +
`if kb.matches(Action, key) { return EventResult::Action(Action) }` chains (mirror
scroll_view.rs:379 / list.rs:105). Keep mode/view guards (`chart_mode`/`chart_view`) as `if`
conditions.

#### 4. Add `ChartsView::update(...)`
`match action { MenuUp|MenuDown|ScrollPageUp|ScrollPageDown|ScrollHome|ScrollEnd|ToggleSelection|ConfirmLeft|ConfirmRight => ... }`
moving `toggle_chart_view` / `move_chart_selection` / `pan_params` / zoom logic out of `on_key`
into `update()` (signature `(&mut self, action, ctx, actions)`).

#### 5. `AppState::handle_app_event` (src/tui/mod.rs:531-551)
Remove raw `KeyCode::Char('q')`/`'h'` matches. Resolve via
`self.inner.wm().keybindings()`:
- bind `q` → `TermWmAction::Quit` in `argtuner_keybindings()`; check `kb.matches(Quit, key)` →
  `open_exit_confirm()`.
- bind `h` → an existing generic variant in `argtuner_keybindings()`; check
  `kb.matches(that, key)` → toggle `chart_mode` + `apply_chart_mode()`.

#### 6. Inject keybindings into the WM
`TermWmApp::new_custom` does not accept keybindings. Build the `WindowManager` via
`AppBuilder::<LayerComponent>::bare().app_ctx(...).keybindings(argtuner_keybindings()).build()`
and wrap with `TermWmApp::from_wm(wm, tx)`, preserving the chrome wiring `new_custom` does
(top/bottom panel, FAB, supported_menu_actions, notification component). (Alternatively add a
keybindings-accepting `TermWmApp` constructor in term-wm.)

### Files to modify
- `src/tui/mod.rs` — `argtuner_keybindings()`, `run()` injection, `ChartsView::on_key`,
  `ChartsView::update` (new), `AppState::handle_app_event`.
- Possibly `term-wm/crates/term-wm-core/src/actions.rs` — ONLY if zoom/reset/pan cannot be
  mapped to a forwarded existing variant (add app-agnostic actions then).

### Verification
1. `cargo build` — clean.
2. `cargo clippy --bin argtuner` — no new warnings.
3. `cargo test --workspace` — all pass.
4. Manual: chart keys (f/+/-/0/arrows/j/k/space) still work exactly as before, now appear in
   bottom-panel keybinding hints, and honor `WmConfig.keybindings`; `q` opens exit confirm,
   `h` toggles Metrics/HyperParams mode.
5. `grep -n "KeyCode::" src/tui/mod.rs` → only keybinding registration (`KeyCombo::new`)
   remains; no raw dispatch in `on_key`/`handle_app_event`.

---

## AMENDMENT 2 — Add generic spatial actions + action hatch to term-wm (Round-3 review)

Status: replaces Amendment 1's action-mapping approach. The user owns the `term-wm` repo, so
we extend the framework properly rather than hijacking unrelated `TermWmAction` variants.

### Accepted review directives
1. **Do NOT hijack existing `TermWmAction` variants** (`ConfirmLeft`, `MenuUp`, etc.) — the
   hint renderer (`label()` in actions.rs) would show misleading tooltips and rebinds would
   collide. REJECTED the Amendment-1 mapping table for domain ops.
2. **Do NOT reimplement chrome init** with `AppBuilder::bare()` inside argtuner. REJECTED the
   Amendment-1 §6 `from_wm` re-wiring approach.
3. **Use generic spatial semantics + an extensibility hatch.**

### term-wm changes (`/Volumes/2TB Storage Vault/term-wm`)
All in `crates/term-wm-core/src/actions.rs` (user's repo):

1. **Add generic spatial actions** (app-agnostic — valid for any canvas/image/plot component):
   `ZoomIn`, `ZoomOut`, `ResetZoom`, `PanLeft`, `PanRight`, `PanUp`, `PanDown`,
   `CycleViewMode`.
2. **Add an extensibility hatch**: `Custom(u16)` — lets any app map keys to its own
   application-state triggers without touching the framework enum again.
3. **Update exhaustive matches** so the crate compiles:
   - `TermWmAction::category()` (actions.rs:192) — new arm for the spatial actions (add to an
     existing `Category`, e.g. `Navigation` or `Scrolling`; `Custom(_)` → `System`/`Navigation`).
   - `TermWmAction::label()` (actions.rs:288-348, exhaustive match) — new arms with accurate
     hint text: `"Zoom in"`, `"Zoom out"`, `"Reset zoom"`, `"Pan left"`, `"Pan right"`,
     `"Cycle view"`, `"Custom action"`.
   - `TermWmAction::category`/`label` are the only exhaustive matches on the enum; the runner's
     `dispatch_action` (runner.rs:151-156) has a wildcard `action =>` fallthrough that delivers
     any unhandled action to the focused component's `update()`, so no runner change is needed.
4. **Keybinding injection without chrome reimplementation** — pick ONE (prefer the cleaner):
   - Add `TermWmApp::new_with_config(app_ctx: AppContext, config: WmConfig) -> Self` to the
     facade (`src/term_wm_app.rs`) that calls the existing `AppBuilder::bare().app_ctx(...)`
     + `.config(config)` + the same top/bottom-panel/FAB/notification wiring as `new_custom`
     (config.rs already exposes `.config(WmConfig)` and `.keybindings(KeyBindings)`), then
     `TermWmApp::from_wm(wm, tx)`. This keeps ALL chrome init inside term-wm.
   - OR add `WindowManager::keybindings_mut(&mut self) -> &mut KeyBindings` (mod.rs:1036 area)
     so argtuner can post-init add bindings via `inner.wm().keybindings_mut().add(...)`.
   Prefer `new_with_config` (single injection point, hints render from `WmConfig.keybindings`
   via layout.rs:289-301).

### argtuner changes (`src/tui/mod.rs`)
1. Build a `KeyBindings` (start from `KeyBindings::new()` or a base) mapping chart/mode keys to
   the **new generic actions**:
   - `+`/`=` → `ZoomIn`; `-` → `ZoomOut`; `0` → `ResetZoom`
   - `Left`/`Right` (HyperParams) → `PanLeft`/`PanRight` (or `PanUp`/`PanDown` as appropriate)
   - `f`/`Space`/`Enter` (Metrics) → `CycleViewMode`
   - `Up`/`k`, `Down`/`j`, `PageUp`, `PageDown`, `Home`, `End` → chart selection (reuse existing
     `MenuUp`/`MenuDown`/`ScrollPageUp`/`ScrollPageDown`/`ScrollHome`/`ScrollEnd`, or the new
     spatial actions if better suited)
   - app-level `q` → `TermWmAction::Quit` (already handled by runner → `open_exit_confirm`,
     runner.rs:82); `h` → `CycleViewMode` (or a `Custom(..)` code).
2. `run()`: construct via the new `TermWmApp::<AppComponent>::new_with_config(ctx, config)`
   where `config.keybindings = argtuner_keybindings()` (config passed in is
   `WmConfig::standalone()` with `.keybindings(...)` set). No `AppBuilder` usage in argtuner.
3. `ChartsView::on_key` (mod.rs:151-201): resolve via `let kb = &ctx.config().keybindings;`
   returning `EventResult::Action(...)` on `kb.matches(action, key)` — mirroring
   list.rs:105 / scroll_view.rs:379. Keep mode/view guards as `if` conditions.
4. Add `ChartsView::update(...)`: match on the new actions (`ZoomIn`, `ZoomOut`, `ResetZoom`,
   `PanLeft`, `PanRight`, `CycleViewMode`, plus selection actions) and mutate chart state —
   move `toggle_chart_view`/`move_chart_selection`/`pan_params`/zoom logic here.
5. `AppState::handle_app_event` (mod.rs:531-551): resolve `q`/`h` via
   `self.inner.wm().keybindings()` against the registered actions (`Quit`, `CycleViewMode`),
   not raw `KeyCode`.

### Files to modify
- `term-wm/crates/term-wm-core/src/actions.rs` — new generic actions + `Custom(u16)` hatch +
  `category()`/`label()` arms.
- `term-wm/src/term_wm_app.rs` — `new_with_config` constructor (or `term-wm-core` WindowManager
  `keybindings_mut`).
- `src/tui/mod.rs` — `argtuner_keybindings()`, `run()` via `new_with_config`,
  `ChartsView::on_key`/`update`, `handle_app_event`.

### Verification
1. `cargo build` (argtuner) — clean.
2. `cargo build` (term-wm) — clean with new enum arms.
3. `cargo clippy --bin argtuner` — no new warnings.
4. `cargo test --workspace` — all pass (both repos).
5. Manual: chart keys (f/+/-/0/arrows/j/k/space) work; bottom-panel hints show the new generic
   action labels ("Zoom in", "Pan left", "Cycle view"); `q` opens exit confirm; `h` toggles
   Metrics/HyperParams mode.
6. `grep -n "KeyCode::" src/tui/mod.rs` → only `KeyCombo::new` registration remains.

---

## AMENDMENT 3 — PTY-starvation & focus-scoping review disposition (Round-4 review)

### Claim under review
"Registering `+`/`-`/`f`/arrows globally to actions starves focused PTY terminals (vim/pico)."

### Disposition: NOT APPLICABLE to argtuner; focus-scoping is already the framework default
1. **argtuner's TUI has NO PTY windows** (verified): all five windows are custom components —
   `ListComponent`, `ChartsView`, `DetailsView`, `ToggleListComponent`. `TerminalComponent` /
   `Pty` do not appear anywhere in `src/tui/mod.rs` (grep count 0). The only `portable_pty`
   usage in argtuner is `src/command/subprocess/runner.rs` — the tuner's background subprocess
   runner, entirely separate from the TUI. There is no PTY input stream to starve.
2. **term-wm keybinding matching is component-scoped, not global** (verified in runner.rs):
   the runner routes every key to the *focused window's* component only
   (runner.rs:240-241, `comp.handle_events(&adjusted_evt, &ctx)`). Component actions
   (ZoomIn, Pan, MenuUp, etc.) are matched **inside** the component's own `on_key` via
   `ctx.config().keybindings` (list.rs:105, scroll_view.rs:379). No global scan of
   `WmConfig.keybindings` exists for arbitrary component actions — the runner only
   keybinding-checks a few global actions (`OpenCommandPalette` runner.rs:492,
   `Quit` runner.rs:653, palette-layer keys).
3. **Direct Mode protects PTY windows by construction** (runner.rs:617-622): a window in
   direct mode forwards keys straight to its PTY before any component handling. So even if
   argtuner later adds terminal windows, component-scoped bindings cannot starve them.
4. **`Custom(u16)` extensibility hatch retained** (from Amendment 2) — satisfies the
   "closed action taxonomy" concern without polluting the framework with domain actions.

### Net effect on the plan
No change to Amendment 2's approach is required for the PTY-starvation concern. The
`Custom(u16)` hatch + generic spatial actions (`ZoomIn`, `ZoomOut`, `ResetZoom`, `PanLeft`,
`PanRight`, `PanUp`, `PanDown`, `CycleViewMode`) from Amendment 2 stand. argtuner registers
these actions in its injected `WmConfig.keybindings`; matching happens only in the focused
`ChartsView::on_key`, so non-focused windows and any future PTY windows are unaffected.

---

## AMENDMENT 4 — Fix palette-not-opening + bottom-panel-not-showing keybindings

### Symptoms (from runtime)
1. The command palette (Ctrl+A) does not open.
2. The bottom panel does not register/show the new chart keybindings.

### Root cause 1 — palette wiped by empty `KeyBindings`
`argtuner_keybindings()` in `src/tui/mod.rs` starts from `KeyBindings::new()` (empty) and
`run()` replaces `WmConfig.keybindings` wholesale. This deletes the default
`OpenCommandPalette` binding (Ctrl+A, keybindings.rs:88). The runner only opens the palette
via `app.wm().keybindings().matches(TermWmAction::OpenCommandPalette, key)` (runner.rs:492),
so with that binding gone, Ctrl+A does nothing.

**Fix (argtuner, `src/tui/mod.rs`):** build on the defaults instead of an empty registry:
```rust
fn argtuner_keybindings() -> KeyBindings {
    let mut kb = KeyBindings::default(); // keep OpenCommandPalette, FocusNext/Prev, scroll, etc.
    kb.add(TermWmAction::Quit, KeyCombo::new(KeyCode::Char('q'), KeyModifiers::NONE));
    kb.add(TermWmAction::Custom(1), KeyCombo::new(KeyCode::Char('h'), KeyModifiers::NONE));
    kb.add(TermWmAction::CycleViewMode, KeyCombo::new(KeyCode::Char('f'), KeyModifiers::NONE));
    // ... keep the remaining Zoom/Pan/selection adds from Amendment 2 ...
    kb
}
```

### Root cause 2 — new actions invisible to the hint renderer
The bottom panel is populated by `register_managed_layout` →
`keybindings().bottom_hints_for_layer(MAX_BOTTOM_HINTS, layer)` (layout.rs:286-301), where
`layer` is `ActionLayer::Global` when the palette is closed. `bottom_hints_filtered`
(keybindings.rs:213-230) drops an action unless BOTH:
- `action.bottom_hint_priority()` returns `Some(..)` (actions.rs:303-319 — the new actions
  hit `_ => None`), AND
- `action.layer() == layer` (actions.rs:210-218 — the new actions hit
  `_ => ActionLayer::CommandPalette`, not `Global`).

So the new spatial actions + `Custom` never appear in the closed-palette bottom panel.

**Fix (term-wm, `crates/term-wm-core/src/actions.rs`):**
- Add `bottom_hint_priority()` arms for the new actions, e.g.:
  `ZoomIn => Some(48)`, `ZoomOut => Some(47)`, `ResetZoom => Some(46)`,
  `PanLeft => Some(45)`, `PanRight => Some(44)`, `PanUp => Some(43)`,
  `PanDown => Some(42)`, `CycleViewMode => Some(41)`, `Custom(_) => Some(40)`
  (pick any free values; keep below `Quit`=90 / `NewWindow`=50 as appropriate).
- Add `layer()` arms so the new actions are `ActionLayer::Global` (visible in the
  closed-palette bottom panel):
  `ZoomIn | ZoomOut | ResetZoom | PanLeft | PanRight | PanUp | PanDown | CycleViewMode |
  Custom(_) => ActionLayer::Global`.
- Optionally add `TermWmAction::Quit` to the `Global` arm too, so the `q`→Quit binding also
  shows in the panel (Quit already has priority 90).

### Files to modify
- `src/tui/mod.rs` — `argtuner_keybindings()`: start from `KeyBindings::default()`.
- `term-wm/crates/term-wm-core/src/actions.rs` — `bottom_hint_priority()` + `layer()` arms for
  the new actions (and optionally `Quit`).

### Verification
1. `cargo build` + `cargo clippy --bin argtuner` (argtuner); `cargo build`/`test` (term-wm).
2. Manual: Ctrl+A opens the command palette; bottom panel shows the new hints (Zoom in, Pan
   left, Cycle view, etc.) with their bound keys; `q` opens exit confirm; `h` toggles mode;
   chart keys (f/+/-/0/arrows) still work in the focused charts pane.
3. `grep -n "bottom_hint_priority\|fn layer" term-wm/crates/term-wm-core/src/actions.rs` shows
   the new arms.

---

## AMENDMENT 5 — Extract Command Palette scroll-into-view into a shared handler (Trials list)

### Symptom
Keying up/down in a list wrapped by a `ScrollViewComponent` (the Trials list; also affects
any `ListComponent`/`ToggleListComponent`) does not auto-scroll the viewport as the selection
moves past the visible area. Mouse-wheel scrolling works, keyboard scroll-follow does not.

### Root cause (verified in source)
- `ScrollViewComponent::on_key` only auto-scrolls via keybindings when
  `keyboard_mode != ScrollKeyMode::None`; the Trials wrapper uses `None` so scroll keys
  forward to the inner component (intended — `ListComponent` handles Up/Down itself).
- The inner component is responsible for scroll-follow. `ListComponent::render`
  (list.rs:52-57) calls `handle.ensure_vertical_visible(self.selected + 1, self.selected + 2)`.
  That primitive uses the ScrollHandle's `height` (= the full window area set by
  `ScrollViewComponent`), but `ListComponent` draws a `Block::borders(ALL)` and renders items
  inside `block.inner(area)` — **2 rows shorter than the window area**. So the last ~2 items
  can move under the bottom border before scroll fires, and the mapping is off.
- The **Command Palette solved this exact bug** by hotwiring a guarded scroll-into-view in
  `CommandPaletteComponent::render` (command_palette.rs:569-591, commit `f029eecf`): it tracks
  `last_display_sel`, sets `content_height` itself, and only adjusts `offset_y` when the
  selection index *changes* (`display_sel != last_display_sel`), using a real `list_height`
  (its list area height), never fighting manual mouse scroll.

### Goal
Extract the palette's proven logic into a **shared handler** usable by the Trials list
(`ListComponent`) — and the parallel `ToggleListComponent` — instead of each component
re-implementing its own off-by-border scroll math.

### Design
Add a guarded selection-follow method to `ScrollHandle` (term-wm-core), then call it from
`CommandPaletteComponent`, `ListComponent`, and `ToggleListComponent`.

#### 1. `term-wm/crates/term-wm-core/src/component_context.rs` — new `ScrollHandle` method
```rust
/// Keep the content row `selected_row` within the viewport, but ONLY when the
/// selection index or the viewport size changed since the last call. This
/// preserves manual (mouse) scrolls (CommandPalette's proven behavior) while
/// still re-following after a terminal resize (viewport_rows shrank and the
/// selection fell out of view).
///
/// Contract: `selected_row` is the row of the selected item in the SAME
/// content coordinate space as `offset_y` / `content_height`. Callers map
/// their own convention (e.g. ListComponent passes `selected + 1` because its
/// content height includes its self-drawn top border; CommandPalette passes
/// the 0-indexed display row). `viewport_rows` is the number of item rows
/// actually visible in that same space.
///
/// IMPORTANT (ordering): the upper clamp MUST use the authoritative physical
/// `inner.max_offset_y()` (= content_height - inner.height), NOT a fabricated
/// `content_height - viewport_rows`. For bordered components (ListComponent)
/// viewport_rows is 2 less than inner.height, so a fabricated max overshoots
/// the physical max by the border height; ScrollViewComponent then force-clamps
/// `pending_offset_y` back on the next frame, and because `last_selected` is
/// cached the selection never re-follows — permanently clipping the last rows.
/// Therefore pre-render callers (CommandPaletteComponent, which runs this
/// before its child `list_scroll.render()` updates `inner.height`) MUST sync
/// `scroll.height` to the current physical viewport first so `max_offset_y()`
/// evaluates correctly.
pub fn ensure_selection_visible(
    &self,
    selected_row: usize,
    viewport_rows: usize,
    last_selected: &mut usize,
    last_viewport_rows: &mut usize,
) {
    if selected_row == *last_selected && viewport_rows == *last_viewport_rows {
        return;
    }
    *last_selected = selected_row;
    *last_viewport_rows = viewport_rows;
    let mut inner = self.scroll.borrow_mut();
    let current = inner.offset_y;
    let new_offset = if selected_row < current {
        selected_row
    } else if selected_row >= current.saturating_add(viewport_rows) {
        selected_row.saturating_sub(viewport_rows).saturating_add(1)
    } else {
        return; // already visible
    };
    let max = inner.max_offset_y(); // authoritative physical clamp
    let clamped = new_offset.min(max);
    inner.offset_y = clamped;
    inner.pending_offset_y = Some(clamped);
}
```
Keep the existing `ensure_vertical_visible` (still used elsewhere; backwards compatible).

#### 2. `term-wm/crates/term-wm-ui-components/src/command_palette.rs`
Replace the hotwired block (lines 569-591) with the shared call. The palette runs the
visibility check BEFORE its child `list_scroll.render()`, so it must sync both `content_height`
and `height` (the physical viewport) so `max_offset_y()` is accurate:
```rust
let total = self.display_nodes.len();
let list_height = bounds.height.saturating_sub(1) as usize;

let handle = self.list_scroll.scroll_handle();
{
    let mut scroll = handle.scroll.borrow_mut();
    scroll.content_height = total;
    scroll.height = list_height; // sync physical height; matches list_area.height used by list_scroll.render
}
handle.ensure_selection_visible(
    display_sel, list_height, &mut self.last_display_sel, &mut self.last_viewport_rows,
);
```
Add field `last_viewport_rows: usize` (init 0 in `new()`); keep `last_display_sel` (now the
`last_selected` arg).

#### 3. `term-wm/crates/term-wm-ui-components/src/list.rs` + `toggle_list.rs`
- Add fields `last_selected: usize` and `last_viewport_rows: usize` to both structs, and
  initialize both to `0` in the struct literal of `new<T: Into<String>>(title)` (list.rs:148-153
  / toggle_list.rs:144-149). Also reset both to `0` in `set_items` when the list is replaced
  (so a fresh list re-follows from the top).
- Replace the `ensure_vertical_visible(selected + 1, selected + 2)` call with:
```rust
// `selected + 1`: item index -> content row, because content_height includes
// the self-drawn top border (see set_content_size(total + 2) and the
// `skip_n = offset_y - 1` render mapping). Do NOT drop the +1 here.
let viewport_rows = inner.height as usize; // block.inner rows actually visible
handle.ensure_selection_visible(
    self.selected + 1, viewport_rows, &mut self.last_selected, &mut self.last_viewport_rows,
);
```

### Coordinate-contract note (why SEV-1 "remove +1" is rejected)
`ListComponent` owns a border-drawing coordinate space: `set_content_size(w, total+2)` and
renders items at virtual rows `offset_y-1 .. offset_y+inner.height-1` (comment list.rs:54-64).
So item `selected` lives at content row `selected + 1`, and `offset_y` is the same
border-inclusive space. Passing `selected + 1` is correct here (the existing code already
does this); dropping it would clip item 0 behind the top border. `CommandPalette` instead
uses a 0-indexed display row (`display_sel`) against `content_height = total`. Both are
correct — each caller maps its own content-space convention, which the method's contract
documents.

### Tests (term-wm-ui-components, mirroring the palette's scroll tests)
Add to `list.rs` (and mirror in `toggle_list.rs`):
- `auto_scroll_starts_at_offset_zero`
- `auto_scroll_advances_when_selection_moves_past_viewport`
- `auto_scroll_goes_back_when_selection_moves_up`
- `auto_scroll_does_not_override_manual_scroll` (manual `offset_y` preserved when selection
  and viewport unchanged)
- `auto_scroll_reengages_after_manual_scroll_when_selection_changes`
- `auto_scroll_reruns_on_viewport_shrink` (resize: same selection, smaller `viewport_rows`
  -> re-follows, per SEV-2)

### Files to modify
- `term-wm/crates/term-wm-core/src/component_context.rs` — new `ensure_selection_visible`.
- `term-wm/crates/term-wm-ui-components/src/command_palette.rs` — use the shared handler
  + `last_viewport_rows` field.
- `term-wm/crates/term-wm-ui-components/src/list.rs` — `last_selected`/`last_viewport_rows`
  fields + shared call (keep `selected + 1`).
- `term-wm/crates/term-wm-ui-components/src/toggle_list.rs` — same.
- `src/tui/mod.rs` — no change required (argtuner's Trials list uses `ListComponent`; it
  inherits the fix). Confirm the wrapper stays `ScrollKeyMode::None` so `ListComponent` owns
  Up/Down selection while the wrapper still draws the scrollbar.

### Verification
1. `cargo build` + `cargo test` (term-wm) — new scroll-follow tests pass (incl. resize).
2. `cargo build` + `cargo clippy --bin argtuner` + `cargo test --workspace` (argtuner).
3. Manual: in the Trials list, key Down past the last visible row → viewport scrolls so the
   selected trial stays visible; key Up back to top → scrolls back; then wheel-scroll away
   (selection unchanged) → stays put; change selection → auto-scroll re-engages; shrink the
   terminal with selection unchanged → viewport re-follows.
4. Same behavior check for the params/metrics `ToggleListComponent` panes.

### Review disposition (Amendment 5)
- **SEV-1 (off-by-one `selected + 1`)** — REJECTED as a false premise for `ListComponent`:
  verified `ListComponent` uses a border-inclusive content space (`set_content_size(w,
  total+2)`, renders at `offset_y-1`), so item `selected` is genuinely at content row
  `selected + 1`; dropping the `+1` would clip item 0 behind the top border. The shared
  method's documented contract ("row in the caller's own content space") makes each caller's
  mapping explicit (`ListComponent` → `selected + 1`; `CommandPalette` → 0-indexed
  `display_sel`).
- **SEV-2 (resize occlusion via strict `last_selected` guard)** — ACCEPTED: added
  `last_viewport_rows` to the guard + all three components, plus a
  `auto_scroll_reruns_on_viewport_shrink` test.
- **SEV-3 (stale-height clamp in `max_offset_y()` for outer components)** — ACCEPTED
  (this review): `CommandPaletteComponent` calls the method before its child
  `list_scroll.render()` updates `inner.height`, so on a shrink the stale (larger) `height`
  makes `max_offset_y()` too small and the cached `last_viewport_rows` then locks the bad
  offset. Fixed two ways: (1) the method clamps against
  `content_height - viewport_rows` (authoritative caller-supplied viewport) instead of
  `inner.max_offset_y()`; (2) `CommandPalette` explicitly writes `scroll.height =
  list_height` before the call.
- **SEV-4 (encapsulation violation from manual `scroll.height` sync)** — REJECTED in the
  prior amendment, but this review correctly supersedes it: once the clamp is restored to the
  authoritative physical `max_offset_y()` (which reads `inner.height`), the pre-render palette
  caller MUST sync `scroll.height` to the current viewport (otherwise `max_offset_y()` is stale
  on shrink and the cached `last_selected` locks a bad offset). It does NOT fight
  `ScrollViewComponent::render`: the palette's `list_height = bounds.height-1` is exactly the
  `list_area.height` it passes to `list_scroll.render()`, so both agree. Restored the sync.
- **SEV-5 (fabricated `content_height - viewport_rows` max)** — ACCEPTED (this review): a
  fabricated max overshoots the physical `max_offset_y()` by the border height (2 rows for
  bordered lists), ScrollViewComponent force-clamps `pending_offset_y` back on the next frame,
  and the cached `last_selected` then prevents re-following — permanently clipping the last
  rows. Reverted the method to clamp with `inner.max_offset_y()` (authoritative) and documented
  the ordering requirement (pre-render callers sync `scroll.height`).

---

## AMENDMENT 6 — Add per-window "closable" option to term-wm

### Goal
Let term-wm apps mark a window as **not closable**: it can never be removed, the ✕ button is
hidden, and the palette "Close window" entry is disabled. All close paths are gated, including
PTY-child-exit.

### Scope decision (confirmed with user)
- **Per-window flag** on `Window`, default `true` (closable), mirroring the existing
  `close_policy` field/accessor pattern.
- **Blocks everything**: ✕ click, palette Close, `TermWmAction::CloseWindow`, and the
  PTY-exit path in the runner.

### Files / changes (all in `/Volumes/2TB Storage Vault/term-wm`)

#### 1. `crates/term-wm-core/src/window/entry.rs`
Add field + accessors next to `close_policy` (entry.rs:111, 247-253):
```rust
// field (default true):
closable: bool,
// accessors:
pub fn closable(&self) -> bool { self.closable }
pub fn set_closable(&mut self, closable: bool) { self.closable = closable; }
```
Init `closable: true` in `Window::new` (entry.rs:138 area).

#### 2. `crates/term-wm-core/src/window/window_manager/mod.rs`
Add WM-level setter mirroring `set_close_policy` (mod.rs:450-454):
```rust
pub fn set_closable(&mut self, key: WindowKey, closable: bool) {
    if let Some(w) = self.windows.get_mut(key) {
        w.set_closable(closable);
    }
}
pub fn is_closable(&self, key: WindowKey) -> bool {
    self.window(key).map_or(false, |w| w.closable())
}
```

#### 3. `crates/term-wm-core/src/window/window_manager/chrome.rs` — authoritative gate
At the top of `close_window` (chrome.rs:97), separate the existence check from the permission
check so an unknown/destroyed key isn't mislabeled as "not closable":
```rust
let w = match self.window(key) {
    Some(w) => w,
    None => {
        tracing::warn!(window_key = ?key, "close_window invoked on unknown or destroyed window");
        return;
    }
};
if !w.closable() {
    tracing::debug!(window_key = ?key, "ignoring close: window is not closable");
    return;
}
```
This blocks ALL paths (✕, palette, `CloseWindow`, runner PTY-exit) while keeping telemetry
accurate for ghost keys.

#### 4. `crates/term-wm-core/src/window/window_manager/mod.rs` — `window_management_buttons`
In `window_management_buttons` (mod.rs:2634-2662), only push the `CloseWindow` button when
the focused window is closable:
```rust
if self.window(focused).is_some_and(|w| w.closable()) {
    btns.push(WmButton { action: TermWmAction::CloseWindow(focused), label: "Close Window", symbol: "X" });
}
```
Omission removes both the visible ✕ and its hitbox (renderer `_ => continue`, draw_plan_renderer.rs:152).

#### 5. `crates/term-wm-core/src/window/window_manager/command_palette.rs` — palette entry
In `wm_menu_items` (command_palette.rs:185-190), disable the Close entry for non-closable
windows (keep it visible but greyed, or omit it — prefer `disabled: true`):
```rust
let closable = self.window(focused).is_some_and(|w| w.closable());
items.push(MenuDisplayItem::Item(MenuItem {
    label: format!("Close {}", title).into(),
    icon: Some("X"),
    action: crate::actions::TermWmAction::CloseWindow(focused),
    disabled: !closable,
}));
```
(`MenuItem.disabled` already exists, components.rs:524-529.)

#### 6. Tests (term-wm-core, window_manager mod.rs `#[cfg(test)]`)
- `close_window_ignored_for_non_closable_window` — `set_closable(k, false)`; call
  `close_window(k)`; assert `has_window(k)` and state stays mapped.
- `set_closable_toggles` — default true; `set_closable(k,false)` → `is_closable(k) == false`.
- `window_management_buttons_hides_close_for_non_closable` — focused non-closable window →
  buttons vector has no `CloseWindow` action.
- `wm_menu_items_disables_close_for_non_closable` — assert the Close entry has
  `disabled: true`.

### argtuner (optional, follow-up)
`src/tui/mod.rs` `run()`: mark the five panes non-closable with
`inner.wm().set_closable(trials_key, false)` etc. after opening them, so the Watch TUI's
windows cannot be closed by ✕ / palette / anything. (Ask user if they want this now or later.)

### Verification
1. `cargo build` + `cargo test` (term-wm) — new tests pass; all existing pass.
2. `cargo clippy` (term-wm).
3. `cargo build`/`test` (argtuner) — still green.
4. Manual (if argtuner wired): focused non-closable pane shows no ✕; palette "Close" is
   greyed; pressing it does nothing; window persists.

### Review disposition (Amendment 6)
- **SEV-2 (conflating `None` window with "not closable")** — ACCEPTED (this review): the
  negated `is_some_and` gate would log "not closable" for unknown/destroyed keys, polluting
  telemetry and hiding race conditions. The gate now resolves the `Option<&Window>` with a
  distinct `warn` for `None` before the `closable()` permission check (separate `debug` log).

---

## AMENDMENT 7 — Fix Trials scrollbars + remove redundant inner list border

### Symptoms (argtuner Watch TUI, Trials window)
1. **Vertical scrollbar snaps back** to the highlighted row after a manual scroll.
2. **Horizontal scrollbar does nothing** (thumb moves, content doesn't).
3. **Redundant inner border** — `ListComponent` draws `Block::default().borders(ALL).title("Trials (focus)")` on top of the window chrome's own header/border.

### Root causes (verified)
- **(1) Snap-back**: `ListComponent::set_items` (list.rs:167-172) resets `last_selected` and
  `last_viewport_rows` to 0 on every call. argtuner's `refresh_trials` (500ms cadence,
  src/tui/mod.rs:606-609) calls `set_items` then `move_selection(delta)` to restore the
  highlight; `move_selection` (list.rs:190-201) never updates the guard fields. So on the next
  render `ensure_selection_visible`'s guard fails (`last_selected` is stale `0`) and the
  "selected above viewport" branch forces `offset_y` back to the highlighted row
  (component_context.rs:231-242). Manual scroll is undone every ~500ms.
- **(2) Horizontal**: `ScrollViewComponent` fully wires horizontal scroll (draw + drag,
  scroll_view.rs:326-346, 550-565) and `ListComponent` reports `content_width = max_width + 2`
  (list.rs:53), so the scrollbar appears — but `ListComponent::render` only ever reads
  `vp.offset_y` (list.rs:68-90); it never applies `offset_x`. The thumb moves, content doesn't.
- **(3) Border**: `ListComponent` (list.rs:31-43) and `ToggleListComponent` (toggle_list.rs:39-48)
  unconditionally paint a `Borders::ALL` + title block inside the content area, layered on top of
  the window chrome's header/border (composite_window → render_window, draw_plan_renderer.rs).

### Changes (all in `/Volumes/2TB Storage Vault/term-wm`)

#### A. `crates/term-wm-ui-components/src/list.rs` + `toggle_list.rs` — remove inner border
Remove the `Block::default().borders(ALL).title(...)` frame (and the `(focus)` suffix). Render
the items directly into the content area:
- `let inner = area;` (no `block.inner(area)`), drop `block.render(...)`.
- Content size: `set_content_size(max_width, total_height)` (no `+ 2`).
- Selection row is now 0-indexed (no top border): `ensure_selection_visible(selected, inner.height, ...)`.
- Render slice: `skip_n = vp.offset_y` (no `saturating_sub(1)`).
- Mouse index (list.rs:102-106): `index = vp.offset_y + local_y` (drop both `-1`s).
- `(focus)` styling moves to the chrome header (already focused-colored); no in-content title.

#### B. `list.rs` + `toggle_list.rs` — make horizontal scroll work (column-aware)
Apply `vp.offset_x` in **visual columns**, not raw chars (wide chars / emoji occupy 2 columns).
Add `unicode-width = { workspace = true }` to `crates/term-wm-ui-components/Cargo.toml` (workspace
dep already exists at term-wm Cargo.toml:113). Slice each item by accumulating visual width,
padding boundary-crossing wide chars with spaces so column alignment is preserved:
```rust
use unicode_width::UnicodeWidthChar;

fn slice_by_columns(s: &str, start_col: usize, width: usize) -> String {
    let mut out = String::new();
    let mut current_col = 0usize;
    let end_col = start_col.saturating_add(width);

    for c in s.chars() {
        let cw = c.width().unwrap_or(0);

        if cw == 0 {
            if current_col > start_col && current_col <= end_col {
                out.push(c); // combining mark / zero-width: keep if inside
            }
            continue;
        }

        let next_col = current_col + cw;

        if next_col <= start_col {
            // Entirely before the viewport — skip.
        } else if current_col < start_col {
            // Crosses the left boundary — pad the visible remainder with spaces.
            let visible = next_col - start_col;
            let take = visible.min(width);
            out.push_str(&" ".repeat(take));
        } else if current_col < end_col {
            if next_col <= end_col {
                // Fully inside the viewport.
                out.push(c);
            } else {
                // Crosses the right boundary — pad the visible fraction.
                let visible = end_col - current_col;
                out.push_str(&" ".repeat(visible));
            }
        } else {
            // Entirely after the viewport.
            break;
        }
        current_col = next_col;
    }
    out
}
```
Then render `ListItem::new(slice_by_columns(s, vp.offset_x, inner.width as usize))`.
- ListComponent items are plain un-styled `String`s (no ANSI), so no escape-sequence risk; the
  boundary-aware column slice handles CJK/emoji alignment correctly.
- Same for `ToggleListComponent`, applied to the assembled `format!("[x] {label}")` string.

#### C. `list.rs` + `toggle_list.rs` — add `update_items` (preserve manual scroll), keep `set_items` contract
Keep `set_items`'s existing reset behavior (fresh dataset → selection/guard reset to 0) — do NOT
weaken it. Add a new method for in-place data refresh that keeps selection + manual scroll:
```rust
/// Replace the items in place WITHOUT resetting the selection or the
/// scroll-follow guard. For periodic live-refresh of an existing list
/// (e.g. argtuner's 500ms trials poll) so a manual scroll is preserved.
pub fn update_items(&mut self, items: Vec<String>) {
    self.items = items;
    if self.selected >= self.items.len() {
        self.selected = self.items.len().saturating_sub(1);
    }
    // last_selected / last_viewport_rows intentionally untouched: the list
    // identity is unchanged, so the guard holds and manual scroll persists.
}
```
Mirror for `ToggleListComponent`. argtuner `refresh_trials` (src/tui/mod.rs:543-554) switches
from `set_items` → `update_items` (keeping its existing `move_selection` restore).
Rationale: `set_items` stays the generic "replace the whole dataset" primitive (fresh list →
snap to top); `update_items` is the "refresh same dataset" primitive (preserve position). This
keeps the component contract intact and localizes the fix to argtuner's polling loop.

#### D. Update tests (term-wm-ui-components)
The Amendment-5 scroll-follow tests assumed the border (`viewport_rows = 8`, `selected + 1`).
Update them to the borderless geometry:
- `viewport_rows` = area height (e.g. 10 for a 10-row area).
- `selected_row` = `list.selected()` (0-indexed).
- `scroll_ctx`/`render_list_with_scroll` helpers unchanged; assertions updated accordingly
  (list.rs + toggle_list.rs scroll-follow tests).
- Add `update_items_preserves_selection_and_scroll`: call `update_items` with new items of the
  same length, assert `selected`, `last_selected`, `last_viewport_rows`, and `offset_y` are
  unchanged; and `set_items_resets_guard_fields` asserts `set_items` still zeroes them.
- Add `horizontal_scroll_slices_columns`: build a list with a wide (2-col CJK) char, set a
  manual `offset_x`, render, and assert the drawn buffer's first visible cells match the expected
  column slice — including **boundary cases**: a 2-col char partially overlapping the left edge
  yields a leading space (not a dropped/left-shifted char), and one overlapping the right edge
  is padded with a trailing space (no overflow).
- Add `slice_by_columns_pads_boundary_wide_chars` unit test for the helper directly: verify a
  wide char straddling `start_col` produces `" "` padding, and one straddling `end_col` produces
  trailing padding, so the output width is exactly `width` columns in both cases.

### Files to modify
- `term-wm/crates/term-wm-ui-components/Cargo.toml` — add `unicode-width = { workspace = true }`.
- `term-wm/crates/term-wm-ui-components/src/list.rs` — border removal, column-aware `offset_x`
  slicing, new `update_items`, mouse index, tests.
- `term-wm/crates/term-wm-ui-components/src/toggle_list.rs` — same (render slice, offset_x,
  update_items).
- argtuner `src/tui/mod.rs` — `refresh_trials` calls `update_items` instead of `set_items`
  (preserves manual scroll); Charts pane keeps its per-chart `Chart` blocks (inside the pane,
  distinct from the window frame).

### Verification
1. `cargo build` + `cargo test` (term-wm) — updated scroll-follow tests + new horizontal test
   pass; all existing pass.
2. `cargo clippy` (term-wm).
3. `cargo build`/`test` (argtuner) — still green.
4. Manual (argtuner Watch): Trials list shows no inner "Trials (focus)" frame (only chrome
   header); dragging the vertical scrollbar stays put across refreshes; dragging the horizontal
   scrollbar scrolls long trial lines (wide CJK chars stay aligned); Up/Down still re-follows
   selection.

### Review disposition (Amendment 7)
- **SEV-1 (char-based slicing corrupts visual width/ANSI)** — ACCEPTED (this review): replaced
  `s.chars().skip(off_x)` with column-aware slicing via `unicode-width`
  (`UnicodeWidthChar::width()` accumulation), added as a workspace dep to term-wm-ui-components.
  ListComponent items are plain un-styled Strings (no ANSI), so no escape-sequence risk, but
  CJK/emoji alignment is now correct.
- **SEV-2 (removing `set_items` reset corrupts the generic contract)** — ACCEPTED (this review):
  `set_items` keeps its original reset semantics (fresh dataset → snap to top). Added a new
  `update_items` primitive that updates items in place WITHOUT touching selection/guard fields;
  argtuner's `refresh_trials` calls `update_items` instead. This preserves manual scroll in the
  polling loop without degrading the framework's data-replacement API.
- **SEV-3 (boundary-crossing wide chars break column alignment)** — ACCEPTED (this review): the
  naive `col >= start_col && col < end_col` test dropped a wide char straddling the left edge
  (shifting subsequent text left) and pushed one straddling the right edge (overflowing the
  viewport). Replaced with a boundary-aware `slice_by_columns` that computes intersections and
  pads partially-occluded wide chars with spaces (exact algorithm above), plus unit tests for
  both boundary cases.

## AMENDMENT 8 — Trial # in window titles, zoom-out hints (config-derived), selectable Trial Details

### Goal
Three UX improvements in the argtuner Watch TUI (all in `/Volumes/2TB Storage Vault/rust-argtuner/src/tui/mod.rs`, no term-wm changes needed):
1. The **metric-curves (Charts)**, **Trials**, and **Trial Details** window titles show the selected trial #.
2. When a metric curve is **zoomed in**, the Charts window shows an inline footer hint with the actual
   zoom-out/reset/list-view keybindings — **derived from the live `KeyBindings` config**, not hardcoded.
3. The **Trial Details** text becomes **selectable + copyable** via term-wm internals: swap the raw
   `Paragraph` renderer for a `TextRendererComponent` inside the existing `ScrollViewComponent`,
   with `set_selection_enabled(true)` so drag-to-select + copy-on-release works automatically.

### 1. Window titles show the selected trial #
The selected trial index is already computed each frame in `push_data_to_components`
(mod.rs:453-500) as `selected` (`AppState::selected_trial_idx`, mod.rs:423). The trial # shown in the
list rows is `trial.trial_id` (see `build_trial_items`, mod.rs:1025). So the titles:

- Charts: `"Metric Curves - Trial {trial_id}"` (summary), `"Metric Curve {cur}/{total} - Trial {trial_id}"`
  (focused), `"Hyperparameter Space"` (hyperparams mode, unchanged).
- Trials: `"Trials - Trial {trial_id}"`.
- Trial Details: `"Trial {trial_id} Details"` (replaces static `"Trial Details"`).
- Fallback when no trials loaded: revert to the base titles (`"Metric Curves"`, `"Trials"`,
  `"Trial Details"`).

Changes in `src/tui/mod.rs`:
- Extend `charts_window_title(...)` (mod.rs:631) to take the selected `trial_id: Option<i64>` and
  append ` - Trial {id}` to the Metrics branches.
- In `push_data_to_components` (mod.rs:453), after computing `selected`, read
  `let trial_id = self.trials.get(selected).map(|t| t.trial_id);`. **All `set_window_title` calls
  MUST live in a standalone block at the very end of `push_data_to_components`, AFTER every
  `if let Some(sv) = self.details_sv() / charts_sv() / ...` block has finished**, because each of
  those borrows `self.inner` mutably (`component_for_key_mut`) and `set_window_title` needs
  `&mut WindowManager`. The existing code already orders it this way (component blocks first at
  mod.rs:464-488, then the title block at mod.rs:490-500) — keep that structure and extend the
  final title block (existing pattern, mod.rs:499):
  ```rust
  // --- standalone title block (no component borrows alive) ---
  let wm = self.inner.wm(); // &mut WindowManager; this is the accessor (no wm_mut exists)
  let trials_title = trial_id.map_or("Trials".into(), |id| format!("Trials - Trial {id}"));
  let details_title = trial_id.map_or("Trial Details".into(), |id| format!("Trial {id} Details"));
  wm.set_window_title(self.trials_key, trials_title);
  wm.set_window_title(self.details_key, details_title);
  wm.set_window_title(self.charts_key, charts_window_title(mode, cv, cs, ml, charts_focused, trial_id));
  ```
  (`set_window_title` is a no-op when unchanged — layout.rs:556-568 — so per-frame is cheap.)
- Keep the static `"Trials"`/`"Trial Details"` initial titles in `run()` (mod.rs:91-102) as the
  fallback base.

### 2. Zoom-out hint, inline in the Charts window, keybindings from config (not hardcoded)
`KeyBindings::combos_for(action) -> Vec<String>` (keybindings.rs:180) returns the display strings for
the currently bound combos of an action. `ChartsView::on_key` already reaches the config via
`ctx.config().keybindings` (mod.rs:170). Render a footer line inside the Charts window when a metric
curve is zoomed in so the user can see how to snap back.

Design:
- New free function `fn chart_keybindings_hint(kb: &KeyBindings) -> String`:
  ```rust
  fn chart_keybindings_hint(kb: &KeyBindings) -> String {
      let zoom_out = kb.combos_for(TermWmAction::ZoomOut).first().cloned().unwrap_or_default();
      let reset    = kb.combos_for(TermWmAction::ResetZoom).first().cloned().unwrap_or_default();
      let list     = kb.combos_for(TermWmAction::CycleViewMode).first().cloned().unwrap_or_default();
      format!("[{}] zoom out    [{}] reset    [{}] list view", zoom_out, reset, list)
  }
  ```
  This is fully config-derived: rebinding `-`/`0`/`f` in `argtuner_keybindings` (mod.rs:653) updates
  the hint automatically. No hardcoded key names.
- In `render_metric_charts` (mod.rs:1062), when `charts.chart_zoom < 1.0` (zoomed in — `chart_zoom`
  is clamped to `[0.1, 1.0]`), reserve the bottom row of `area` for the hint and render the chart in
  the reduced area:
  ```rust
  let hint = if charts.chart_zoom < 1.0 {
      Some(chart_keybindings_hint(&ctx.config().keybindings))
  } else { None };
  let chart_area = match hint {
      Some(_) => Rect { x: area.x, y: area.y, width: area.width, height: area.height.saturating_sub(1) },
      None => area,
  };
  // ... render chart into chart_area (both Summary and Focused branches) ...
  if let Some(h) = hint {
      Paragraph::new(h)
          .style(Style::default().fg(Color::DarkGray))
          .render(Rect { x: area.x, y: area.y + chart_area.height, width: area.width, height: 1 }, &mut backend.buffer);
  }
  ```
- The hint only appears when actually zoomed in; at `chart_zoom == 1.0` (default) the Charts window
  renders exactly as today. The `CycleViewMode` ("list view") and `ResetZoom` entries are exactly the
  two ways to snap out (back to summary / zoom reset).

### 3. Trial Details selectable + copyable via term-wm internals
Replace the custom `DetailsView` raw-`Paragraph` renderer (mod.rs:1263-1290) with
`ScrollViewComponent<TextRendererComponent>` (term-wm already re-exports `TextRendererComponent`
via `src/lib.rs:4`). This gives drag-to-select, selection overlay, and copy-on-drag-release for free
via the runner's `update_selection_snapshot` → `copy_selection_to_clipboard` (runner.rs:816-846,
`clipboard_enabled` default true).

Changes in `src/tui/mod.rs`:
- `AppComponent::Details` variant changes from `ScrollViewComponent<DetailsView>` to
  `ScrollViewComponent<TextRendererComponent>` (mod.rs:338).
- Delete the `DetailsView` struct + its `Component` impl (mod.rs:315-330) and
  `render_details_content` (mod.rs:1263-1290); keep `trial_detail_lines` (mod.rs:1686) as the
  line-builder.
- `mk_details_sv()` (mod.rs:766): build `ScrollViewComponent::new(TextRendererComponent::new())`,
  call `set_wrap(false)` (preserve current one-line-per-field formatting — see wrap decision
  below), `set_selection_enabled(true)`, keep `set_keyboard_mode(ScrollKeyMode::Full)`.
- `details_sv()` helper (mod.rs:385) return type becomes
  `Option<&mut ScrollViewComponent<TextRendererComponent>>`.
- In `push_data_to_components` (mod.rs:483-487), instead of pushing raw data into `DetailsView`,
  build the text once and call `set_text`:
  ```rust
  if let Some(sv) = self.details_sv() {
      let idx = selected.min(self.trials.len().saturating_sub(1));
      let text = match self.trials.get(idx) {
          Some(trial) => {
              let epochs = self.epoch_rows.get(&trial.trial_id).cloned().unwrap_or_default();
              Text::from(trial_detail_lines(trial, &epochs))
          }
          None => Text::from(vec![Line::from("No trial selected.")]),
      };
      sv.content.borrow_mut().set_text(text);
  }
  ```
  (Set before the trial-change scroll-reset at mod.rs:466-474 so the reset still applies; note the
  reset uses `details_sv().scroll_handle()` which `ScrollViewComponent` still exposes.)
- Add `use ratatui::text::Text;` to the ratatui imports (mod.rs:11).

**Wrap decision:** `TextRendererComponent` defaults to `wrap = true`, which reflows long key=value
lines at the window edge — the user is worried that breaks the current one-field-per-line layout.
Use `set_wrap(false)` so each `trial_detail_lines` row stays on its own visual line (same look as
today's `Paragraph`), with `content_width = max line width` → a horizontal scrollbar appears only if
a field value is wider than the window (which the existing `ScrollViewComponent` handles). Selection
and copy are unaffected by wrap.

### Files to modify
- `/Volumes/2TB Storage Vault/rust-argtuner/src/tui/mod.rs` — titles (§1), `chart_keybindings_hint`
  + zoom-footer render (§2), `TextRendererComponent` swap for Details (§3), `Text` import.

### Verification
1. `cargo build` + `cargo clippy` + `cargo test` (argtuner, 71 lib tests) — green; update any test
   that referenced `DetailsView`/`render_details_content` (none expected — they're app-internal).
2. Manual (argtuner Watch with a live run):
   - Select different trials in the Trials list → Trials/Charts/Details window titles update to
     `Trial {id}`; empty DB → base titles.
   - In a metric curve, press `+`/`=` to zoom in → a dim footer `[-] zoom out    [0] reset    [f]
     list view` appears at the bottom of the Charts window; press `0` → hint disappears (zoom back
     to 1.0); press `f` → returns to the summary/list view.
   - In Trial Details: drag the mouse to select a span of text → highlight appears; release → "Selection
     copied to clipboard" notification; paste elsewhere yields the selected lines (formatting intact,
     no reflow/wrap).

### Open decision (resolved via user Q)
- Hint placement: **inline in the chart window** (not the global bottom hint bar), and key strings
  come from the live `KeyBindings` config via `combos_for`, not hardcoded.
- Title format: `Metric Curves - Trial 7`, `Trials - Trial 7`, `Trial 7 Details`.
- Details wrapping: user concerned wrap breaks the field-per-line layout → `set_wrap(false)`.

### Review disposition (Amendment 8)
- **SEV-1 (borrow/signature for `set_window_title`)** — ACCEPTED: `set_window_title` takes
  `&mut WindowManager`; the accessor is `TermWmApp::wm(&mut self)` (term_wm_app.rs:322, there is
  **no** `wm_mut`). Title updates are grouped in a standalone block at the end of
  `push_data_to_components`, after every `details_sv()`/`charts_sv()` mutable component borrow has
  been dropped, so no E0499. Matches the existing title block ordering (mod.rs:490-500).
- **SEV-2 (KeyCombo serialization)** — REJECTED (verified in source): `KeyBindings::combos_for`
  already returns `Vec<String>` of display strings (keybindings.rs:180, it maps `KeyCombo::display()`
  to String), so `combos_for(action).first().cloned().unwrap_or_default()` yields a `String` that
  formats directly in `[{}]`. No `.to_string()` on `KeyCombo` is involved, and no `KeyCombo` is
  ever formatted.

### Completed (Amendment 8)
- All three changes implemented in `src/tui/mod.rs`: dynamic trial-# titles (Trials/Charts/Details),
  `chart_keybindings_hint` + zoom footer in `render_metric_charts`, Details swapped to
  `ScrollViewComponent<TextRendererComponent>` (wrap off, selection on). `cargo build`/`clippy`
  (only pre-existing talkback.rs warnings)/`test` (71 passed) all green. No term-wm changes needed.

## AMENDMENT 9 — Fix hint discoverability + declutter titles (follow-up to Amendment 8)

### Symptoms (user feedback on Amendment 8)
1. "i don't see the keybindings" — the zoom footer only renders when `chart_zoom < 1.0`, i.e. only
   AFTER the user already zoomed in. Chicken-and-egg: you can't discover `+`/`-`/`0`/`f` because the
   hint isn't shown until you've already used them. (Current code: `render_metric_charts`,
   `let hint = if charts.chart_zoom < 1.0 { Some(...) } else { None }`.)
2. "those app titles are busy" — `push_data_to_components` appends ` - Trial {id}` to the Trials
   window too, and the Charts title reads `Metric Curves - Trial 7`. User wants: Trials title = plain
   `"Trials"` (no trial number); Charts/Details lead with the trial number.

### Decisions (confirmed with user)
- **Hint visibility:** ALWAYS render the config-derived footer inside the Charts window (Metrics
  mode), regardless of zoom state. Keybindings remain active only while the Charts window is focused
  (which the WM already enforces — `ChartsView::on_key` only fires when focused). So the footer is
  always discoverable; when not focused it's inert.
- **Title format (trial-first):**
  - Trials: `"Trials"` (always, no number).
  - Charts: `"Trial {id} - Metric Curves"` / `"Trial {id} - Metric Curve {cur}/{total}"` (focused) /
    `"Trial {id} - Hyperparameter Space"`; fallback to the base names when no trial loaded.
  - Details: `"Trial {id} Details"`; fallback `"Trial Details"`.

### Changes (all in `/Volumes/2TB Storage Vault/rust-argtuner/src/tui/mod.rs`)
1. `render_metric_charts` (mod.rs:1098): render the hint footer **unconditionally** in Metrics mode
   instead of gating on `chart_zoom < 1.0`:
   ```rust
   let hint = Some(chart_keybindings_hint(&ctx.config().keybindings));
   let chart_area = Rect { x: area.x, y: area.y, width: area.width, height: area.height.saturating_sub(1) };
   // ... render charts into chart_area (both branches) ...
   if let Some(h) = hint { Paragraph::new(h).style(...).render(footer row); }
   ```
   (Behavior: footer always present → zoom keys discoverable; only functional when Charts focused,
   which is inherent. Simplify by dropping the `Option` and using `chart_area` directly.)
2. `chart_keybindings_hint` (mod.rs:661): add `ZoomIn` so the always-visible footer teaches the full
   zoom loop (`+`/`=` to zoom in, `-` to zoom out, `0` reset, `f` list view):
   ```rust
   fn chart_keybindings_hint(kb: &KeyBindings) -> String {
       let zoom_in  = kb.combos_for(TermWmAction::ZoomIn).first().cloned().unwrap_or_default();
       let zoom_out = kb.combos_for(TermWmAction::ZoomOut).first().cloned().unwrap_or_default();
       let reset    = kb.combos_for(TermWmAction::ResetZoom).first().cloned().unwrap_or_default();
       let list     = kb.combos_for(TermWmAction::CycleViewMode).first().cloned().unwrap_or_default();
       format!("[{zoom_in}] zoom in    [{zoom_out}] zoom out    [{reset}] reset    [{list}] list view")
   }
   ```
   (`combos_for` returns `Vec<String>` — keybindings.rs:180 — so `.cloned().unwrap_or_default()` on
   the `.first()` `Option<&String>` yields a `String`; no `.to_string()` needed.)
3. `charts_window_title` (mod.rs:630): swap to trial-first format — `format!("Trial {id} - Metric
   Curves")`, `format!("Trial {id} - Metric Curve {cur}/{total}")`, `format!("Trial {id} -
   Hyperparameter Space")`; `None` id → base names.
4. `push_data_to_components` (mod.rs:495): Trials title is a constant `"Trials"` (remove the
   `Trials - Trial {id}` branch); keep Details `Trial {id} Details`; pass `trial_id` to
   `charts_window_title` as before.

### Verification
1. `cargo build` + `cargo clippy` + `cargo test` (argtuner) — green (only pre-existing talkback.rs
   clippy warnings).
2. Manual (argtuner Watch):
   - Open Charts: a dim `[+] zoom in    [-] zoom out    [0] reset    [f] list view` footer is always
     visible at the bottom (config-derived via `combos_for`); pressing `+`/`=` zooms in (footer
     persists), `-` zooms out, `0` resets, `f` toggles summary/focused.
   - Titles: Trials window shows plain `Trials`; Charts shows `Trial 7 - Metric Curves` (focused:
     `Trial 7 - Metric Curve 2/7`); Details shows `Trial 7 Details`; with no trials loaded all fall
     back to base names.

### Review disposition (Amendment 9)
- **SEV-2 (ZoomIn missing from the always-visible hint)** — ACCEPTED: the unconditional footer must
  teach the full zoom loop, so `ZoomIn` is added to `chart_keybindings_hint` (first bound combo via
  `combos_for`), giving `[+] zoom in  [-] zoom out  [0] reset  [f] list view`. This restores the
  original intent of discoverability for the `+`/`=` zoom-in keys. Note: the suggested
  `.map(|c| c.to_string())` is unnecessary — `combos_for` already returns `Vec<String>` of display
  strings (keybindings.rs:180); the existing `.first().cloned().unwrap_or_default()` pattern is kept.

### Completed (Amendment 9)
- `chart_keybindings_hint` now includes `ZoomIn` (full `[+] zoom in  [-] zoom out  [0] reset
  [f] list view`, config-derived).
- `render_metric_charts` always reserves the bottom row for the footer (removed the
  `chart_zoom < 1.0` gate) — keys discoverable at all times, active only when Charts focused.
- `charts_window_title` is trial-first (`Trial {id} - Metric Curves`, `Trial {id} - Metric Curve
  {cur}/{total}`, `Trial {id} - Hyperparameter Space`); Trials window title is the constant
  `"Trials"`; Details stays `Trial {id} Details`. All fall back to base names with no trial loaded.
- `cargo build`/`clippy` (only pre-existing talkback.rs warnings)/`test` (71 passed) all green.

## AMENDMENT 11 — Callback-based app tasks (REPLACES the AppTick/on_app_tick design)

### Why (user feedback)
Amendment 10 used `SystemTask::AppTick` (unit variant) + a single `WindowManagerHost::on_app_tick()`
hook. The user rejected this: a task should carry a **callback**, not a unit payload or an ID.
`on_app_tick` collapses ALL repeating tasks into one hook — multiple repeating tasks can't be
differentiated, and IDs are worse. Fix: make the scheduler's app payload a **closure** so each
scheduled task runs its own callback. `TaskScheduler<T>` is already generic and `schedule_repeating`
requires `T: Clone`; a callback payload `Rc<RefCell<Box<dyn FnMut(&mut A)>>>` is `Clone`, so this
fits the existing scheduler with no new scheduling machinery.

### Design
- New core type `AppTask<A>` (in `crates/term-wm-core/src/task_scheduler.rs`), with a **manually
  implemented `Clone`** so the generic `A` never needs `Clone`:
  ```rust
  /// A callback-carrying app task payload. Clone shares the underlying
  /// callback (Rc), so a repeating task fires the same closure each interval.
  pub struct AppTask<A> {
      callback: Rc<RefCell<Box<dyn FnMut(&mut A)>>>,
  }

  // Manual Clone: derives the inner Rc only, WITHOUT injecting an `A: Clone`
  // bound. `#[derive(Clone)]` would require `A: Clone` (AppState is !Clone).
  impl<A> Clone for AppTask<A> {
      fn clone(&self) -> Self {
          Self { callback: Rc::clone(&self.callback) }
      }
  }

  impl<A> AppTask<A> {
      pub fn new<F: FnMut(&mut A) + 'static>(f: F) -> Self {
          Self { callback: Rc::new(RefCell::new(Box::new(f))) }
      }
      fn run(&self, app: &mut A) {
          (self.callback.borrow_mut())(app);
      }
  }
  ```
  This satisfies `schedule_repeating`'s `T: Clone` (task_scheduler.rs:116) without ever requiring
  `A: Clone` — the closure is what's shared, and `A` is only ever borrowed as `&mut A` at run time.
- The runner owns a **second scheduler** `TaskScheduler<AppTask<A>>` alongside the existing
  `TaskScheduler<SystemTask>`, drains it every loop iteration, and invokes each `AppTask::run(app)`.
  `SystemTask` is untouched (no `AppTick` variant needed).
- Multiple repeating tasks are naturally differentiated: each schedules its own `AppTask::new(closure)`,
  and the runner calls the matching closure. No IDs, no collapsed hook.

### Changes

#### term-wm-core
1. `task_scheduler.rs` — add `AppTask<A>` (Clone via `Rc`); `Rc`/`RefCell` already imported.
2. `actions.rs` — REVERT the `SystemTask::AppTick` variant added in Amendment 10 (remove it).
3. `runner.rs`:
   - `WindowManagerHost` trait: **remove** `fn on_app_tick`. Change `on_app_scheduler_ready`
     signature to hand the app a callback handle:
     ```rust
     fn on_app_scheduler_ready(&mut self, _handle: TaskHandle<AppTask<Self>>) {}
     ```
   - `run_event_loop` (runner.rs:271, already `#[allow(clippy::too_many_arguments)]`): add param
     `app_scheduler: TaskScheduler<AppTask<A>>`. In the loop, alongside the system drain
     (runner.rs:326), add:
     ```rust
     for (_id, task) in app_handle.drain_expired() {
         task.run(app);
     }
     ```
     with `let app_handle = app_scheduler.handle();` next to `system_handle` (runner.rs:289).
   - Sleep clamp (runner.rs:414-421): take the **min** of the system deadline, the app deadline, and
     the frame-pacer deadline:
     ```rust
     let deadline = match (fp_deadline, system_handle.time_until_next(), app_handle.time_until_next()) {
         (Some(fp), s, a) => Some(fp.min(s.unwrap_or(Duration::MAX)).min(a.unwrap_or(Duration::MAX))),
         (None, s, a) => s.map(|d| a.map_or(d, |ad| d.min(ad))).or(a),
     };
     ```
     (A recurring app task keeps `time_until_next()` populated, so the loop stays awake — together
     with the Amendment-10 `ConsoleEventSource::set_max_sleep_duration` cap this fires on time even
     when idle.)
   - pending-work (runner.rs:767-768): OR the app handle in so a scheduled callback keeps the
     profile awake:
     ```rust
     driver.set_pending_work(system_handle.is_keep_awake_active() || app_handle.has_pending());
     ```
   - `run_with_defaults` (runner.rs:797-821): create both schedulers:
     ```rust
     let system_scheduler = TaskScheduler::<SystemTask>::new();
     let system_handle = system_scheduler.handle();
     app.wm().set_system_task_handle(system_handle.clone());
     let app_scheduler = TaskScheduler::<AppTask<A>>::new();
     let app_handle = app_scheduler.handle();
     app.on_app_scheduler_ready(app_handle);
     run_event_loop(..., system_scheduler, app_scheduler, ...);
     ```
4. Update `run_event_loop` call sites to pass `TaskScheduler::<AppTask<_>>::new()`:
   - runner.rs:1982 (test), tests/panic_debug_log.rs:165, 266, 348.

#### argtuner (src/tui/mod.rs)
5. Imports: add `use term_wm_core::task_scheduler::AppTask;` (drop `SystemTask` if unused elsewhere).
6. Replace the two hooks (mod.rs:601-613):
   ```rust
   fn on_app_scheduler_ready(&mut self, handle: TaskHandle<AppTask<Self>>) {
       let _ = handle.schedule_repeating(self.poll, AppTask::new(|app| {
           app.refresh_trials();
           if let Some(h) = global_debug_log() {
               h.push(format!(
                   "poll tick: {} trials, {} epoch rows",
                   app.trials.len(),
                   app.epoch_rows.len()
               ));
           }
       }));
   }
   ```
   Delete `on_app_tick` entirely. `AppState` fields stay as-is (no `last_refresh`; `poll` drives the
   interval). `render()` stays `push_data_to_components()` + `render_app()`.

### Files to modify
- `term-wm/crates/term-wm-core/src/task_scheduler.rs` — `AppTask<A>`.
- `term-wm/crates/term-wm-core/src/actions.rs` — remove `AppTick`.
- `term-wm/crates/term-wm-core/src/runner.rs` — trait hook, second scheduler param, drain arm, sleep
  clamp, pending-work, `run_with_defaults`, 1 test call site.
- `term-wm/tests/panic_debug_log.rs` — 3 `run_event_loop` call sites.
- `/Volumes/2TB Storage Vault/rust-argtuner/src/tui/mod.rs` — callback scheduling, remove `on_app_tick`.

### Verification
1. `cargo build` + `cargo clippy` + `cargo test` (term-wm workspace incl. term-wm-console) — green.
2. `cargo build`/`clippy`/`test` (argtuner, 71 lib tests) — green.
3. Manual (argtuner Watch):
   - Ctrl+A → palette → "≣ Debug Log" opens the Debug Log.
   - Debug Log fills with `poll tick: N trials, M epoch rows` every ~`--poll-ms` (default 500ms) even
     when the terminal is idle (proving the callback fires independent of the render loop).
   - Trials list still live-updates; selection preserved.
   - (Design check) With two different `AppTask` callbacks scheduled, each fires its own closure —
     verified by reading `on_app_scheduler_ready`/`drain_expired`; no shared hook, no IDs.

## AMENDMENT 10 — Debug Log window + replace SQLite render-polling with a recurring core scheduler task

> **SUPERSEDED by AMENDMENT 11 (task-scheduler portion only).** The Debug Log window, tracing,
> `ConsoleEventSource::set_max_sleep_duration` cap, `init_system_windows()` call, and
> `toggle_debug_window` delegation in this amendment REMAIN VALID and are kept. ONLY the
> `SystemTask::AppTick` + `on_app_tick` recurring-task design (§2 / §3 of this amendment) is
> REVERSED and replaced by the callback-based `AppTask<A>` design in Amendment 11. When executing
> Amendment 11, treat §2/§3 below as the "before" state to revert from; the runner/actions files
> must end in the Amendment-11 state (no `AppTick`, no `on_app_tick`, no `last_refresh`).

### Goal
1. Show the **Debug Log** window in argtuner (like term-wm's main.rs): call `init_system_windows()`
   so the hidden Debug Log window + logging exist; reachable **palette-only** (Ctrl+A → "≣ Debug
   Log") and auto-open on errors/panics. User chose palette-only (no new keybinding).
2. Replace the time-gated SQLite polling in `AppState::render` (tui/mod.rs:591-608) with a **proper
   recurring core task** (`TaskScheduler::schedule_repeating`), fired by the runner's event loop —
   NOT piggybacked on the render hook. Keep `--poll-ms` as the interval.
3. Each iteration **logs a line to the Debug Log** so the user can monitor the refresh cadence live.

### Verified architecture facts (adapting the user's blueprint to the real codebase)
- `SystemTask` is defined in **`crates/term-wm-core/src/actions.rs:409`** (not `tasks.rs`), derives
  `Clone` — required by `schedule_repeating`.
- The `WindowManagerHost` trait is in **`crates/term-wm-core/src/runner.rs:24`** (not
  `window_manager/mod.rs`). It already has default methods incl. `toggle_debug_window` (no-op default,
  runner.rs:46) and `render` (runner.rs:61).
- The runner drains expired tasks in `run_event_loop` at **runner.rs:318-337** (exhaustive match over
  the 4 `SystemTask` variants). `run_with_defaults` (runner.rs:786) creates the scheduler + handle and
  installs it via `app.wm().set_system_task_handle(system_handle)` (runner.rs:799-801); **no public
  getter** exists, so the app must receive the handle via a new host hook.
- The runner already clamps the driver sleep to the next task deadline
  (`driver.set_max_sleep_duration(min(fp_deadline, system_handle.time_until_next()))`, runner.rs:409-414)
  **and requests a redraw after draining tasks** (runner.rs:341-342) — so a scheduled task both fires
  on time and triggers a render.
- **Idle-wake gap (critical):** `ConsoleEventSource` (used by argtuner) does NOT implement
  `set_max_sleep_duration` (trait default no-op, console_event_source.rs) — its `poll_interval()` is
  purely profile-driven (8/16ms active, **3600s in PowerSaver**). So when idle it would sleep through
  a 500ms AppTick. `UnifiedEventSource` (term-wm main.rs) DOES clamp (unified_event_source.rs:457-464).
  → argtuner's recurring task needs ConsoleEventSource to honor the cap, mirroring UnifiedEventSource.

### Changes

#### term-wm-core (runner.rs + actions.rs)
1. `actions.rs:409` — add a 5th variant to `SystemTask`:
   ```rust
   /// Application-level recurring tick (e.g. periodic SQLite refresh).
   AppTick,
   ```
2. `runner.rs` `WindowManagerHost` trait — add two default no-op methods (backward compatible):
   ```rust
   /// Called once by `run_with_defaults` after the system scheduler is installed,
   /// BEFORE the event loop starts. Override to schedule recurring app tasks.
   fn on_app_scheduler_ready(&mut self, _handle: TaskHandle<SystemTask>) {}
   /// Called each time a scheduled `SystemTask::AppTick` fires.
   fn on_app_tick(&mut self) {}
   ```
3. `run_with_defaults` (runner.rs:799-801): clone the handle for the hook (TaskHandle is `Clone`):
   ```rust
   let system_handle = system_scheduler.handle();
   app.wm().set_system_task_handle(system_handle.clone());
   app.on_app_scheduler_ready(system_handle);
   ```
4. `run_event_loop` drain match (runner.rs:318-337): add
   ```rust
   SystemTask::AppTick => app.on_app_tick(),
   ```
   (The existing `driver.request_redraw()` after the drain loop already re-renders, so `on_app_tick`
   mutating `AppState` data is reflected on screen automatically.)

#### term-wm-console (console_event_source.rs)
5. Mirror UnifiedEventSource's sleep cap so a pending scheduler deadline keeps the idle loop awake:
   - add field `max_sleep_duration: Option<Duration>` (init `None`),
   - implement `fn set_max_sleep_duration(&mut self, d: Option<Duration>)`,
   - `poll_interval()` → `base.min(max_sleep.unwrap_or(base))` (non-Windows; keep the existing
     Windows clamp branch shape as in unified_event_source.rs:461-480).

#### argtuner (src/tui/mod.rs)
6. Imports: `use term_wm_core::actions::SystemTask;`,
   `use term_wm_core::task_scheduler::TaskHandle;`,
   `use term_wm_core::debug_log::global_debug_log;`.
7. `run()` (mod.rs:40-111): call `app.inner.init_system_windows();` right after constructing
   `AppState` and before `run_with_defaults` (creates hidden Debug Log window + installs logging;
   idempotent). No keybinding added (palette-only per user).
8. `AppState`:
   - remove field `last_refresh: Instant` (mod.rs:337, 73-75, and the gate in render);
   - keep `poll: Duration` (the recurring interval);
   - override `WindowManagerHost::on_app_scheduler_ready`:
     ```rust
     fn on_app_scheduler_ready(&mut self, handle: TaskHandle<SystemTask>) {
         let _ = handle.schedule_repeating(self.poll, SystemTask::AppTick);
     }
     ```
   - override `WindowManagerHost::on_app_tick`:
     ```rust
     fn on_app_tick(&mut self) {
         self.refresh_trials();
         if let Some(h) = global_debug_log() {
             h.push(format!(
                 "poll tick: {} trials, {} epoch rows",
                 self.trials.len(),
                 self.epoch_rows.len()
             ));
         }
     }
     ```
   - override `WindowManagerHost::toggle_debug_window` → `self.inner.toggle_debug_window();`
     (needed so the palette's "≣ Debug Log" item actually toggles the window).
9. `render()` (mod.rs:591-608): drop the `last_refresh`/`poll` time gate entirely; keep
   `self.push_data_to_components(); self.inner.render_app(backend);`. Delete the TODO comment
   (tui/mod.rs:592-601) — now resolved.

### Files to modify
- `term-wm/crates/term-wm-core/src/actions.rs` — `SystemTask::AppTick`.
- `term-wm/crates/term-wm-core/src/runner.rs` — trait hooks + `run_with_defaults` + drain arm.
- `term-wm/crates/term-wm-console/src/console_event_source.rs` — `set_max_sleep_duration` cap.
- `/Volumes/2TB Storage Vault/rust-argtuner/src/tui/mod.rs` — imports, `init_system_windows()`,
  AppState field/hook/render changes, debug-log push.

### Verification
1. `cargo build` + `cargo clippy` + `cargo test` (term-wm workspace, incl. term-wm-console) — green.
2. `cargo build`/`clippy`/`test` (argtuner, 71 lib tests) — green.
3. Manual (argtuner Watch):
   - Ctrl+A → Command Palette → "≣ Debug Log" opens the Debug Log window (palette-only; also opens
     on panics/errors).
   - With a trial running, the Debug Log fills with `poll tick: N trials, M epoch rows` roughly every
     `--poll-ms` (default 500ms) **even when the terminal is idle** (no keypresses) — proving the
     recurring scheduler task fires independent of the render loop.
   - Trials list still live-updates as before; selection is preserved across refreshes.

### Review disposition (Amendment 11)
- **SEV-1 (stale code block / ordering)** — ACCEPTED (documentation): Amendment 11 was inserted into
  the "Completed (Amendment 9)" section, so it appears BEFORE the Amendment-10 spec, making the
  older `AppTick`/`on_app_tick` text read like a regression. Fixed by adding a "SUPERSEDED" banner to
  the Amendment-10 section clarifying that ONLY its task-scheduler §2/§3 are reversed by Amendment 11,
  while the Debug Log / ConsoleEventSource / init_system_windows portions remain valid. Amendment 11's
  argtuner snippet (`render` w/o time-gate, `on_app_scheduler_ready` w/ `AppTask::new(closure)`, no
  `on_app_tick`) is the authoritative target state.
- **SEV-2 (`Rc<RefCell>` fails a `Send` bound)** — REJECTED (verified in source): `TaskScheduler<T>`
  and `TaskHandle<T>` are `Rc<RefCell<SchedulerInner<T>>>` (task_scheduler.rs:58-69), and
  `schedule_repeating`/`drain_expired` only require `T: Clone` (task_scheduler.rs:116, 174) — there is
  **no `Send`/`Sync` bound** on `T` anywhere, and the runner drains synchronously on the main thread
  (runner.rs:318-337) with no channels/mpsc involved. `Rc<RefCell<Box<dyn FnMut(&mut A)>>>` is
  therefore correct and consistent with the existing scheduler; switching to `Arc<Mutex>` would be
  unnecessary (adds lock overhead, zero benefit) and inconsistent with the rest of the Rc-based
  scheduler. Kept as specified.
- **SEV-1 (derive(Clone) generic bound trap)** — ACCEPTED (this review): `#[derive(Clone)]` on
  `AppTask<A>` injects an implicit `A: Clone` bound; `AppState` is `!Clone`, so
  `AppTask<AppState>` would not be `Clone` and `schedule_repeating`'s `T: Clone` (task_scheduler.rs:116)
  would fail to compile. Fixed by replacing the derive with a **manual `impl<A> Clone for AppTask<A>`**
  that clones only the inner `Rc` (Rc::clone), with no `A: Clone` bound. This is exactly how the
  existing `TaskHandle<T>` handles Clone (Rc inner, task_scheduler.rs:57-58).

## AMENDMENT 12 — Build argtuner's WM exactly like main.rs (Debug Log palette fix, no helpers)

### Problem
The Debug Log palette item ("≣ Debug Log") doesn't appear in argtuner. Root cause: argtuner
constructs its WM via `TermWmApp::new_with_config` (tui/mod.rs:44-48), which hardcodes a **reduced**
`supported_menu_actions` list (term_wm_app.rs:139-144: CloseMenu, ToggleMouseCapture,
ToggleClipboardMode, ToggleWindowSelection, ExitUi). The command palette filter
(focus.rs:76-98) drops any menu item whose action is NOT in `supported_menu_actions`, so
`ToggleDebugWindow` is filtered out → no "Debug Log" entry.

**main.rs never calls `.supported_menu_actions(...)`** (src/main.rs:109-124). The `AppBuilder`
default (window_manager/mod.rs:785-796) therefore fills in the **full** list which INCLUDES
`ToggleDebugWindow`. That's why main.rs works and argtuner doesn't — the two paths differ only in
this one `.supported_menu_actions(...)` call.

### Fix — make argtuner build the WM like main.rs
argtuner should construct the WM via `AppBuilder` directly (exactly like main.rs), passing its
custom `WmConfig` and **omitting** `.supported_menu_actions(...)` so the full default list applies.
Wrap the result in `TermWmApp::from_wm` (same as main.rs via its own `new_with`).

Changes in `/Volumes/2TB Storage Vault/rust-argtuner/src/tui/mod.rs`:
- Replace the `TermWmApp::new_with_config(...)` call (mod.rs:44-48) with a direct build:
  ```rust
  let app_name = env!("CARGO_PKG_NAME").to_string();
  let app_version = env!("CARGO_PKG_VERSION").to_string();
  let hostname = None; // argtuner doesn't pass a hostname

  use term_wm_sys_ui_components::{
      WmBottomPanelComponent, WmFabComponent, WmNotificationAreaComponent, WmTopPanelComponent,
  };
  let wm = AppBuilder::<LayerComponent>::new()
      .config(config)                       // custom keybindings, same as new_with_config
      .app_ctx(Arc::new(AppContext::new(app_name, app_version)))
      .top_panel(LayerComponent::TopPanel(WmTopPanelComponent::new(&app_name)))
      .bottom_panel(LayerComponent::BottomPanel(WmBottomPanelComponent::new(
          &app_name, &app_version, hostname,
      )))
      .fab(LayerComponent::Fab(WmFabComponent::new()))
  .build()
  .expect("standalone build");
  // `from_wm` needs `crossbeam_channel::Sender<UnifiedEvent>`; match
  // `new_with_config` (term_wm_app.rs:151): `let (tx, _) = bounded(256);`
  use term_wm::unified_event_source::UnifiedEvent;   // or the re-export path used elsewhere
  let (tx, _) = crossbeam_channel::bounded(256);
  let mut inner = TermWmApp::<AppComponent>::from_wm(wm, tx);
  ```
- **Remove** the argtuner-side `set_supported_menu_actions(...)` call added earlier in this session
  (tui/mod.rs ~103-121). It is now unnecessary — the default list already includes Debug Log.
- Keep `app.inner.init_system_windows()` (already present) — this creates the hidden Debug Log
  window + logging, matching main.rs.
- Keep the `TermWmApp` / `AppBuilder` / `LayerComponent` imports; add
  `use term_wm::config::AppBuilder;`, `use term_wm::app_context::AppContext;`, and the sys-ui
  component imports if not already reachable.

### term-wm cleanup (REVERT the helper from earlier this session)
- `crates/term-wm-core/src/window/window_manager/mod.rs` — **remove** the `set_supported_menu_actions`
  setter added earlier (mod.rs ~2587-2595). It was an unnecessary workaround; the real fix is
  constructing like main.rs. Revert to the pre-existing getter-only state.
- `src/term_wm_app.rs` — **leave untouched** (already reverted to the reduced-list
  `new_with_config`; we no longer depend on it from argtuner).

### Verification
1. `cargo build` (term-wm) + `cargo test` — green; confirm the mod.rs setter removal compiles.
2. `cargo build`/`clippy`/`test` (argtuner, 71 lib tests) — green.
3. Manual (argtuner Watch):
   - Ctrl+A → Command Palette now lists "≣ Debug Log"; selecting it opens the Debug Log window
     (Mapped + focused), exactly like main.rs.
   - Recurring `poll tick: N trials, M epoch rows` lines stream into the Debug Log every `--poll-ms`
     even when idle.

### Review disposition (Amendment 12)
- **"You did NOT implement it like main.rs; why the helper functions?"** — ACCEPTED. The helper
  `set_supported_menu_actions` was a workaround for the real root cause: argtuner's construction
  path (`TermWmApp::new_with_config`) hardcodes a reduced `supported_menu_actions` list that omits
  `ToggleDebugWindow`, whereas main.rs builds its WM via `AppBuilder` with NO
  `.supported_menu_actions(...)` call, so the builder's full default list (which includes Debug Log)
  applies. The correct fix is to make argtuner build the WM exactly like main.rs (direct `AppBuilder`
  + `from_wm`), not to add helpers. Amendment 12 does that and removes the helper setter.
- **SEV-1 (stale snippet / regressions)** — REJECTED (verified against source): the reviewed "snippet"
  does not match the actual `src/tui/mod.rs`. The source already implements Amendment 11: `render()`
  has no `last_refresh` time-gate (tui/mod.rs:609-612), no `on_app_tick`/`AppTick` exists, and
  `on_app_scheduler_ready` uses the `AppTask` callback (tui/mod.rs:616-626). The only remaining
  items — `TermWmApp::new_with_config` (tui/mod.rs:48) and the `set_supported_menu_actions` call
  (tui/mod.rs:110) — are exactly what Amendment 12 replaces/removes. The `last_refresh`/`AppTick`
  text the reviewer saw is historical spec text inside the plan's Amendment-10 section (marked
  SUPERSEDED), not the implementation. No regression exists.

## AMENDMENT 13 — Precision fix: add `ToggleDebugWindow` to the safe baseline (REPLACES Amendment 12's manual-build approach)

### Why (user feedback)
Amendment 12 tried to make the Debug Log appear in argtuner's palette by building the WM manually
like main.rs (`AppBuilder` + `from_wm`). That leaked boilerplate (a dummy `bounded(256)` PTY channel,
`term-wm-sys-ui-components`/`crossbeam-channel` deps) into argtuner. Then a one-line "fix" removing
`.supported_menu_actions(...)` from `new_with_config` had a huge blast radius — every `new_with_config`
consumer (e.g. dual_image) would suddenly get the FULL default list including `NewWindow`,
`ToggleMonocle`, `ToggleTiling`, etc., which standalone TUIs don't support. Bad UX.

**Resolution (confirmed by user):** the restricted list in `new_with_config` is a deliberate, safe
baseline. The Debug Log is a framework diagnostic (not an app feature), so it **belongs in that safe
baseline**. Add `TermWmAction::ToggleDebugWindow` to the hardcoded list — one line — and keep
argtuner on the clean `new_with_config` path with zero boilerplate.

### Current state (verified)
- argtuner `src/tui/mod.rs` already: uses `TermWmApp::new_with_config(...)` (mod.rs:44-48), calls
  `app.inner.init_system_windows()` (mod.rs:107), has `on_app_scheduler_ready` with `AppTask`
  (mod.rs:601-612) + `toggle_debug_window` delegation (mod.rs:614-616). No `set_supported_menu_actions`
  workaround remains. Cargo.toml has NO sys-ui/crossbeam deps (already reverted).
- term-wm `src/term_wm_app.rs:138-145` still has the reduced `supported_menu_actions` list, missing
  `ToggleDebugWindow`. `set_supported_menu_actions` setter is already reverted (git checkout).

### Change
1. `/Volumes/2TB Storage Vault/term-wm/src/term_wm_app.rs` (`new_with_config`, lines 138-145) — add
   `ToggleDebugWindow` to the safe baseline list:
   ```rust
   .supported_menu_actions(vec![
       TermWmAction::CloseMenu,
       TermWmAction::ToggleMouseCapture,
       TermWmAction::ToggleClipboardMode,
       TermWmAction::ToggleWindowSelection,
       TermWmAction::ToggleDebugWindow,   // ← add: framework diagnostic, safe for all apps
       TermWmAction::ExitUi,
   ])
   ```
2. No argtuner source changes needed — it already uses `new_with_config`, so the Debug Log palette
   item appears automatically once the WM's `supported_menu_actions` includes it. (The palette filter
   focus.rs:76-98 keeps any item whose action is in the list.)

### Files to modify
- `/Volumes/2TB Storage Vault/term-wm/src/term_wm_app.rs` — one-line add in the safe-baseline list.

### Verification
1. `cargo build` + `cargo test` (term-wm) — green; dual_image / other `new_with_config` consumers get
   the SAME reduced list as before PLUS the Debug Log toggle (no NewWindow/Monocle/Tiling leak).
2. `cargo build`/`clippy`/`test` (argtuner, 71 lib tests) — green; no new deps.
3. Manual (argtuner Watch):
   - Ctrl+A → Command Palette lists "≣ Debug Log"; selecting it opens the Debug Log window
     (Mapped + focused).
   - Palette does NOT show New Window / Toggle Monocle / Toggle Tiling (reduced baseline preserved).
   - Recurring `poll tick: N trials, M epoch rows` lines stream into the Debug Log every `--poll-ms`
     even when idle.

## AMENDMENT 14 — Use standardized `tracing` logging for the poll tick (not raw debug-log pushes)

### Problem (user feedback)
argtuner's `on_app_tick` writes the poll line via `global_debug_log().push(format!("poll tick: ..."))`
(tui/mod.rs:604-608). This bypasses the framework's standardized logging: `init_system_windows()`
→ `init_default()` (term-wm src/logging.rs:87-113) installs a `tracing_subscriber` whose `fmt` layer
uses `SubscriberMakeWriter` → `DebugLogWriter`, so **every `tracing::info!` automatically appears in
the Debug Log window with a timestamp**. The `fmt` layer default `SystemTime` timer
(tracing-subscriber format.rs:613-617) emits a timestamp prefix; `.compact()` keeps it
(format.rs:634-637). My raw `push()` produced untimed, ad-hoc lines — the user rightly wants the
standard path.

### Change (all in `/Volumes/2TB Storage Vault/rust-argtuner`)
1. `Cargo.toml`:
   - `[workspace.dependencies]`: add `tracing = "0.1.44"` (already in Cargo.lock transitively).
   - `[dependencies]`: add `tracing.workspace = true`.
2. `src/tui/mod.rs`:
   - Replace the `global_debug_log()` block in `on_app_scheduler_ready`'s callback with
     `tracing::info!`:
     ```rust
     fn on_app_scheduler_ready(&mut self, handle: TaskHandle<AppTask<Self>>) {
         let _ = handle.schedule_repeating(self.poll, AppTask::new(|app: &mut Self| {
             app.refresh_trials();
             tracing::info!(
                 "poll tick: {} trials, {} epoch rows",
                 app.trials.len(),
                 app.epoch_rows.len()
             );
         }));
     }
     ```
   - Remove `use term_wm_core::debug_log::global_debug_log;` (tui/mod.rs:36) if it becomes unused.

### Files to modify
- `/Volumes/2TB Storage Vault/rust-argtuner/Cargo.toml` — add `tracing`.
- `/Volumes/2TB Storage Vault/rust-argtuner/src/tui/mod.rs` — `tracing::info!` in the AppTask callback;
  drop the `global_debug_log` import.

### Verification
1. `cargo build` + `cargo clippy` (only pre-existing talkback.rs warnings) + `cargo test`
   (71 lib tests) — green.
2. Manual (argtuner Watch): Ctrl+A → "≣ Debug Log" opens; every `--poll-ms` a line like
   `2026-08-07T…Z INFO poll tick: N trials, M epoch rows` appears — timestamped, matching the
   format of term-wm's own `tracing::info!` output (e.g. runner.rs:377).

## AMENDMENT 15 — Add `new_with_actions` constructor (explicit supported menu actions)

### Why (user feedback)
The safe-baseline fix (Amendment 13, adding `ToggleDebugWindow` to `new_with_config`) made the Debug
Log appear, but forcing it onto EVERY `new_with_config` consumer is wrong — and the earlier
"build AppBuilder manually" approach leaked boilerplate (dummy channel, sys-ui deps). The clean
design (confirmed with user, Option 1): add a constructor that takes the supported menu actions
explicitly, and have the existing `new_with_config` delegate to it with its current reduced list.
argtuner calls the new constructor with its own list (incl. `ToggleDebugWindow`). Zero regressions,
no dummy channel in argtuner, no shared-behavior change.

### Corrected design (the user's sketch had 3 type bugs — fixed here)
- Impl bound is `Component<TermWmAction>` (term_wm_app.rs:100), NOT `Component<UnifiedEvent>`.
- `AppContext` is passed **by value** (`AppContext`), matching the existing `new_with_config`/`new_custom`
  signatures so callers (argtuner tui/mod.rs:48, dual_image.rs:25) don't break. Do NOT switch to
  `Arc<AppContext>`.
- `AppContext` fields are `pub` (app_context.rs:11-13) → use `app_ctx.app_name`/`app_version`/`hostname`
  directly (same as the current body).
- **Keep `set_notification_component(...)`** (NotificationArea) — the user's sketch dropped it; the
  current body sets it (term_wm_app.rs:149-152) and must be preserved.

### Changes
#### `/Volumes/2TB Storage Vault/term-wm/src/term_wm_app.rs`
1. Add `new_with_actions` next to `new_with_config` (inside the same
   `impl<C: Component<TermWmAction>> TermWmApp<C>` block, term_wm_app.rs:100), containing the body
   currently in `new_with_config` but with the actions list parameterized:
   ```rust
   /// Standalone constructor with system chrome + explicit supported command
   /// palette actions. `new_with_config` delegates here with its default list.
   #[cfg(feature = "sys-ui")]
   pub fn new_with_actions(
       app_ctx: AppContext,
       config: WmConfig,
       actions: Vec<TermWmAction>,
   ) -> Self {
       let app_name = app_ctx.app_name.clone();
       let app_version = app_ctx.app_version.clone();
       let hostname = app_ctx.hostname.clone();

       use term_wm_sys_ui_components::{
           WmBottomPanelComponent, WmFabComponent, WmNotificationAreaComponent, WmTopPanelComponent,
       };

       let wm = AppBuilder::<LayerComponent>::new()
           .config(config)
           .app_ctx(Arc::new(app_ctx))
           .top_panel(LayerComponent::TopPanel(WmTopPanelComponent::new(&app_name)))
           .bottom_panel(LayerComponent::BottomPanel(WmBottomPanelComponent::new(
               &app_name, &app_version, hostname.as_deref(),
           )))
           .fab(LayerComponent::Fab(WmFabComponent::new()))
           .supported_menu_actions(actions)
           .build()
           .expect("standalone build");
       let mut wm = wm;
       wm.set_notification_component(LayerComponent::NotificationArea(
           WmNotificationAreaComponent::new(),
       ));
       let (tx, _) = bounded(256);
       Self::from_wm(wm, tx)
   }
   ```
2. Replace `new_with_config`'s body with a delegation call to the new constructor (keeping its
   existing signature + doc):
   ```rust
   pub fn new_with_config(app_ctx: AppContext, config: WmConfig) -> Self {
       Self::new_with_actions(app_ctx, config, vec![
           TermWmAction::CloseMenu,
           TermWmAction::ToggleMouseCapture,
           TermWmAction::ToggleClipboardMode,
           TermWmAction::ToggleWindowSelection,
           TermWmAction::ExitUi,
       ])
   }
   ```
   (Behavior unchanged — same reduced list as today.)

#### `/Volumes/2TB Storage Vault/rust-argtuner/src/tui/mod.rs`
3. Switch argtuner's `run()` from `new_with_config` to `new_with_actions` with its own list (the
   reduced baseline **plus** `ToggleDebugWindow`):
   ```rust
   let mut inner = TermWmApp::<AppComponent>::new_with_actions(
       AppContext::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
       config,
       vec![
           TermWmAction::CloseMenu,
           TermWmAction::ToggleMouseCapture,
           TermWmAction::ToggleClipboardMode,
           TermWmAction::ToggleWindowSelection,
           TermWmAction::ToggleDebugWindow,
           TermWmAction::ExitUi,
       ],
   );
   ```
   Keep `app.inner.init_system_windows()` (tui/mod.rs:107) and the `AppTask`/`tracing::info!`
   changes from Amendments 11/14. No new deps; no dummy channel; no `AppBuilder` boilerplate in
   argtuner.

### Files to modify
- `/Volumes/2TB Storage Vault/term-wm/src/term_wm_app.rs` — add `new_with_actions`; make
  `new_with_config` delegate.
- `/Volumes/2TB Storage Vault/rust-argtuner/src/tui/mod.rs` — call `new_with_actions` with the
  explicit list incl. `ToggleDebugWindow`.

### Verification
1. `cargo build` + `cargo test` (term-wm) — green; `new_custom`/`new_with_config` (dual_image +
  argtuner legacy path) unchanged; new `new_with_actions` compiles.
2. `cargo build`/`clippy` (only pre-existing talkback.rs warnings)/`test` (argtuner, 71 lib tests) —
  green.
3. Manual (argtuner Watch):
   - Ctrl+A → palette shows "≣ Debug Log" (argtuner opted in via `new_with_actions`), but NOT
     New Window / Toggle Monocle / Toggle Tiling.
   - Debug Log window opens (Mapped + focused); `poll tick: N trials, M epoch rows` lines appear
     timestamped via `tracing::info!` every `--poll-ms`.

### Completed (Amendments 14 + 15)
- term-wm `term_wm_app.rs`: added `new_with_actions(app_ctx, config, actions)` (full chrome body,
  actions parameterized); `new_with_config` now delegates to it with the unchanged reduced list.
  `new_custom`/`new_with_config` signatures untouched.
- argtuner `src/tui/mod.rs`: calls `new_with_actions` with the reduced list + `ToggleDebugWindow`;
  poll tick logged via `tracing::info!` (timestamped, routed to Debug Log window); removed the
  `global_debug_log` import. Added `tracing = "0.1.44"` (workspace + package dep).
- `cargo build`/`clippy` (only pre-existing talkback.rs warnings)/`test` (term-wm 10 suites,
  argtuner 71) all green.

## AMENDMENT 16 — Add `fire_immediate` bool to `TaskHandle::schedule_repeating`

### Why (user feedback)
argtuner's initial-poll workaround (Amendment 15 follow-up) manually pairs
`schedule_once(Duration::ZERO, ...)` with `schedule_repeating(...)` (tui/mod.rs:619-620) to get an
immediate first fire. The user wants this built into the scheduler API: `schedule_repeating` gains a
`fire_immediate: bool` parameter so the first deadline is `now` (instead of `now + interval`),
removing the manual `schedule_once` dance.

### Verified facts
- `TaskHandle::schedule_repeating(&self, interval, payload)` (task_scheduler.rs:115-129) pushes a
  `HeapEntry` with `deadline: Instant::now() + interval`, `interval: Some(interval)`.
- `drain_expired` re-inserts repeating tasks at `entry.deadline + interval` (task_scheduler.rs:190-196),
  so a first deadline of `now` gives: fire immediately, then `now+interval`, `now+2·interval`, ... —
  exactly the desired cadence.
- Callers of `schedule_repeating`: 3 unit tests (task_scheduler.rs:330, 343, 375), plus argtuner
  tui/mod.rs:620. `schedule_once` callers are unaffected.

### Changes
#### `/Volumes/2TB Storage Vault/term-wm/crates/term-wm-core/src/task_scheduler.rs`
1. Change `schedule_repeating` signature to take a `fire_immediate: bool` and set the first deadline
   accordingly:
   ```rust
   /// Schedule a repeating task.
   ///
   /// The payload is cloned on each re-insertion.  The next deadline is
   /// computed as `original_deadline + interval` to prevent timer drift
   /// under load.  When `fire_immediate` is true the first fire happens on
   /// the next `drain_expired` (deadline = now); otherwise it waits one
   /// interval.
   pub fn schedule_repeating(
       &self,
       interval: Duration,
       fire_immediate: bool,
       payload: T,
   ) -> TaskId
   where
       T: Clone,
   {
       let mut inner = self.inner.borrow_mut();
       let id = TaskId(inner.next_id);
       inner.next_id += 1;
       let deadline = if fire_immediate {
           Instant::now()
       } else {
           Instant::now() + interval
       };
       inner.heap.push(HeapEntry {
           deadline,
           interval: Some(interval),
           payload,
           id,
       });
       id
   }
   ```
2. Update the 3 unit-test call sites to pass `false` (preserving current behavior):
   - task_scheduler.rs:330 `schedule_repeating(Duration::from_millis(10), false, "tick")`
   - task_scheduler.rs:343 `schedule_repeating(Duration::from_millis(10), false, "tick")`
   - task_scheduler.rs:375 `schedule_repeating(Duration::from_secs(60), false, "slow")`
3. (Optional) Add a unit test asserting `fire_immediate=true` produces an entry whose deadline is
   `<= now` (peekable via the same `inner.heap` technique as `anti_drift_uses_original_deadline`).

#### `/Volumes/2TB Storage Vault/rust-argtuner/src/tui/mod.rs`
4. Replace the manual `schedule_once` + `schedule_repeating` pair in `on_app_scheduler_ready`
   (tui/mod.rs:619-620) with a single call using `fire_immediate = true`:
   ```rust
   fn on_app_scheduler_ready(&mut self, handle: TaskHandle<AppTask<Self>>) {
       let _ = handle.schedule_repeating(
           self.poll,
           true, // fire immediately, then every --poll-ms
           AppTask::new(|app: &mut Self| {
               app.refresh_trials();
               tracing::info!(
                   "poll tick: {} trials, {} epoch rows",
                   app.trials.len(),
                   app.epoch_rows.len()
               );
           }),
       );
   }
   ```

### Files to modify
- `/Volumes/2TB Storage Vault/term-wm/crates/term-wm-core/src/task_scheduler.rs` — signature +
  deadline logic; 3 test call sites; optional new test.
- `/Volumes/2TB Storage Vault/rust-argtuner/src/tui/mod.rs` — single `schedule_repeating(..., true, ...)`
  call; drop the `schedule_once` line.

### Verification
1. `cargo build` + `cargo clippy` + `cargo test` (term-wm) — green; the 3 existing repeating-task
   tests still pass with `false`; new `fire_immediate=true` test (if added) passes.
2. `cargo build`/`clippy` (only pre-existing talkback.rs warnings)/`test` (argtuner, 71 lib tests) —
   green.
3. Manual (argtuner Watch): on launch, a `poll tick` line appears immediately in the Debug Log
   (no 5s wait), then every `--poll-ms` (default 5000ms) thereafter.

### Completed (Amendment 16)
- `task_scheduler.rs`: `schedule_repeating(interval, fire_immediate, payload)` — first deadline is
  `Instant::now()` when `fire_immediate`, else `now + interval`; anti-drift re-insertion unchanged
  (`deadline + interval`). 3 internal test call sites updated to pass `false`.
- argtuner `on_app_scheduler_ready`: single `schedule_repeating(self.poll, true, AppTask::new(...))`,
  manual `schedule_once` pair removed.
- `cargo build`/`clippy` (only pre-existing talkback.rs warnings)/`test` (term-wm 10 suites incl.
  term-wm-core 391, argtuner 71) all green.

## AMENDMENT 17 — Verify cancel works for BOTH once + repeating tasks; comprehensive tests

### Goal (user feedback)
"the task scheduler for both schedule once and repeated tasks should have the ability to cancel a
task. Everything should also be tested."

Cancel ALREADY works for both: `TaskHandle::cancel(id)` (task_scheduler.rs:147) inserts into the
`cancelled` set; BOTH `drain_expired` and `drain_expired_once` skip cancelled IDs at the top of the
heap loop (task_scheduler.rs:200-202 → `if inner.cancelled.remove(&entry.id) { continue; }`). No
production code change is needed. The work is **test coverage** — the current cancel tests leave
gaps for both variants.

### Existing cancel tests (task_scheduler.rs)
- `cancel_prevents_firing` — once task, cancel before deadline, drained via `drain_expired_once`.
- `cancel_repeating_stops_future_fires` — repeating, cancel AFTER first fire, drained via
  `drain_expired`.
- `cancel_non_existent_is_noop` — bogus TaskId.
- `anti_drift_uses_original_deadline` — cancels at end to avoid waiting.

### Coverage gaps to fill (all in `crates/term-wm-core/src/task_scheduler.rs` `#[cfg(test)]`)
1. **`cancel_once_before_deadline_suppresses`** — `schedule_once(60s, "x")` + `cancel(id)` →
   `has_pending()` false, `time_until_next()` None, both drains empty (assert cancel is O(1)-lazy
   but fully suppresses). Complements the existing 1ms-deadline test which already passes; this one
   asserts the queue state is clean.
2. **`cancel_once_after_deadline_not_yet_drained_suppresses`** — `schedule_once(1ms, "x")`, sleep
   10ms (deadline passes), then `cancel(id)` BEFORE `drain_expired_once` → still empty. Verifies
   cancel works even when the deadline has elapsed but the task hasn't been drained yet.
3. **`cancel_repeating_before_first_fire_suppresses_all`** — `schedule_repeating(10ms, false, "tick")`,
   cancel immediately, sleep 25ms, `drain_expired()` → empty. (Existing repeating test only cancels
   AFTER the first fire.)
4. **`cancel_repeating_fire_immediate_suppresses`** — `schedule_repeating(10ms, true, "tick")`,
   cancel immediately, sleep 25ms, `drain_expired()` → empty. Verifies cancel works with the new
   `fire_immediate=true` path.
5. **`cancel_repeating_fire_immediate_then_stops_after_first`** — `schedule_repeating(10ms, true,
   "tick")`, sleep ~1ms, `drain_expired()` → exactly 1 fire; then `cancel(id)`, sleep 15ms,
   `drain_expired()` → empty. Verifies immediate-first-fire then stop on cancel (mirrors
   argtuner's usage).
6. **`cancel_via_shared_handle`** — schedule via `h1`, cancel via `h2` (both from the same
   `TaskScheduler`), sleep past deadline, both drains empty. Verifies cancel propagates through
   `TaskHandle` clones (the argtuner runner passes the handle around).

### Files to modify
- `/Volumes/2TB Storage Vault/term-wm/crates/term-wm-core/src/task_scheduler.rs` — add the 6 tests
  above (test-only changes; no production code touched).

### Verification
1. `cargo test -p term-wm-core` — all new tests pass alongside the existing 391; no failures.
2. `cargo clippy -p term-wm-core --all-targets` — clean.
3. `cargo test` (full term-wm workspace) — 10 suites green.
4. `cargo build`/`test` (argtuner) — unaffected (no argtuner change), still 71 pass.

### Completed (Amendment 17)
- Confirmed cancel already works for both once + repeating (ID-based `cancelled` set honored by both
  `drain_expired` and `drain_expired_once`). No production cancel change needed.
- `has_pending()` and `time_until_next()` now lazily purge cancelled top-of-heap entries via a new
  `purge_cancelled` helper, so queue queries reflect only live tasks (per review's SEV-1 note).
- Added 6 tests in `task_scheduler.rs`: `cancel_once_before_deadline_suppresses`,
  `cancel_once_after_deadline_not_yet_drained_suppresses`,
  `cancel_repeating_before_first_fire_suppresses_all`,
  `cancel_repeating_fire_immediate_suppresses`,
  `cancel_repeating_fire_immediate_then_stops_after_first`, `cancel_via_shared_handle`.
- term-wm-core scheduler tests now 18 passed; full term-wm 10 suites green; clippy clean; argtuner
  71 passed.

## AMENDMENT 18 — Documentation accuracy audit + fixes

### Audit result
Two explore agents audited all docs in both repos against the current implementation.

**argtuner** — `README.md` was badly stale:
- Documented nonexistent subcommands `argtuner show` and `argtuner trial ...` (CLI only has
  Run/RebuildCsv/Watch/Plan).
- Stale `crates/tuner/examples/...` paths (package lives at workspace root).
- False `trial_<config_id>_b<bracket>` trial-dir claim (always `artifacts/trial_{trial_id}`).
- Malformed markdown section (list items inside a code fence).
- Zero coverage of the `watch` TUI (windows, keybindings, Debug Log, 5000ms poll + immediate
  first poll).
- `examples/guitar_tuning/README.md`: phantom `[project.fixed]` table (would fail
  `deny_unknown_fields`), now corrected to the real fixed-scheduler config.

**term-wm** — mostly accurate; fixed:
- `src/term_wm_app.rs` "Choosing a constructor" doc table: added `new_with_config` +
  `new_with_actions`, clarified the reduced command-palette allow-list (5 actions) vs the
  full default set.
- `AGENTS.md`: stale `WmCommandPaletteOverlay`/`wm_menu_overlay.rs`,
  `src/components/mod.rs`, and `Pane` path (term-wm-core → term-wm-pty-engine).

**Verified accurate (no change):** term-wm README, CHANGELOG (documents new API), COMPATIBILITY,
PROFILING, WINDOW-BORDERS.txt, help.md, all crate READMEs, all audited doc-comments (task_scheduler,
runner, actions, debug_log, window_manager); argtuner bindings/interactive-probe/loss-pattern READMEs;
CLI help text.

### Changes made
- `/Volumes/2TB Storage Vault/rust-argtuner/README.md` — removed show/trial sections, fixed paths +
  trial-dir claim, repaired malformed block, added "Watch (live TUI)" section.
- `/Volumes/2TB Storage Vault/rust-argtuner/examples/guitar_tuning/README.md` — corrected config claim.
- `/Volumes/2TB Storage Vault/term-wm/src/term_wm_app.rs` — constructor doc table.
- `/Volumes/2TB Storage Vault/term-wm/AGENTS.md` — 3 stale path/type references.
- Both repos build clean.

## AMENDMENT 19 — Formalize examples (all under `examples/`) + require `--project` for watch

### Goal (user feedback)
1. "My mixture of examples are scattershot… /examples and /crates/tooling examples… need a more
   formalized approach." → Consolidate ALL examples into one canonical location, each self-contained.
2. "The TUI should require --project to even launch." → `watch` requires `--project`; drop `--db`.

### Target structure (confirmed with user)
Every example becomes a cargo **subdir example** (`examples/<name>/main.rs`, which cargo
auto-discovers; CI's `--examples` already builds them). Each dir: `main.rs` (+ helpers) +
`argtuner.toml` + `README.md`. Fully uniform:

```
examples/
├── linear_regression/          (moved from examples/linear_regression.rs; ADD argtuner.toml)
│   ├── main.rs
│   ├── argtuner.toml           (NEW — same template as README/tests use)
│   └── README.md               (NEW)
├── guitar_tuning/              (already self-contained — unchanged)
│   ├── main.rs  argtuner.toml  README.md
├── interactive_probe/          (converted from crates/tooling/interactive-probe)
│   ├── main.rs                 (from src/main.rs)
│   ├── argtuner.toml           (from probe-tuning-project/argtuner.toml)
│   └── README.md               (from crate README; fix run paths)
└── loss_pattern_generator/     (converted from crates/tooling/loss-pattern-generator)
    ├── main.rs  patterns.rs    (from src/)
    ├── argtuner.toml           (from loss-tuning-project/argtuner.toml)
    └── README.md               (from loss-tuning-project/README.md)
```

### Changes

#### A. Examples consolidation (all in `/Volumes/2TB Storage Vault/rust-argtuner`)
1. **`examples/linear_regression.rs` → `examples/linear_regression/main.rs`** (git mv), add
   `examples/linear_regression/argtuner.toml` (template
   `cargo run -p argtuner --example linear_regression -- --lr {lr} --steps {steps} --checkpoint-dir {trial_dir}`,
   metric `metric`, goal `min`, sampler `random`, scheduler `fixed` n_trials small) and a short
   `README.md`. Verify `tests/linear_regression.rs:22` still uses `--example linear_regression` —
   cargo subdir examples keep the same `--example <name>` name, so no test change needed.
2. **`crates/tooling/interactive-probe/src/main.rs` → `examples/interactive_probe/main.rs`**;
   `crates/tooling/interactive-probe/probe-tuning-project/argtuner.toml` →
   `examples/interactive_probe/argtuner.toml`; rewrite its template to the example name:
   `cargo run -q -p argtuner --example interactive_probe -- --metric-key metric --checkpoint-dir {trial_dir}`;
   move crate README → `examples/interactive_probe/README.md` (fix paths: `cargo run -p argtuner -- run examples/interactive_probe`,
   direct-run `cargo run -p argtuner --example interactive_probe -- ...`). Delete the crate dir.
3. **`crates/tooling/loss-pattern-generator/src/{main,patterns}.rs` → `examples/loss_pattern_generator/`**;
   `loss-tuning-project/argtuner.toml` → `examples/loss_pattern_generator/argtuner.toml` with template
   rewritten to `cargo run -q -p argtuner --example loss_pattern_generator -- --metric-key val_loss --checkpoint-dir {trial_dir} --pattern {pattern} --noise {noise} --spike-prob {spike_prob} --epoch-time {epoch_time}`;
   move `loss-tuning-project/README.md` → `examples/loss_pattern_generator/README.md` (fix paths).
   Delete the crate dir.
4. **`Cargo.toml`**: remove `crates/tooling/interactive-probe` + `crates/tooling/loss-pattern-generator`
   from `[workspace] members` (lines 18-19). Add `indicatif.workspace = true` to root `[dev-dependencies]`
   (the converted loss-pattern example needs it; it's already a workspace dep at line 44). Delete the
   two crates' Cargo.tomls. Run `cargo update`/build to refresh the lockfile.
5. **Top-level `README.md`**: the "Watch" + example sections already reference
   `examples/linear_regression.rs`/`examples/guitar_tuning/`; update any `examples/linear_regression.rs`
   mention to `examples/linear_regression/`, and add the two new examples
   (`interactive_probe`, `loss_pattern_generator`) to the README.

#### B. `watch` requires `--project` (in `/Volumes/2TB Storage Vault/rust-argtuner/src/main.rs`)
6. `Commands::Watch` (main.rs:43-54): make `--project` a required arg; remove `--db`. A bare
   `project: PathBuf` (non-`Option`) is **implicitly required** by clap v4 — no `required = true`
   attribute needed:
   ```rust
   Watch {
       /// Path to the project directory (watches <dir>/trials.sqlite)
       #[arg(long, value_name = "PROJECT_DIR")]
       project: PathBuf,
       /// Polling interval (ms)
       #[arg(long, default_value_t = 5000)]
       poll_ms: u64,
   },
   ```
7. Dispatch (main.rs:97-114) simplifies to:
   ```rust
   Commands::Watch { project, poll_ms } => {
       let project = Project::new(project);
       if let Err(e) = tui::run(project.trials_db_path(), *poll_ms) {
           eprintln!("Error: {}", e);
           std::process::exit(1);
       }
   }
   ```
   (No more `(_, Some(db))` / `(None, None)` fallback arms.)
8. **Tests**: add a small `tests/cli_watch.rs` that exercises the ROOT `Cli` parser. `Commands`
   derives `Subcommand` (main.rs:22) and does NOT expose `command()` — parsing must go through
   `Cli` (main.rs:9, derives `Parser`):
   ```rust
   use argtuner::project::Project;
   use clap::Parser;

   // The Cli type is private to the binary crate; test via the binary's own
   // main.rs by extracting Cli into a `pub` module, OR assert through the
   // binary directly. If Cli is not reachable from integration tests, put the
   // test in a `#[cfg(test)] mod` inside src/main.rs using
   // `Cli::try_parse_from([...])`.
   ```
   Concretely, either:
   - **Preferred**: add a `#[cfg(test)] mod cli_tests` inside `src/main.rs` (it already imports
     `Cli`/`Parser`) with:
     ```rust
     #[test]
     fn watch_requires_project() {
         assert!(Cli::try_parse_from(["argtuner", "watch"]).is_err());
         assert!(Cli::try_parse_from(["argtuner", "watch", "--project", "x"]).is_ok());
         assert!(Cli::try_parse_from(["argtuner", "watch", "--db", "x"]).is_err());
     }
     ```
     (No new file, no visibility problem, and directly validates the `--project`/`--db` contract.)
   - Or extract `Cli`/`Commands` to a `pub` location for an integration test.
9. Update docs that mention `watch --db` / implicit `trials.sqlite`: argtuner README "Watch" section
   (remove `--db` examples, `--db path/to/trials.sqlite` line), and the
   `loss_pattern_generator/README.md` watch example (remove `--db` usage if any).

### Files to modify
- `/Volumes/2TB Storage Vault/rust-argtuner/examples/` — moves + new linear_regression project.
- `/Volumes/2TB Storage Vault/rust-argtuner/crates/tooling/` — delete the two crates.
- `/Volumes/2TB Storage Vault/rust-argtuner/Cargo.toml` — workspace members + dev-deps.
- `/Volumes/2TB Storage Vault/rust-argtuner/src/main.rs` — `--project` required (implicit), drop
  `--db`, add `#[cfg(test)] mod cli_tests`.
- `/Volumes/2TB Storage Vault/rust-argtuner/README.md` + example READMEs — path/flag updates.

### Verification
1. `cargo build` + `cargo clippy` + `cargo test` (argtuner, 71 lib tests + new CLI test; CI runs
   `--examples` so converted examples compile).
2. `cargo test --examples` — all four examples (linear_regression, guitar_tuning, interactive_probe,
   loss_pattern_generator) build.
3. Manual: `argtuner watch` (no --project) → clap error; `argtuner watch --project examples/loss_pattern_generator`
   → TUI opens; `--db` flag no longer exists.
4. `cargo run -p argtuner -- run examples/interactive_probe` and `... -- run examples/loss_pattern_generator`
   work end-to-end.

### Completed (Amendment 19)
- **Examples consolidated** under `examples/`, each self-contained (main.rs + argtuner.toml + README):
  - `examples/linear_regression/` — moved from `examples/linear_regression.rs`; added `argtuner.toml`
    (random sampler, fixed 5 trials, `lr`/`steps` space, `metric_key = "loss"`) + README.
  - `examples/guitar_tuning/` — unchanged.
  - `examples/interactive_probe/` — converted from `crates/tooling/interactive-probe` (src/main.rs,
    probe-tuning-project/argtuner.toml, README); template rewritten to
    `cargo run -p argtuner --example interactive_probe ...`.
  - `examples/loss_pattern_generator/` — converted from `crates/tooling/loss-pattern-generator`
    (main.rs + patterns.rs, loss-tuning-project/argtuner.toml + README); template rewritten to
    `cargo run -p argtuner --example loss_pattern_generator ...`.
  - Deleted `crates/tooling/` entirely.
- **Cargo.toml**: removed the two tooling crates from `[workspace] members`; added
  `indicatif.workspace = true` to `[dev-dependencies]` (needed by the loss-pattern example).
- **`watch` requires `--project`**: `Commands::Watch.project` is now a non-`Option` `PathBuf`
  (implicitly required); `--db` removed; dispatch simplified to `Project::new(project).trials_db_path()`.
- **CLI tests**: added `#[cfg(test)] mod cli_tests` in src/main.rs (`watch_requires_project`,
  `watch_poll_ms_defaults_to_5000`) via `Cli::try_parse_from` (root `Parser`, avoiding the
  `Subcommand::command()` E0599).
- **READMEs**: root README Watch section drops `--db`, example sections updated for the new paths +
  two new examples.
- All green: 71 lib + 2 CLI tests + integration/example suites; clippy clean (only pre-existing
  talkback warnings); `cargo test --workspace --all-features --lib --bins --tests --examples` passes.

## AMENDMENT 20 — Gate the `h` (Custom(1)) mode toggle to built-in argtuner windows only

### Problem (user feedback)
The `h` key (bound to `TermWmAction::Custom(1)`, src/tui/mod.rs:713-714) toggles argtuner's
Metrics ↔ HyperParams chart mode in `AppState::handle_app_event` (src/tui/mod.rs:583-591). It fires
whenever the argtuner TUI owns the terminal, regardless of which window is focused. argtuner can
open real **terminal windows** via `wm_new_terminal` (src/tui/mod.rs:623-626 → term-wm
`TermWmApp::wm_new_terminal`, which creates `AppRootComponent::Core(CoreWmComponent::Terminal)`).
When such a terminal app is focused, pressing `h` should go to that app — but argtuner intercepts it
and flips the mode, interfering with the terminal app's input.

### Root cause (verified)
- argtuner's windows are `AppRootComponent::Custom(AppComponent::{Trials,Charts,Details,Params,Metrics})`
  (src/tui/mod.rs:318-331).
- Terminal windows (via `wm_new_terminal`) are `AppRootComponent::Core(CoreWmComponent::Terminal)`
  (term-wm src/components.rs:9-13, src/term_wm_app.rs:248).
- `AppState::handle_app_event` (src/tui/mod.rs:570-596) handles `Custom(1)` unconditionally; there is
  no focus check.

### Change (all in `/Volumes/2TB Storage Vault/rust-argtuner/src/tui/mod.rs`)
Gate the `Custom(1)` branch on the focused window being a built-in argtuner (`Custom`) window. Only
argtuner's own windows should respond to `h`; terminal (`Core`) windows must pass the key through.

```rust
fn handle_app_event(&mut self, event: &Event) -> bool {
    if let Event::Key(key) = event
        && key.kind == KeyKind::Press
    {
        let kb = self.inner.wm().keybindings();
        if kb.matches(TermWmAction::Quit, key) {
            self.open_exit_confirm();
            return true;
        }
        // `h` (Metrics ↔ HyperParams) only applies to argtuner's built-in
        // windows. When a terminal window is focused, let the key through so
        // the underlying app receives it.
        if kb.matches(TermWmAction::Custom(1), key)
            && self.is_argtuner_window_focused()
        {
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

/// True when the focused window is one of argtuner's built-in windows
/// (`AppRootComponent::Custom(...)`), i.e. NOT a terminal (`Core`) window.
fn is_argtuner_window_focused(&self) -> bool {
    let key = self.inner.wm().focused_window();
    matches!(
        self.inner.wm().component_for_key(key),
        Some(AppRootComponent::Custom(_))
    )
}
```

Notes:
- `WindowManager::focused_window()` returns the focused `WindowKey`; `component_for_key(&self, key)`
  (term-wm mod.rs:2277) is the immutable accessor already used by term-wm internally — no mutable
  borrow, so it composes with the existing `handle_app_event` flow.
- `Quit` (`q`) stays global (exiting the TUI from anywhere is fine and intended).
- No keybinding change: `h` remains the Metrics ↔ HyperParams toggle; it simply stops firing when a
  terminal window has focus.

### Files to modify
- `/Volumes/2TB Storage Vault/rust-argtuner/src/tui/mod.rs` — focus gate in `handle_app_event` +
  `is_argtuner_window_focused` helper.

### Verification
1. `cargo build` + `cargo clippy` + `cargo test` (71 lib + 2 CLI tests) — green.
2. Manual (argtuner Watch):
   - With a built-in window (Trials/Charts/Details) focused, press `h` → mode toggles (Metrics ↔
     HyperParams) as before.
   - Open a terminal window (`Ctrl+A` → New Window, or whatever `wm_new_terminal` is bound to);
     focus it and press `h` → the mode does NOT toggle; the terminal app receives `h` (e.g. a shell
     echoes nothing / a pager navigates), confirming the key is no longer intercepted.
   - Press `q` anywhere → still quits.
