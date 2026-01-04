use crossterm::event::{Event, KeyCode};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use crate::window::ScrollState;

pub struct ListComponent {
    items: Vec<String>,
    selected: usize,
    scroll: ScrollState,
    title: String,
}

impl ListComponent {
    pub fn new<T: Into<String>>(title: T) -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            scroll: ScrollState::default(),
            title: title.into(),
        }
    }

    pub fn set_items(&mut self, items: Vec<String>) {
        self.items = items;
        if self.selected >= self.items.len() {
            self.selected = self.items.len().saturating_sub(1);
        }
    }

    pub fn items(&self) -> &[String] {
        &self.items
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn set_selected(&mut self, selected: usize) {
        self.selected = selected.min(self.items.len().saturating_sub(1));
    }

    pub fn scroll_state(&self) -> ScrollState {
        self.scroll
    }

    pub fn scroll_state_mut(&mut self) -> &mut ScrollState {
        &mut self.scroll
    }

    fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            self.selected = 0;
            return;
        }
        if delta.is_negative() {
            self.selected = self.selected.saturating_sub(delta.unsigned_abs());
        } else {
            self.selected = (self.selected + delta as usize).min(self.items.len() - 1);
        }
    }
}

impl super::Component for ListComponent {
    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        let block = if focused {
            Block::default()
                .borders(Borders::ALL)
                .title(format!("{} (focus)", self.title))
                .border_style(Style::default().fg(Color::Green))
        } else {
            Block::default().borders(Borders::ALL).title(self.title.as_str())
        };
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let total = self.items.len();
        let view = inner.height as usize;
        if self.items.is_empty() {
            self.scroll.reset();
        } else {
            self.scroll.apply(total, view);
        }

        let offset = self.scroll.offset;
        let items = self
            .items
            .iter()
            .skip(offset)
            .take(view)
            .map(|item| ListItem::new(item.clone()))
            .collect::<Vec<_>>();

        let mut state = ListState::default();
        if total > 0 && self.selected >= offset {
            state.select(Some(self.selected - offset));
        }

        let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, inner, &mut state);
    }

    fn handle_event(&mut self, event: &Event) -> bool {
        let Event::Key(key) = event else {
            return false;
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                true
            }
            KeyCode::PageUp => {
                self.move_selection(-5);
                true
            }
            KeyCode::PageDown => {
                self.move_selection(5);
                true
            }
            KeyCode::Home => {
                self.selected = 0;
                true
            }
            KeyCode::End => {
                if !self.items.is_empty() {
                    self.selected = self.items.len() - 1;
                }
                true
            }
            _ => false,
        }
    }
}
