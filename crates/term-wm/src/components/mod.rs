use crossterm::event::Event;
use ratatui::{layout::Rect, Frame};

pub mod terminal;
pub mod list;
pub mod scroll_view;
pub mod status_bar;
pub mod toggle_list;

pub use list::ListComponent;
pub use scroll_view::ScrollView;
pub use status_bar::StatusBar;
pub use terminal::{default_shell, default_shell_command, TerminalComponent};
pub use toggle_list::{ToggleItem, ToggleListComponent};

pub trait Component {
    fn resize(&mut self, _area: Rect) {}

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool);

    fn handle_event(&mut self, _event: &Event) -> bool {
        false
    }
}
