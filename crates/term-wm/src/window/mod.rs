pub mod decorator;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, MouseEventKind};
use ratatui::prelude::Rect;

use self::decorator::{OpenStepDecorator, WindowDecorator};
use crate::components::{Component, ConfirmAction, ConfirmOverlay, DialogOverlay};
use crate::layout::floating::*;
use crate::layout::{
    FloatingPane, LayoutNode, LayoutPlan, RectSpec, RegionMap, SplitHandle, TilingLayout,
    rect_contains, render_handles_masked,
};
use crate::panel::Panel;

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
    panel: Panel<R>,
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
    wm_menu_selected: usize,
    exit_confirm: ConfirmOverlay,
    decorator: Box<dyn WindowDecorator>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WmMenuAction {
    CloseMenu,
    NewWindow,
    ExitUi,
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
            panel: Panel::new(),
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
            wm_menu_selected: 0,
            exit_confirm: ConfirmOverlay::new(),
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
        self.panel.begin_frame();
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
        self.wm_menu_selected = 0;
    }

    pub fn close_wm_overlay(&mut self) {
        self.wm_overlay_visible = false;
        self.wm_overlay_opened_at = None;
        self.wm_overlay.set_visible(false);
    }

    pub fn open_exit_confirm(&mut self) {
        self.exit_confirm.open(
            "Exit App",
            "Exit the application?\n\nUnsaved changes will be lost.\n\nEnter: confirm  Esc: cancel\nTab/Shift-Tab/Arrows: switch",
        );
    }

    pub fn close_exit_confirm(&mut self) {
        self.exit_confirm.close();
    }

    pub fn exit_confirm_visible(&self) -> bool {
        self.exit_confirm.visible()
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
        self.panel.set_visible(visible);
    }

    pub fn set_panel_height(&mut self, height: u16) {
        self.panel.set_height(height);
    }

    pub fn register_managed_layout(&mut self, area: Rect) {
        let (_, managed_area) = self.panel.split_area(self.panel_active(), area);
        self.managed_area = managed_area;
        self.clamp_floating_to_bounds();
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
            self.resize_handles.extend(resize_handles_for_region(
                floating.id,
                rect,
                self.managed_area,
            ));
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
        if let Event::Mouse(mouse) = event
            && self.panel_active()
            && rect_contains(self.panel.area(), mouse.column, mouse.row)
        {
            if self.panel.hit_test_menu(event) {
                if self.wm_overlay_visible {
                    self.close_wm_overlay();
                } else {
                    self.open_wm_overlay();
                }
            } else if let Some(id) = self.panel.hit_test_window(event)
                && let Some(target) = self.focus_for_region(id)
            {
                self.set_focus(target);
                self.bring_floating_to_front(id);
            }
            return true;
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
        let x = column.saturating_sub(offset_x);
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

    fn clamp_floating_to_bounds(&mut self) {
        let bounds = self.managed_area;
        if bounds.width == 0 || bounds.height == 0 {
            return;
        }
        for pane in &mut self.managed_floating {
            let RectSpec::Absolute(rect) = pane.rect else {
                continue;
            };
            if rects_intersect(rect, bounds) {
                continue;
            }
            // Only recover panes that are fully off-screen; keep normal dragging untouched.
            let rect_right = rect.x.saturating_add(rect.width);
            let rect_bottom = rect.y.saturating_add(rect.height);
            let bounds_right = bounds.x.saturating_add(bounds.width);
            let bounds_bottom = bounds.y.saturating_add(bounds.height);
            // Clamp only the axis that is fully outside the viewport.
            let out_x = rect_right <= bounds.x || rect.x >= bounds_right;
            let out_y = rect_bottom <= bounds.y || rect.y >= bounds_bottom;
            let min_w = FLOATING_MIN_WIDTH.min(bounds.width.max(1));
            let min_h = FLOATING_MIN_HEIGHT.min(bounds.height.max(1));
            let width = rect.width.max(min_w).min(bounds.width);
            let height = rect.height.max(min_h).min(bounds.height);
            let max_x = bounds.x.saturating_add(bounds.width.saturating_sub(width));
            let max_y = bounds
                .y
                .saturating_add(bounds.height.saturating_sub(height));
            let x = if out_x {
                rect.x.clamp(bounds.x, max_x)
            } else {
                rect.x
            };
            let y = if out_y {
                rect.y.clamp(bounds.y, max_y)
            } else {
                rect.y
            };
            pane.rect = RectSpec::Absolute(Rect {
                x,
                y,
                width,
                height,
            });
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
        let obscuring: Vec<Rect> = self
            .managed_draw_order
            .iter()
            .filter_map(|&id| self.regions.get(id))
            .collect();
        let is_obscured =
            |x: u16, y: u16| -> bool { obscuring.iter().any(|r| rect_contains(*r, x, y)) };
        render_handles_masked(frame, &self.handles, hovered, is_obscured);
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
        let status_line = if self.wm_overlay_visible {
            let esc_state = if let Some(remaining) = self.esc_passthrough_remaining() {
                format!("Esc passthrough: active ({}ms)", remaining.as_millis())
            } else {
                "Esc passthrough: inactive".to_string()
            };
            Some(format!("{esc_state} · Tab/Shift-Tab: cycle windows"))
        } else {
            None
        };
        self.panel.render(
            frame,
            self.panel_active(),
            self.focus.current,
            &self.focus.order,
            &self.managed_draw_order,
            status_line.as_deref(),
        );
        let menu_labels = wm_menu_items()
            .iter()
            .map(|item| (item.icon, item.label))
            .collect::<Vec<_>>();
        let bounds = frame.area();
        self.panel.render_menu(
            frame,
            self.wm_overlay_visible,
            bounds,
            &menu_labels,
            self.wm_menu_selected,
        );
        self.panel.render_menu_backdrop(
            frame,
            self.wm_overlay_visible,
            self.managed_area,
            self.panel.area(),
        );
        if self.exit_confirm.visible() {
            self.exit_confirm.render(frame, frame.area(), false);
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
            && self.panel.visible()
            && self.panel.height() > 0
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

    pub fn handle_wm_menu_event(&mut self, event: &Event) -> Option<WmMenuAction> {
        if !self.wm_overlay_visible {
            return None;
        }
        if let Event::Mouse(mouse) = event
            && matches!(mouse.kind, MouseEventKind::Down(_))
        {
            if let Some(index) = self.panel.hit_test_menu_item(event) {
                self.wm_menu_selected = index.min(wm_menu_items().len().saturating_sub(1));
                return wm_menu_items()
                    .get(self.wm_menu_selected)
                    .map(|item| item.action);
            }
            if self.panel.menu_icon_contains_point(mouse.column, mouse.row) {
                return Some(WmMenuAction::CloseMenu);
            }
            if !self.panel.menu_contains_point(mouse.column, mouse.row) {
                return Some(WmMenuAction::CloseMenu);
            }
        }
        let Event::Key(key) = event else {
            return None;
        };
        match key.code {
            KeyCode::Up => {
                let total = wm_menu_items().len();
                if total > 0 {
                    if self.wm_menu_selected == 0 {
                        self.wm_menu_selected = total - 1;
                    } else {
                        self.wm_menu_selected -= 1;
                    }
                }
                None
            }
            KeyCode::Down => {
                let total = wm_menu_items().len();
                if total > 0 {
                    self.wm_menu_selected = (self.wm_menu_selected + 1) % total;
                }
                None
            }
            KeyCode::Char('k') => {
                let total = wm_menu_items().len();
                if total > 0 {
                    if self.wm_menu_selected == 0 {
                        self.wm_menu_selected = total - 1;
                    } else {
                        self.wm_menu_selected -= 1;
                    }
                }
                None
            }
            KeyCode::Char('j') => {
                let total = wm_menu_items().len();
                if total > 0 {
                    self.wm_menu_selected = (self.wm_menu_selected + 1) % total;
                }
                None
            }
            KeyCode::Enter => wm_menu_items()
                .get(self.wm_menu_selected)
                .map(|item| item.action),
            _ => None,
        }
    }

    pub fn handle_exit_confirm_event(&mut self, event: &Event) -> Option<ConfirmAction> {
        if !self.exit_confirm.visible() {
            return None;
        }
        self.exit_confirm.handle_confirm_event(event)
    }

    pub fn wm_menu_consumes_event(&self, event: &Event) -> bool {
        if !self.wm_overlay_visible {
            return false;
        }
        let Event::Key(key) = event else {
            return false;
        };
        matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Char('j') | KeyCode::Char('k')
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct WmMenuItem {
    label: &'static str,
    icon: Option<&'static str>,
    action: WmMenuAction,
}

fn wm_menu_items() -> [WmMenuItem; 3] {
    [
        WmMenuItem {
            label: "Resume",
            icon: None,
            action: WmMenuAction::CloseMenu,
        },
        WmMenuItem {
            label: "New Window",
            icon: Some("+"),
            action: WmMenuAction::NewWindow,
        },
        WmMenuItem {
            label: "Exit UI",
            icon: Some("⏻"),
            action: WmMenuAction::ExitUi,
        },
    ]
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

fn rects_intersect(a: Rect, b: Rect) -> bool {
    if a.width == 0 || a.height == 0 || b.width == 0 || b.height == 0 {
        return false;
    }
    let a_right = a.x.saturating_add(a.width);
    let a_bottom = a.y.saturating_add(a.height);
    let b_right = b.x.saturating_add(b.width);
    let b_bottom = b.y.saturating_add(b.height);
    a.x < b_right && a_right > b.x && a.y < b_bottom && a_bottom > b.y
}
