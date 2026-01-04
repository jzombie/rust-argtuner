use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::{Constraint, Direction, Rect};
use ratatui::widgets::{Block, Borders};
use ratatui::{Frame, Terminal};

use portable_pty::PtySize;
use term_wm::components::{default_shell_command, Component, TerminalComponent};
use term_wm::layout::LayoutNode;
use term_wm::runner::{run_app, HasWindowManager};
use term_wm::window::WindowManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PaneId {
    Left,
    Right,
}

fn main() -> io::Result<()> {
    let mut app = App::new()?;
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, event::EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(
        &mut terminal,
        &mut app,
        &[PaneId::Left, PaneId::Right],
        |id| id,
        Duration::from_millis(16),
        |frame, app| draw_ui(frame, app),
        |event, focus_handled, app| {
            if focus_handled {
                return;
            }
            match app.windows.focus() {
                PaneId::Left => {
                    let _ = app.left.handle_event(event);
                }
                PaneId::Right => {
                    let _ = app.right.handle_event(event);
                }
            }
        },
        |event| matches!(
            event,
            Event::Key(key)
                if key.code == KeyCode::Char('q')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
        ),
    );

    terminal::disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        event::DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    result
}

struct App {
    windows: WindowManager<PaneId, PaneId>,
    left: TerminalComponent,
    right: TerminalComponent,
}

impl App {
    fn new() -> io::Result<Self> {
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let left = TerminalComponent::spawn(default_shell_command(), size)
            .map_err(io::Error::other)?;
        let right = TerminalComponent::spawn(default_shell_command(), size)
            .map_err(io::Error::other)?;
        let mut windows = WindowManager::new(PaneId::Left);
        windows.set_focus_order(vec![PaneId::Left, PaneId::Right]);
        Ok(Self {
            windows,
            left,
            right,
        })
    }
}

impl HasWindowManager<PaneId, PaneId> for App {
    fn windows(&mut self) -> &mut WindowManager<PaneId, PaneId> {
        &mut self.windows
    }
}

fn draw_ui(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let layout = LayoutNode::split(
        Direction::Horizontal,
        vec![Constraint::Percentage(50), Constraint::Percentage(50)],
        vec![LayoutNode::leaf(PaneId::Left), LayoutNode::leaf(PaneId::Right)],
    );
    for (id, rect) in layout.layout(area) {
        app.windows.set_region(id, rect);
    }
    let left_rect = app.windows.region(PaneId::Left);
    let right_rect = app.windows.region(PaneId::Right);

    render_pane(frame, app, PaneId::Left, left_rect);
    render_pane(frame, app, PaneId::Right, right_rect);
}

fn render_pane(frame: &mut Frame, app: &mut App, id: PaneId, area: Rect) {
    let focused = app.windows.focus() == id;
    let title = if focused { "Shell (focus)" } else { "Shell" };
    let block = if focused {
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(ratatui::style::Style::default().fg(ratatui::style::Color::Green))
    } else {
        Block::default().borders(Borders::ALL).title(title)
    };
    let inner = block.inner(area);
    frame.render_widget(block, area);
    match id {
        PaneId::Left => {
            app.left.resize(inner);
            app.left.render(frame, inner, focused);
        }
        PaneId::Right => {
            app.right.resize(inner);
            app.right.render(frame, inner, focused);
        }
    }
}
