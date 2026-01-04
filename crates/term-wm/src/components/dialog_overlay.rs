use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

#[derive(Debug, Clone)]
pub struct DialogOverlay {
    title: String,
    body: String,
    visible: bool,
    width: u16,
    height: u16,
    bg: Color,
}

impl DialogOverlay {
    pub fn new() -> Self {
        Self {
            title: "Dialog".to_string(),
            body: String::new(),
            visible: false,
            width: 70,
            height: 9,
            bg: Color::Black,
        }
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    pub fn set_title(&mut self, title: impl Into<String>) {
        self.title = title.into();
    }

    pub fn set_body(&mut self, body: impl Into<String>) {
        self.body = body.into();
    }

    pub fn set_size(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
    }

    pub fn set_bg(&mut self, bg: Color) {
        self.bg = bg;
    }
}

impl super::Component for DialogOverlay {
    fn render(&mut self, frame: &mut Frame, area: Rect, _focused: bool) {
        if !self.visible || area.width == 0 || area.height == 0 {
            return;
        }
        let width = area.width.min(self.width).max(24);
        let height = area.height.min(self.height).max(5);
        let x = area
            .x
            .saturating_add(area.width.saturating_sub(width) / 2);
        let y = area
            .y
            .saturating_add(area.height.saturating_sub(height) / 2);
        let rect = Rect { x, y, width, height };
        frame.render_widget(Clear, rect);
        let block = Block::default().title(self.title.as_str()).borders(Borders::ALL);
        let paragraph = Paragraph::new(self.body.as_str())
            .style(Style::default().bg(self.bg))
            .block(block)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });
        frame.render_widget(paragraph, rect);
    }
}
