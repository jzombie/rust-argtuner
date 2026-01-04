use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use crate::window::CaptureStatus;

#[derive(Debug, Clone)]
pub struct CaptureBadge {
    status: CaptureStatus,
}

impl CaptureBadge {
    pub fn new() -> Self {
        Self {
            status: CaptureStatus::None,
        }
    }

    pub fn set_status(&mut self, status: CaptureStatus) {
        self.status = status;
    }
}

impl super::Component for CaptureBadge {
    fn render(&mut self, frame: &mut Frame, area: Rect, _focused: bool) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let (label, style) = match self.status {
            CaptureStatus::Active => (
                " WM ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            CaptureStatus::Pending => (
                " ESC ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightYellow)
                    .add_modifier(Modifier::BOLD),
            ),
            CaptureStatus::None => return,
        };
        frame.buffer_mut().set_string(area.x, area.y, label, style);
    }
}
