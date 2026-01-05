use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, MouseEventKind};
use ratatui::prelude::Rect;

use crate::components::{Component, DialogOverlay};
use crate::layout::{
    FloatingPane, LayoutNode, LayoutPlan, RectSpec, SplitHandle, TilingLayout, render_handles,
};

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

#[derive(Debug)]
pub struct WindowManager<W: Copy + Eq + Ord, R: Copy + Eq + Ord> {
    focus: FocusRing<W>,
    regions: RegionMap<R>,
    scroll: BTreeMap<W, ScrollState>,
    handles: Vec<SplitHandle>,
    tab_handles: Vec<TabHandle<R>>,
    managed_draw_order: Vec<R>,
    managed_layout: Option<TilingLayout<R>>,
    managed_floating: Vec<FloatingPane<R>>,
    managed_area: Rect,
    drag_tab: Option<TabDrag<R>>,
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
            tab_handles: Vec::new(),
            managed_draw_order: Vec::new(),
            managed_layout: None,
            managed_floating: Vec::new(),
            managed_area: Rect::default(),
            drag_tab: None,
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
        self.tab_handles.clear();
        self.managed_draw_order.clear();
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

    pub fn bring_focus_to_front<F>(&mut self, map_focus: F)
    where
        F: Fn(W) -> Option<R>,
    {
        if self.layout_contract != LayoutContract::WindowManaged {
            return;
        }
        if let Some(region) = map_focus(self.focus.current()) {
            self.bring_floating_to_front(region);
        }
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

    pub fn full_region(&self, id: R) -> Rect {
        self.regions.get(id).unwrap_or_default()
    }

    pub fn region(&self, id: R) -> Rect {
        let rect = self.regions.get(id).unwrap_or_default();
        if self.layout_contract == LayoutContract::WindowManaged {
            clamp_rect(rect, self.managed_area)
        } else {
            rect
        }
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

    pub fn set_managed_layout(&mut self, layout: TilingLayout<R>) {
        self.managed_layout = Some(layout);
        self.managed_floating.clear();
    }

    pub fn register_managed_layout(&mut self, area: Rect) {
        self.managed_area = area;
        if let Some(layout) = self.managed_layout.as_ref() {
            let (regions, handles) = layout.root().layout_with_handles(area);
            for (id, rect) in &regions {
                self.regions.set(*id, *rect);
                if let Some(tab) = tab_handle_for_region(*id, *rect) {
                    self.tab_handles.push(tab);
                }
                self.managed_draw_order.push(*id);
            }
            self.handles.extend(handles);
        }
        for floating in &self.managed_floating {
            let rect = floating.rect.resolve(area);
            self.regions.set(floating.id, rect);
            if let Some(tab) = tab_handle_for_region(floating.id, rect) {
                self.tab_handles.push(tab);
            }
            self.managed_draw_order.push(floating.id);
        }
    }

    pub fn managed_draw_order(&self) -> &[R] {
        &self.managed_draw_order
    }

    pub fn handle_managed_event(&mut self, event: &Event) -> bool {
        if self.layout_contract != LayoutContract::WindowManaged {
            return false;
        }
        if self.handle_tab_drag_event(event) {
            return true;
        }
        if let Some(layout) = self.managed_layout.as_mut() {
            return layout.handle_event(event, self.managed_area);
        }
        false
    }

    fn handle_tab_drag_event(&mut self, event: &Event) -> bool {
        use crossterm::event::MouseEventKind;
        let Event::Mouse(mouse) = event else {
            return false;
        };
        match mouse.kind {
            MouseEventKind::Down(_) => {
                let hit_id = self
                    .tab_handles
                    .iter()
                    .find(|tab| rect_contains(tab.rect, mouse.column, mouse.row))
                    .map(|tab| tab.id);
                if let Some(id) = hit_id {
                    let rect = self.region(id);
                    if self.floating_index(id).is_none() {
                        let _ = self.detach_to_floating(id, rect);
                    } else {
                        self.bring_floating_to_front(id);
                    }
                    self.drag_tab = Some(TabDrag {
                        id,
                        offset_x: mouse.column.saturating_sub(rect.x),
                        offset_y: mouse.row.saturating_sub(rect.y),
                    });
                    return true;
                }
            }
            MouseEventKind::Drag(_) => {
                if let Some(drag) = self.drag_tab {
                    if let Some(index) = self.floating_index(drag.id) {
                        self.move_floating(
                            index,
                            mouse.column,
                            mouse.row,
                            drag.offset_x,
                            drag.offset_y,
                        );
                    }
                    return true;
                }
            }
            MouseEventKind::Up(_) => {
                if self.drag_tab.take().is_some() {
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    fn detach_to_floating(&mut self, id: R, rect: Rect) -> bool {
        if self.floating_index(id).is_some() {
            return true;
        }
        if let Some(layout) = self.managed_layout.as_ref() {
            if matches!(layout.root(), LayoutNode::Leaf(leaf) if *leaf == id) {
                self.managed_layout = None;
            } else {
                let Some(layout) = self.managed_layout.as_mut() else {
                    return false;
                };
                if !layout.root_mut().remove_leaf(id) {
                    return false;
                }
            }
        } else {
            return false;
        }
        let width = rect.width.max(1);
        let height = rect.height.max(1);
        let x = rect.x;
        let y = rect.y;
        self.managed_floating.push(FloatingPane {
            id,
            rect: RectSpec::Absolute(Rect {
                x,
                y,
                width,
                height,
            }),
        });
        true
    }

    fn floating_index(&self, id: R) -> Option<usize> {
        self.managed_floating.iter().position(|pane| pane.id == id)
    }

    fn move_floating(&mut self, index: usize, column: u16, row: u16, offset_x: u16, offset_y: u16) {
        let pane = &mut self.managed_floating[index];
        let RectSpec::Absolute(rect) = pane.rect else {
            return;
        };
        let width = rect.width.max(1);
        let height = rect.height.max(1);
        let x = column.saturating_sub(offset_x);
        let y = row.saturating_sub(offset_y);
        pane.rect = RectSpec::Absolute(Rect {
            x,
            y,
            width,
            height,
        });
    }

    fn bring_floating_to_front(&mut self, id: R) {
        if let Some(index) = self.floating_index(id) {
            let pane = self.managed_floating.remove(index);
            self.managed_floating.push(pane);
        }
    }

    pub fn render_overlays(&mut self, frame: &mut ratatui::Frame) {
        let hovered = self.hover.and_then(|(column, row)| {
            self.handles
                .iter()
                .find(|handle| rect_contains(handle.rect, column, row))
        });
        let hovered_tab = self
            .hover
            .and_then(|(column, row)| {
                self.tab_handles
                    .iter()
                    .find(|tab| rect_contains(tab.rect, column, row))
            })
            .map(|tab| tab.id);
        render_handles(frame, &self.handles, hovered);
        render_tab_handles(
            frame,
            &self.tab_handles,
            self.drag_tab.as_ref(),
            hovered_tab,
        );
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
                "Window manager mode (placeholder)\n\n- Esc: dismiss overlay\n- n: new window\n- Esc (quick double): send to app\n- Esc passthrough: {remaining_ms} {bar}\n- Tab/Shift-Tab: cycle focus\n- More commands coming soon"
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

    /// Hit-test regions by draw order so overlapping panes pick the topmost one.
    /// This avoids clicks "falling through" floating panes to windows behind them.
    fn hit_test_region_topmost(&self, column: u16, row: u16, ids: &[R]) -> Option<R> {
        for id in ids.iter().rev() {
            if let Some(rect) = self.regions.get(*id) {
                if rect_contains(rect, column, row) {
                    return Some(*id);
                }
            }
        }
        None
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
                        let hit = if self.layout_contract == LayoutContract::WindowManaged
                            && !self.managed_draw_order.is_empty()
                        {
                            self.hit_test_region_topmost(
                                mouse.column,
                                mouse.row,
                                &self.managed_draw_order,
                            )
                        } else {
                            self.hit_test_region(mouse.column, mouse.row, hit_targets)
                        };
                        if let Some(hit) = hit {
                            self.set_focus(map(hit));
                            if self.layout_contract == LayoutContract::WindowManaged {
                                self.bring_floating_to_front(hit);
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
}

#[derive(Debug, Clone, Copy)]
struct TabHandle<R: Copy + Eq + Ord> {
    id: R,
    rect: Rect,
}

#[derive(Debug, Clone, Copy)]
struct TabDrag<R: Copy + Eq + Ord> {
    id: R,
    offset_x: u16,
    offset_y: u16,
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

fn tab_handle_for_region<R: Copy + Eq + Ord>(id: R, rect: Rect) -> Option<TabHandle<R>> {
    if rect.width < 6 || rect.height == 0 {
        return None;
    }
    let width = rect.width.min(14);
    Some(TabHandle {
        id,
        rect: Rect {
            x: rect.x.saturating_add(1),
            y: rect.y,
            width,
            height: 1,
        },
    })
}

fn render_tab_handles<R: Copy + Eq + Ord>(
    frame: &mut ratatui::Frame,
    tabs: &[TabHandle<R>],
    drag: Option<&TabDrag<R>>,
    hovered: Option<R>,
) {
    use ratatui::style::{Color, Style};
    let buffer = frame.buffer_mut();
    for tab in tabs {
        let is_drag = drag.is_some_and(|active| active.id == tab.id);
        if !is_drag && hovered != Some(tab.id) {
            continue;
        }
        let style = if is_drag {
            Style::default().fg(Color::Black).bg(Color::LightYellow)
        } else {
            Style::default().fg(Color::Black).bg(Color::DarkGray)
        };
        for dx in 0..tab.rect.width {
            if let Some(cell) = buffer.cell_mut((tab.rect.x + dx, tab.rect.y)) {
                cell.set_symbol(" ").set_style(style);
            }
        }
        if tab.rect.width >= 3 {
            let label = "tab";
            for (idx, ch) in label.chars().enumerate() {
                let x = tab.rect.x.saturating_add(1 + idx as u16);
                if x >= tab.rect.x.saturating_add(tab.rect.width) {
                    break;
                }
                if let Some(cell) = buffer.cell_mut((x, tab.rect.y)) {
                    cell.set_symbol(&ch.to_string()).set_style(style);
                }
            }
        }
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

fn clamp_rect(area: Rect, bounds: Rect) -> Rect {
    let x0 = area.x.max(bounds.x);
    let y0 = area.y.max(bounds.y);
    let x1 = area
        .x
        .saturating_add(area.width)
        .min(bounds.x.saturating_add(bounds.width));
    let y1 = area
        .y
        .saturating_add(area.height)
        .min(bounds.y.saturating_add(bounds.height));
    if x1 <= x0 || y1 <= y0 {
        return Rect::default();
    }
    Rect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}
