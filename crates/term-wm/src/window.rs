use std::collections::BTreeMap;

use crossterm::event::{Event, KeyCode, MouseEvent, MouseEventKind};
use ratatui::prelude::Rect;

use crate::layout::{LayoutNode, LayoutPlan};
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
use ratatui::Frame;

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

pub fn render_scrollbar(
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

#[derive(Debug, Default)]
pub struct ScrollbarDrag {
    dragging: bool,
}

pub struct ScrollbarDragResponse {
    pub handled: bool,
    pub offset: Option<usize>,
}

impl ScrollbarDrag {
    pub fn new() -> Self {
        Self { dragging: false }
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    pub fn handle_mouse(
        &mut self,
        mouse: &MouseEvent,
        area: Rect,
        total: usize,
        view: usize,
    ) -> ScrollbarDragResponse {
        if total <= view || view == 0 || area.height == 0 || area.width == 0 {
            self.dragging = false;
            return ScrollbarDragResponse {
                handled: false,
                offset: None,
            };
        }
        let scrollbar_x = area.x.saturating_add(area.width.saturating_sub(1));
        let on_scrollbar = rect_contains(area, mouse.column, mouse.row) && mouse.column == scrollbar_x;
        match mouse.kind {
            MouseEventKind::Down(_) if on_scrollbar => {
                self.dragging = true;
                ScrollbarDragResponse {
                    handled: true,
                    offset: Some(scrollbar_offset_from_row(mouse.row, area, total, view)),
                }
            }
            MouseEventKind::Drag(_) if self.dragging => ScrollbarDragResponse {
                handled: true,
                offset: Some(scrollbar_offset_from_row(mouse.row, area, total, view)),
            },
            MouseEventKind::Up(_) if self.dragging => {
                self.dragging = false;
                ScrollbarDragResponse {
                    handled: true,
                    offset: None,
                }
            }
            _ => ScrollbarDragResponse {
                handled: false,
                offset: None,
            },
        }
    }
}

fn scrollbar_offset_from_row(row: u16, area: Rect, total: usize, view: usize) -> usize {
    let content_len = total.saturating_sub(view).saturating_add(1).max(1);
    let max_offset = content_len.saturating_sub(1);
    if max_offset == 0 || area.height <= 1 {
        return 0;
    }
    let rel = row.saturating_sub(area.y).min(area.height.saturating_sub(1));
    let ratio = rel as f64 / (area.height.saturating_sub(1)) as f64;
    (ratio * max_offset as f64).round() as usize
}

pub struct ScrollEvent {
    pub handled: bool,
    pub offset: Option<usize>,
}

#[derive(Debug)]
pub struct ScrollView {
    state: ScrollState,
    drag: ScrollbarDrag,
    last_area: Rect,
    last_total: usize,
    last_view: usize,
}

impl ScrollView {
    pub fn new() -> Self {
        Self {
            state: ScrollState::default(),
            drag: ScrollbarDrag::new(),
            last_area: Rect::default(),
            last_total: 0,
            last_view: 0,
        }
    }

    pub fn update(&mut self, area: Rect, total: usize, view: usize) {
        self.last_area = area;
        self.last_total = total;
        self.last_view = view;
        self.state.apply(total, view);
    }

    pub fn area(&self) -> Rect {
        self.last_area
    }

    pub fn total(&self) -> usize {
        self.last_total
    }

    pub fn view(&self) -> usize {
        self.last_view
    }

    pub fn set_total_view(&mut self, total: usize, view: usize) {
        self.last_total = total;
        self.last_view = view;
        self.state.apply(total, view);
    }

    pub fn offset(&self) -> usize {
        self.state.offset
    }

    pub fn set_offset(&mut self, offset: usize) {
        self.state.offset = offset.min(self.max_offset());
    }

    pub fn bump(&mut self, delta: isize) {
        self.state.bump(delta);
        self.state.apply(self.last_total, self.last_view);
    }

    pub fn handle_event(&mut self, event: &Event) -> ScrollEvent {
        if self.last_total == 0 || self.last_view == 0 {
            return ScrollEvent {
                handled: false,
                offset: None,
            };
        }
        let Event::Mouse(mouse) = event else {
            return ScrollEvent {
                handled: false,
                offset: None,
            };
        };
        let response = self
            .drag
            .handle_mouse(mouse, self.last_area, self.last_total, self.last_view);
        if let Some(offset) = response.offset {
            self.set_offset(offset);
        }
        ScrollEvent {
            handled: response.handled,
            offset: response.offset,
        }
    }

    fn max_offset(&self) -> usize {
        self.last_total.saturating_sub(self.last_view)
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
}

impl<W: Copy + Eq + Ord, R: Copy + Eq + Ord> WindowManager<W, R> {
    pub fn new(current: W) -> Self {
        Self {
            focus: FocusRing::new(current),
            regions: RegionMap::default(),
            scroll: BTreeMap::new(),
        }
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

    pub fn draw_scrollbar(
        &self,
        id: W,
        frame: &mut Frame,
        area: Rect,
        total: usize,
        view: usize,
    ) {
        render_scrollbar(frame, area, total, view, self.scroll_offset(id));
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
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(_) => {
                    if let Some(hit) = self.hit_test_region(mouse.column, mouse.row, hit_targets) {
                        self.set_focus(map(hit));
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            },
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
