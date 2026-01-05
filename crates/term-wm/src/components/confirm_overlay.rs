use crossterm::event::{Event, KeyCode};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use crate::components::{Component, DialogOverlay};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    Confirm,
    Cancel,
}

#[derive(Debug, Default)]
pub struct ConfirmOverlay {
    dialog: DialogOverlay,
    visible: bool,
    selected_confirm: bool,
}

impl ConfirmOverlay {
    pub fn new() -> Self {
        let mut dialog = DialogOverlay::new();
        dialog.set_size(56, 9);
        dialog.set_dim_backdrop(true);
        Self {
            dialog,
            visible: false,
            selected_confirm: true,
        }
    }

    pub fn open(&mut self, title: &str, body: &str) {
        self.dialog.set_title(title);
        self.dialog.set_body(body);
        self.dialog.set_visible(true);
        self.visible = true;
        self.selected_confirm = true;
    }

    pub fn close(&mut self) {
        self.dialog.set_visible(false);
        self.visible = false;
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn set_dim_backdrop(&mut self, dim: bool) {
        self.dialog.set_dim_backdrop(dim);
    }
}

impl Component for ConfirmOverlay {
    fn render(&mut self, frame: &mut Frame, area: Rect, _focused: bool) {
        if !self.visible || area.width == 0 || area.height == 0 {
            return;
        }
        self.dialog.render(frame, area, false);
        let rect = self.dialog.rect_for(area);
        if rect.width < 3 || rect.height < 3 {
            return;
        }
        let inner = Rect {
            x: rect.x.saturating_add(1),
            y: rect.y.saturating_add(1),
            width: rect.width.saturating_sub(2),
            height: rect.height.saturating_sub(2),
        };
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let button_y = inner.y.saturating_add(inner.height.saturating_sub(1));
        let cancel = "[ Cancel ]";
        let confirm = "[ Exit ]";
        let cancel_style = if self.selected_confirm {
            Style::default().fg(Color::White).bg(Color::DarkGray)
        } else {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Gray)
                .add_modifier(Modifier::BOLD)
        };
        let confirm_style = if self.selected_confirm {
            Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White).bg(Color::DarkGray)
        };
        let total_width = cancel.len() + 2 + confirm.len();
        let start_x = inner
            .x
            .saturating_add(inner.width.saturating_sub(total_width as u16) / 2);
        let buffer = frame.buffer_mut();
        buffer.set_string(start_x, button_y, cancel, cancel_style);
        buffer.set_string(
            start_x.saturating_add(cancel.len() as u16 + 2),
            button_y,
            confirm,
            confirm_style,
        );
    }

    fn handle_event(&mut self, event: &Event) -> bool {
        let Event::Key(key) = event else {
            return false;
        };
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') => true,
            KeyCode::Esc | KeyCode::Char('n') => true,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => true,
            _ => false,
        }
    }
}

impl ConfirmOverlay {
    pub fn handle_confirm_event(&mut self, event: &Event) -> Option<ConfirmAction> {
        let Event::Key(key) = event else {
            return None;
        };
        match key.code {
            KeyCode::Tab => {
                self.selected_confirm = !self.selected_confirm;
                None
            }
            KeyCode::BackTab => {
                self.selected_confirm = !self.selected_confirm;
                None
            }
            KeyCode::Left => {
                self.selected_confirm = false;
                None
            }
            KeyCode::Right => {
                self.selected_confirm = true;
                None
            }
            KeyCode::Enter | KeyCode::Char('y') => {
                if self.selected_confirm {
                    Some(ConfirmAction::Confirm)
                } else {
                    Some(ConfirmAction::Cancel)
                }
            }
            KeyCode::Esc | KeyCode::Char('n') => Some(ConfirmAction::Cancel),
            _ => None,
        }
    }
}
