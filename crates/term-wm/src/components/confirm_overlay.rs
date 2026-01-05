use crossterm::event::{Event, KeyCode};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::components::Component;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    Confirm,
    Cancel,
}

#[derive(Debug, Default)]
pub struct ConfirmOverlay {
    visible: bool,
    title: String,
    body: String,
    width: u16,
    height: u16,
    selected_confirm: bool,
}

impl ConfirmOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            title: "Confirm".to_string(),
            body: String::new(),
            width: 56,
            height: 9,
            selected_confirm: true,
        }
    }

    pub fn open(&mut self, title: &str, body: &str) {
        self.title = title.to_string();
        self.body = body.to_string();
        self.visible = true;
        self.selected_confirm = true;
    }

    pub fn close(&mut self) {
        self.visible = false;
    }

    pub fn visible(&self) -> bool {
        self.visible
    }
}

impl Component for ConfirmOverlay {
    fn render(&mut self, frame: &mut Frame, area: Rect, _focused: bool) {
        if !self.visible || area.width == 0 || area.height == 0 {
            return;
        }
        let width = area.width.min(self.width).max(28);
        let height = area.height.min(self.height).max(7);
        let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
        let y = area
            .y
            .saturating_add(area.height.saturating_sub(height) / 2);
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        frame.render_widget(Clear, rect);

        let block = Block::default()
            .title(self.title.as_str())
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Gray));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);

        if inner.height < 3 || inner.width == 0 {
            return;
        }

        let body_rect = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: inner.height.saturating_sub(2),
        };
        let paragraph = Paragraph::new(self.body.as_str())
            .style(Style::default().fg(Color::White))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });
        frame.render_widget(paragraph, body_rect);

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
