pub mod decorator;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, MouseEventKind};
use ratatui::prelude::Rect;
use ratatui::style::Style;

use self::decorator::{OpenStepDecorator, WindowDecorator};
use crate::components::{Component, DialogOverlay};
use crate::layout::floating::*;
use crate::layout::{
    FloatingPane, LayoutNode, LayoutPlan, RectSpec, RegionMap, SplitHandle, TilingLayout,
    rect_contains, render_handles,
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

#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollState {
    pub offset: usize,
    pending: isize,
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
    resize_handles: Vec<ResizeHandle<R>>,
    floating_headers: Vec<DragHandle<R>>,
    managed_draw_order: Vec<R>,
    managed_layout: Option<TilingLayout<R>>,
    managed_floating: Vec<FloatingPane<R>>,
    managed_area: Rect,
    panel_visible: bool,
    panel_height: u16,
    panel_area: Rect,
    panel_window_hits: Vec<PanelWindowHit<R>>,
    drag_header: Option<HeaderDrag<R>>,
    drag_resize: Option<ResizeDrag<R>>,
    hover: Option<(u16, u16)>,
    capture_deadline: Option<Instant>,
    pending_deadline: Option<Instant>,
    layout_contract: LayoutContract,
    wm_overlay_visible: bool,
    wm_overlay_opened_at: Option<Instant>,
    esc_passthrough_window: Duration,
    wm_overlay: DialogOverlay,
    decorator: Box<dyn WindowDecorator>,
}

#[derive(Debug, Clone, Copy)]
struct PanelWindowHit<R: Copy + Eq + Ord> {
    id: R,
    rect: Rect,
}

impl<W: Copy + Eq + Ord, R: Copy + Eq + Ord + std::fmt::Debug> WindowManager<W, R>
where
    R: PartialEq<W>,
{
    pub fn new(current: W) -> Self {
        Self {
            focus: FocusRing::new(current),
            regions: RegionMap::default(),
            scroll: BTreeMap::new(),
            handles: Vec::new(),
            resize_handles: Vec::new(),
            floating_headers: Vec::new(),
            managed_draw_order: Vec::new(),
            managed_layout: None,
            managed_floating: Vec::new(),
            managed_area: Rect::default(),
            panel_visible: true,
            panel_height: 1,
            panel_area: Rect::default(),
            panel_window_hits: Vec::new(),
            drag_header: None,
            drag_resize: None,
            hover: None,
            capture_deadline: None,
            pending_deadline: None,
            layout_contract: LayoutContract::AppManaged,
            wm_overlay_visible: false,
            wm_overlay_opened_at: None,
            esc_passthrough_window: esc_passthrough_window_default(),
            wm_overlay: DialogOverlay::new(),
            decorator: Box::new(OpenStepDecorator),
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
        self.resize_handles.clear();
        self.floating_headers.clear();
        self.managed_draw_order.clear();
        self.panel_window_hits.clear();
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
        if let Some(deadline) = self.capture_deadline
            && Instant::now() > deadline
        {
            self.capture_deadline = None;
        }
        if let Some(deadline) = self.pending_deadline
            && Instant::now() > deadline
        {
            self.pending_deadline = None;
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
        if !self.focus.order.is_empty() && !self.focus.order.contains(&self.focus.current) {
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
            let clamped = clamp_rect(rect, self.managed_area);
            if clamped.width < 3 || clamped.height < 4 {
                return Rect::default();
            }
            Rect {
                x: clamped.x + 1,
                y: clamped.y + 2,
                width: clamped.width.saturating_sub(2),
                height: clamped.height.saturating_sub(3),
            }
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

    pub fn set_panel_visible(&mut self, visible: bool) {
        self.panel_visible = visible;
    }

    pub fn set_panel_height(&mut self, height: u16) {
        self.panel_height = height.max(1);
    }

    pub fn register_managed_layout(&mut self, area: Rect) {
        let (panel_area, managed_area) = self.split_panel_area(area);
        self.panel_area = panel_area;
        self.managed_area = managed_area;
        if let Some(layout) = self.managed_layout.as_ref() {
            let (regions, handles) = layout.root().layout_with_handles(self.managed_area);
            for (id, rect) in &regions {
                if self.floating_index(*id).is_some() {
                    continue;
                }
                self.regions.set(*id, *rect);
                if let Some(header) = floating_header_for_region(*id, *rect, self.managed_area) {
                    self.floating_headers.push(header);
                }
                self.managed_draw_order.push(*id);
            }
            let filtered_handles: Vec<SplitHandle> = handles
                .into_iter()
                .filter(|handle| {
                    let Some(LayoutNode::Split { children, .. }) =
                        layout.root().node_at_path(&handle.path)
                    else {
                        return false;
                    };
                    let left = children.get(handle.index);
                    let right = children.get(handle.index + 1);
                    left.is_some_and(|node| {
                        node.subtree_any(|id| self.floating_index(id).is_none())
                    }) || right.is_some_and(|node| {
                        node.subtree_any(|id| self.floating_index(id).is_none())
                    })
                })
                .collect();
            self.handles.extend(filtered_handles);
        }
        for floating in &self.managed_floating {
            let rect = floating.rect.resolve(self.managed_area);
            self.regions.set(floating.id, rect);
            self.resize_handles
                .extend(resize_handles_for_region(floating.id, rect, self.managed_area));
            if let Some(header) = floating_header_for_region(floating.id, rect, self.managed_area) {
                self.floating_headers.push(header);
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
        if let Event::Mouse(mouse) = event {
            if self.panel_active() && rect_contains(self.panel_area, mouse.column, mouse.row) {
                return self.handle_panel_event(event);
            }
        }
        if let Event::Mouse(mouse) = event {
            self.hover = Some((mouse.column, mouse.row));
        }
        if self.handle_resize_event(event) {
            return true;
        }
        if self.handle_header_drag_event(event) {
            return true;
        }
        if let Some(layout) = self.managed_layout.as_mut() {
            return layout.handle_event(event, self.managed_area);
        }
        false
    }

    fn handle_header_drag_event(&mut self, event: &Event) -> bool {
        use crossterm::event::MouseEventKind;
        let Event::Mouse(mouse) = event else {
            return false;
        };
        match mouse.kind {
            MouseEventKind::Down(_) => {
                // Check if the mouse is blocked by a window above
                let topmost_hit = if self.layout_contract == LayoutContract::WindowManaged
                    && !self.managed_draw_order.is_empty()
                {
                    self.hit_test_region_topmost(mouse.column, mouse.row, &self.managed_draw_order)
                } else {
                    None
                };

                if let Some(header) = self
                    .floating_headers
                    .iter()
                    .rev()
                    .find(|handle| rect_contains(handle.rect, mouse.column, mouse.row))
                    .copied()
                {
                    // If we hit a window body that is NOT the owner of this header,
                    // then the header is obscured.
                    if let Some(hit_id) = topmost_hit
                        && hit_id != header.id
                    {
                        return false;
                    }

                    let rect = self.full_region(header.id);
                    if self.floating_index(header.id).is_none() {
                        let _ = self.detach_to_floating(header.id, rect);
                    } else {
                        self.bring_floating_to_front(header.id);
                    }
                    self.drag_header = Some(HeaderDrag {
                        id: header.id,
                        offset_x: mouse.column.saturating_sub(rect.x),
                        offset_y: mouse.row.saturating_sub(rect.y),
                    });
                    return true;
                }
            }
            MouseEventKind::Drag(_) => {
                if let Some(drag) = self.drag_header {
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
                if self.drag_header.take().is_some() {
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    fn handle_resize_event(&mut self, event: &Event) -> bool {
        use crossterm::event::MouseEventKind;
        let Event::Mouse(mouse) = event else {
            return false;
        };
        match mouse.kind {
            MouseEventKind::Down(_) => {
                // Check if the mouse is blocked by a window above
                let topmost_hit = if self.layout_contract == LayoutContract::WindowManaged
                    && !self.managed_draw_order.is_empty()
                {
                    self.hit_test_region_topmost(mouse.column, mouse.row, &self.managed_draw_order)
                } else {
                    None
                };

                let hit = self
                    .resize_handles
                    .iter()
                    .rev()
                    .find(|handle| rect_contains(handle.rect, mouse.column, mouse.row))
                    .copied();
                if let Some(handle) = hit {
                    // If we hit a window body that is NOT the owner of this handle,
                    // then the handle is obscured.
                    if let Some(hit_id) = topmost_hit
                        && hit_id != handle.id
                    {
                        return false;
                    }

                    let rect = self.full_region(handle.id);
                    if self.floating_index(handle.id).is_none() {
                        return false;
                    }
                    self.bring_floating_to_front(handle.id);
                    self.drag_resize = Some(ResizeDrag {
                        id: handle.id,
                        edge: handle.edge,
                        start_rect: rect,
                        start_col: mouse.column,
                        start_row: mouse.row,
                    });
                    return true;
                }
            }
            MouseEventKind::Drag(_) => {
                if let Some(drag) = self.drag_resize.as_ref()
                    && let Some(index) = self.floating_index(drag.id)
                {
                    let resized = apply_resize_drag(
                        drag.start_rect,
                        drag.edge,
                        mouse.column,
                        mouse.row,
                        drag.start_col,
                        drag.start_row,
                        self.managed_area,
                    );
                    self.managed_floating[index].rect = RectSpec::Absolute(resized);
                    return true;
                }
            }
            MouseEventKind::Up(_) => {
                if self.drag_resize.take().is_some() {
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
        if self.managed_layout.is_none() {
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
        let panel_active = self.panel_active();
        let bounds = self.managed_area;
        let pane = &mut self.managed_floating[index];
        let RectSpec::Absolute(rect) = pane.rect else {
            return;
        };
        let width = rect.width.max(1);
        let height = rect.height.max(1);
        let mut x = column.saturating_sub(offset_x);
        let mut y = row.saturating_sub(offset_y);
        if panel_active && y < bounds.y {
            y = bounds.y;
        }
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
        let hovered_resize = self.hover.and_then(|(column, row)| {
            self.resize_handles
                .iter()
                .find(|handle| rect_contains(handle.rect, column, row))
        });
        render_handles(frame, &self.handles, hovered);
        let focused = self.focus.current();

        for (i, &id) in self.managed_draw_order.iter().enumerate() {
            let Some(rect) = self.regions.get(id) else {
                continue;
            };
            if rect.width < 3 || rect.height < 3 {
                continue;
            }

            // Collect obscuring rects (windows above this one)
            let obscuring: Vec<Rect> = self.managed_draw_order[i + 1..]
                .iter()
                .filter_map(|&above_id| self.regions.get(above_id))
                .collect();

            let is_obscured =
                |x: u16, y: u16| -> bool { obscuring.iter().any(|r| rect_contains(*r, x, y)) };

            let title = format!("Window {:?}", id);
            self.decorator.render_window(
                frame,
                rect,
                self.managed_area,
                &title,
                id == focused,
                &is_obscured,
            );
        }

        render_resize_outline(
            frame,
            hovered_resize.map(|handle| handle.id),
            self.drag_resize.as_ref().map(|drag| drag.id),
            &self.regions,
            self.managed_area,
            &self.managed_floating,
            &self.managed_draw_order,
        );
        self.render_panel(frame);
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
            if let Some(rect) = self.regions.get(*id)
                && rect_contains(rect, column, row)
            {
                return Some(*id);
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

    fn panel_active(&self) -> bool {
        self.layout_contract == LayoutContract::WindowManaged
            && self.panel_visible
            && self.panel_height > 0
    }

    fn split_panel_area(&self, area: Rect) -> (Rect, Rect) {
        if !self.panel_active() {
            return (Rect::default(), area);
        }
        let height = self.panel_height.min(area.height);
        let panel = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height,
        };
        let managed = Rect {
            x: area.x,
            y: area.y.saturating_add(height),
            width: area.width,
            height: area.height.saturating_sub(height),
        };
        (panel, managed)
    }

    fn panel_order(&self) -> Vec<R> {
        if self.focus.order.is_empty() {
            return self.managed_draw_order.clone();
        }
        let mut ordered = Vec::new();
        for focus in &self.focus.order {
            if let Some(id) = self
                .managed_draw_order
                .iter()
                .copied()
                .find(|id| *id == *focus)
            {
                ordered.push(id);
            }
        }
        for id in &self.managed_draw_order {
            if !ordered.contains(id) {
                ordered.push(*id);
            }
        }
        ordered
    }

    fn render_panel(&mut self, frame: &mut ratatui::Frame) {
        if !self.panel_active() {
            return;
        }
        let area = self.panel_area;
        if area.width == 0 || area.height == 0 {
            return;
        }
        let buffer = frame.buffer_mut();
        let mut x = area.x;
        let y = area.y;
        let prefix = "Windows:";
        let prefix_width = prefix.chars().count() as u16;
        let max_x = area.x.saturating_add(area.width);
        if x.saturating_add(prefix_width) <= max_x {
            buffer.set_string(x, y, prefix, Style::default());
            x = x.saturating_add(prefix_width);
        }
        for id in self.panel_order() {
            let focused = id == self.focus.current;
            let chunk = if focused {
                format!(" [*{:?}]", id)
            } else {
                format!(" [{:?}]", id)
            };
            let chunk_width = chunk.chars().count() as u16;
            if x.saturating_add(chunk_width) > max_x {
                break;
            }
            buffer.set_string(x, y, &chunk, Style::default());
            self.panel_window_hits.push(PanelWindowHit {
                id,
                rect: Rect {
                    x,
                    y,
                    width: chunk_width,
                    height: 1,
                },
            });
            x = x.saturating_add(chunk_width);
        }
    }

    fn handle_panel_event(&mut self, event: &Event) -> bool {
        let Event::Mouse(mouse) = event else {
            return false;
        };
        if !matches!(mouse.kind, MouseEventKind::Down(_)) {
            return false;
        }
        if let Some(hit) = self
            .panel_window_hits
            .iter()
            .find(|hit| rect_contains(hit.rect, mouse.column, mouse.row))
            .copied()
        {
            if let Some(target) = self.focus_for_region(hit.id) {
                self.set_focus(target);
                self.bring_floating_to_front(hit.id);
            }
            return true;
        }
        true
    }

    fn focus_for_region(&self, id: R) -> Option<W> {
        if self.focus.order.is_empty() {
            if id == self.focus.current {
                Some(self.focus.current)
            } else {
                None
            }
        } else {
            self.focus.order.iter().copied().find(|focus| id == *focus)
        }
    }
}

fn esc_passthrough_window_default() -> Duration {
    #[cfg(windows)]
    {
        Duration::from_millis(1200)
    }
    #[cfg(not(windows))]
    {
        Duration::from_millis(600)
    }
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
