use crossterm::event::{Event, MouseEventKind};
use ratatui::{Frame, layout::Rect, style::Style};

use crate::layout::rect_contains;

#[derive(Debug, Clone, Copy)]
pub struct PanelWindowHit<R: Copy + Eq + Ord> {
    id: R,
    rect: Rect,
}

#[derive(Debug)]
pub struct Panel<R: Copy + Eq + Ord> {
    visible: bool,
    height: u16,
    area: Rect,
    window_hits: Vec<PanelWindowHit<R>>,
}

impl<R: Copy + Eq + Ord + std::fmt::Debug> Panel<R> {
    pub fn new() -> Self {
        Self {
            visible: true,
            height: 1,
            area: Rect::default(),
            window_hits: Vec::new(),
        }
    }

    pub fn begin_frame(&mut self) {
        self.window_hits.clear();
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn area(&self) -> Rect {
        self.area
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    pub fn set_height(&mut self, height: u16) {
        self.height = height.max(1);
    }

    pub fn split_area(&mut self, active: bool, area: Rect) -> (Rect, Rect) {
        if !active {
            self.area = Rect::default();
            return (Rect::default(), area);
        }
        let height = self.height.min(area.height);
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
        self.area = panel;
        (panel, managed)
    }

    pub fn render<W: Copy + Eq>(
        &mut self,
        frame: &mut Frame,
        active: bool,
        focus_current: W,
        focus_order: &[W],
        managed_draw_order: &[R],
    ) where
        R: PartialEq<W>,
    {
        if !active {
            return;
        }
        let area = self.area;
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
        for id in panel_order(focus_order, managed_draw_order) {
            let focused = id == focus_current;
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
            self.window_hits.push(PanelWindowHit {
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

    pub fn hit_test(&self, event: &Event) -> Option<R> {
        let Event::Mouse(mouse) = event else {
            return None;
        };
        if !matches!(mouse.kind, MouseEventKind::Down(_)) {
            return None;
        }
        self.window_hits
            .iter()
            .find(|hit| rect_contains(hit.rect, mouse.column, mouse.row))
            .map(|hit| hit.id)
    }
}

fn panel_order<W: Copy + Eq, R: Copy + Eq + Ord>(
    focus_order: &[W],
    managed_draw_order: &[R],
) -> Vec<R>
where
    R: PartialEq<W>,
{
    if focus_order.is_empty() {
        return managed_draw_order.to_vec();
    }
    let mut ordered = Vec::new();
    for focus in focus_order {
        if let Some(id) = managed_draw_order.iter().copied().find(|id| *id == *focus) {
            ordered.push(id);
        }
    }
    for id in managed_draw_order {
        if !ordered.contains(id) {
            ordered.push(*id);
        }
    }
    ordered
}
