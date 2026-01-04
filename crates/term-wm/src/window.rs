use std::collections::BTreeMap;

use crossterm::event::{Event, KeyCode, MouseEventKind};
use ratatui::prelude::Rect;

use crate::layout::{render_handles, LayoutNode, LayoutPlan, SplitHandle, TilingLayout};

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
}

impl<W: Copy + Eq + Ord, R: Copy + Eq + Ord> WindowManager<W, R> {
    pub fn new(current: W) -> Self {
        Self {
            focus: FocusRing::new(current),
            regions: RegionMap::default(),
            scroll: BTreeMap::new(),
            handles: Vec::new(),
            hover: None,
        }
    }

    pub fn begin_frame(&mut self) {
        self.regions = RegionMap::default();
        self.handles.clear();
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

    pub fn render_overlays(&self, frame: &mut ratatui::Frame) {
        let hovered = self
            .hover
            .and_then(|(column, row)| {
                self.handles
                    .iter()
                    .find(|handle| rect_contains(handle.rect, column, row))
            });
        render_handles(frame, &self.handles, hovered);
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
