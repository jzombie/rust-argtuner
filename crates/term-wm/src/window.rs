use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, MouseEventKind};
use ratatui::prelude::Rect;

use crate::components::{Component, DialogOverlay};
use crate::layout::{render_handles, LayoutNode, LayoutPlan, SplitHandle, TilingLayout};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Describes who owns layout placement and how WM-level input is handled.
///
/// - AppManaged: the app owns regions; `Esc` passes through.
/// - WindowManaged: the WM owns layout; `Esc` enters WM mode/overlay.
pub enum LayoutContract {
    AppManaged,
    WindowManaged,
}

#[derive(Debug, Clone, Copy)]
pub struct ScrollState {
    pub offset: usize,
    pending: isize,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            offset: 0,
            pending: 0,
        }
    }
}

impl ScrollState {
    pub fn reset(&mut self) {
        self.offset = 0;
        self.pending = 0;
    }

    pub fn bump(&mut self, delta: isize) {
        self.pending = self.pending.saturating_add(delta);
    }

    pub fn apply(&mut self, total: usize, view: usize) {
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


#[derive(Debug, Clone)]
pub struct FocusRing<T: Copy + Eq> {
    order: Vec<T>,
    current: T,
}

impl<T: Copy + Eq> FocusRing<T> {
    pub fn new(current: T) -> Self {
        Self {
            order: Vec::new(),
            current,
        }
    }

    pub fn set_order(&mut self, order: Vec<T>) {
        self.order = order;
    }

    pub fn current(&self) -> T {
        self.current
    }

    pub fn set_current(&mut self, current: T) {
        self.current = current;
    }

    pub fn advance(&mut self, forward: bool) {
        if self.order.is_empty() {
            return;
        }
        let idx = self
            .order
            .iter()
            .position(|item| *item == self.current)
            .unwrap_or(0);
        let step = if forward { 1isize } else { -1isize };
        let next = ((idx as isize + step).rem_euclid(self.order.len() as isize)) as usize;
        self.current = self.order[next];
    }
}

#[derive(Debug, Clone)]
pub struct WindowManager<W: Copy + Eq + Ord, R: Copy + Eq + Ord> {
    focus: FocusRing<W>,
    regions: RegionMap<R>,
    scroll: BTreeMap<W, ScrollState>,
    handles: Vec<SplitHandle>,
    hover: Option<(u16, u16)>,
    capture_deadline: Option<Instant>,
    pending_deadline: Option<Instant>,
    layout_contract: LayoutContract,
    wm_overlay_visible: bool,
    wm_overlay_opened_at: Option<Instant>,
    esc_passthrough_window: Duration,
    wm_overlay: DialogOverlay,
}

impl<W: Copy + Eq + Ord, R: Copy + Eq + Ord> WindowManager<W, R> {
    pub fn new(current: W) -> Self {
        Self {
            focus: FocusRing::new(current),
            regions: RegionMap::default(),
            scroll: BTreeMap::new(),
            handles: Vec::new(),
            hover: None,
            capture_deadline: None,
            pending_deadline: None,
            layout_contract: LayoutContract::AppManaged,
            wm_overlay_visible: false,
            wm_overlay_opened_at: None,
            esc_passthrough_window: Duration::from_millis(600),
            wm_overlay: DialogOverlay::new(),
        }
    }

    pub fn new_managed(current: W) -> Self {
        let mut manager = Self::new(current);
        manager.layout_contract = LayoutContract::WindowManaged;
        manager
    }

    pub fn set_layout_contract(&mut self, contract: LayoutContract) {
        self.layout_contract = contract;
    }

    pub fn layout_contract(&self) -> LayoutContract {
        self.layout_contract
    }

    pub fn begin_frame(&mut self) {
        self.regions = RegionMap::default();
        self.handles.clear();
        if self.layout_contract == LayoutContract::AppManaged {
            self.clear_capture();
        } else {
            // Refresh deadlines so overlay badges can expire without events.
            self.refresh_capture();
        }
    }

    pub fn arm_capture(&mut self, timeout: Duration) {
        self.capture_deadline = Some(Instant::now() + timeout);
        self.pending_deadline = None;
    }

    pub fn arm_pending(&mut self, timeout: Duration) {
        // Shows an "Esc pending" badge while waiting for the chord.
        self.pending_deadline = Some(Instant::now() + timeout);
    }

    pub fn clear_capture(&mut self) {
        self.capture_deadline = None;
        self.pending_deadline = None;
        self.wm_overlay_visible = false;
        self.wm_overlay_opened_at = None;
        self.wm_overlay.set_visible(false);
    }

    pub fn capture_active(&mut self) -> bool {
        if self.layout_contract == LayoutContract::WindowManaged && self.wm_overlay_visible {
            return true;
        }
        self.refresh_capture();
        self.capture_deadline.is_some()
    }

    fn refresh_capture(&mut self) {
        if let Some(deadline) = self.capture_deadline {
            if Instant::now() > deadline {
                self.capture_deadline = None;
            }
        }
        if let Some(deadline) = self.pending_deadline {
            if Instant::now() > deadline {
                self.pending_deadline = None;
            }
        }
    }

    pub fn open_wm_overlay(&mut self) {
        self.wm_overlay_visible = true;
        self.wm_overlay_opened_at = Some(Instant::now());
        self.wm_overlay.set_visible(true);
    }

    pub fn close_wm_overlay(&mut self) {
        self.wm_overlay_visible = false;
        self.wm_overlay_opened_at = None;
        self.wm_overlay.set_visible(false);
    }

    pub fn wm_overlay_visible(&self) -> bool {
        self.wm_overlay_visible
    }

    pub fn esc_passthrough_active(&self) -> bool {
        self.esc_passthrough_remaining().is_some()
    }

    pub fn esc_passthrough_remaining(&self) -> Option<Duration> {
        if !self.wm_overlay_visible {
            return None;
        }
        let opened_at = self.wm_overlay_opened_at?;
        let elapsed = opened_at.elapsed();
        if elapsed >= self.esc_passthrough_window {
            return None;
        }
        Some(self.esc_passthrough_window.saturating_sub(elapsed))
    }

    pub fn focus(&self) -> W {
        self.focus.current()
    }

    pub fn set_focus(&mut self, focus: W) {
        self.focus.set_current(focus);
    }

    pub fn set_focus_order(&mut self, order: Vec<W>) {
        self.focus.set_order(order);
        if !self.focus.order.is_empty()
            && !self
                .focus
                .order
                .iter()
                .any(|item| *item == self.focus.current)
        {
            self.focus.current = self.focus.order[0];
        }
    }

    pub fn advance_focus(&mut self, forward: bool) {
        self.focus.advance(forward);
    }

    pub fn scroll(&self, id: W) -> ScrollState {
        self.scroll.get(&id).copied().unwrap_or_default()
    }

    pub fn scroll_mut(&mut self, id: W) -> &mut ScrollState {
        self.scroll.entry(id).or_default()
    }

    pub fn scroll_offset(&self, id: W) -> usize {
        self.scroll(id).offset
    }

    pub fn reset_scroll(&mut self, id: W) {
        self.scroll_mut(id).reset();
    }

    pub fn apply_scroll(&mut self, id: W, total: usize, view: usize) {
        self.scroll_mut(id).apply(total, view);
    }

    pub fn set_region(&mut self, id: R, rect: Rect) {
        self.regions.set(id, rect);
    }

    pub fn region(&self, id: R) -> Rect {
        self.regions.get(id).unwrap_or_default()
    }

    pub fn set_regions_from_layout(&mut self, layout: &LayoutNode<R>, area: Rect) {
        self.regions = RegionMap::default();
        for (id, rect) in layout.layout(area) {
            self.regions.set(id, rect);
        }
    }

    pub fn register_tiling_layout(&mut self, layout: &TilingLayout<R>, area: Rect) {
        let (regions, handles) = layout.root().layout_with_handles(area);
        for (id, rect) in regions {
            self.regions.set(id, rect);
        }
        self.handles.extend(handles);
    }

    pub fn render_overlays(&mut self, frame: &mut ratatui::Frame) {
        let hovered = self
            .hover
            .and_then(|(column, row)| {
                self.handles
                    .iter()
                    .find(|handle| rect_contains(handle.rect, column, row))
            });
        render_handles(frame, &self.handles, hovered);
        if self.layout_contract == LayoutContract::WindowManaged && self.wm_overlay_visible {
            let (remaining_ms, bar) = if let Some(remaining) = self.esc_passthrough_remaining() {
                let total = self.esc_passthrough_window.as_millis().max(1);
                let remaining_ms = remaining.as_millis();
                let filled = ((remaining_ms * 10) / total) as usize;
                let filled = filled.min(10);
                let bar = format!("[{}{}]", "#".repeat(filled), "-".repeat(10 - filled));
                (format!("{remaining_ms}ms"), bar)
            } else {
                ("inactive".to_string(), "[----------]".to_string())
            };
            let text = format!(
                "Window manager mode (placeholder)\n\n- Esc: dismiss overlay\n- Esc (quick double): send to app\n- Esc passthrough: {remaining_ms} {bar}\n- Tab/Shift-Tab: cycle focus\n- More commands coming soon"
            );
            self.wm_overlay.set_title("Window Manager");
            self.wm_overlay.set_body(text);
            self.wm_overlay.render(frame, frame.area(), false);
        }
    }

    pub fn set_regions_from_plan(&mut self, plan: &LayoutPlan<R>, area: Rect) {
        self.regions = plan.regions(area);
    }

    pub fn hit_test_region(&self, column: u16, row: u16, ids: &[R]) -> Option<R> {
        self.regions.hit_test(column, row, ids)
    }

    pub fn handle_focus_event<F>(&mut self, event: &Event, hit_targets: &[R], map: F) -> bool
    where
        F: Fn(R) -> W,
    {
        match event {
            Event::Key(key) => match key.code {
                KeyCode::Tab => {
                    self.advance_focus(true);
                    true
                }
                KeyCode::BackTab => {
                    self.advance_focus(false);
                    true
                }
                _ => false,
            },
            Event::Mouse(mouse) => {
                self.hover = Some((mouse.column, mouse.row));
                match mouse.kind {
                MouseEventKind::Down(_) => {
                    if let Some(hit) = self.hit_test_region(mouse.column, mouse.row, hit_targets) {
                        self.set_focus(map(hit));
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            }}
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RegionMap<T: Copy + Eq + Ord> {
    regions: BTreeMap<T, Rect>,
}

impl<T: Copy + Eq + Ord> Default for RegionMap<T> {
    fn default() -> Self {
        Self {
            regions: BTreeMap::new(),
        }
    }
}

impl<T: Copy + Eq + Ord> RegionMap<T> {
    pub fn set(&mut self, id: T, rect: Rect) {
        self.regions.insert(id, rect);
    }

    pub fn get(&self, id: T) -> Option<Rect> {
        self.regions.get(&id).copied()
    }

    pub fn hit_test(&self, column: u16, row: u16, ids: &[T]) -> Option<T> {
        for id in ids {
            if let Some(rect) = self.regions.get(id) {
                if rect_contains(*rect, column, row) {
                    return Some(*id);
                }
            }
        }
        None
    }
}

pub fn rect_contains(rect: Rect, column: u16, row: u16) -> bool {
    if rect.width == 0 || rect.height == 0 {
        return false;
    }
    let max_x = rect.x.saturating_add(rect.width);
    let max_y = rect.y.saturating_add(rect.height);
    column >= rect.x && column < max_x && row >= rect.y && row < max_y
}
