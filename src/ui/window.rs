use std::collections::BTreeMap;

use ratatui::prelude::Rect;
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

#[derive(Debug, Clone)]
pub struct FocusRing<T: Copy + Eq> {
    order: Vec<T>,
    current: T,
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

    pub fn set_region(&mut self, id: R, rect: Rect) {
        self.regions.set(id, rect);
    }

    pub fn region(&self, id: R) -> Rect {
        self.regions.get(id).unwrap_or_default()
    }
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
}

pub fn rect_contains(rect: Rect, column: u16, row: u16) -> bool {
    if rect.width == 0 || rect.height == 0 {
        return false;
    }
    let max_x = rect.x.saturating_add(rect.width);
    let max_y = rect.y.saturating_add(rect.height);
    column >= rect.x && column < max_x && row >= rect.y && row < max_y
}
