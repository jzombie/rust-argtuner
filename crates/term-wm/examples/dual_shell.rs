use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::{Constraint, Direction, Rect};
use ratatui::widgets::{Block, Borders, Clear};
use ratatui::{Frame, Terminal};

use portable_pty::PtySize;
use term_wm::components::{Component, TerminalComponent, default_shell_command};
use term_wm::layout::{LayoutNode, TilingLayout};
use term_wm::runner::{HasWindowManager, run_app};
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
        |event, app| {
            if matches!(event, Event::Mouse(_)) && app.windows.handle_managed_event(event) {
                return true;
            }
            match app.windows.focus() {
                PaneId::Left => app.left.handle_event(event),
                PaneId::Right => app.right.handle_event(event),
            }
        },
        |event, app| {
            if app.left.has_exited() && app.right.has_exited() {
                return true;
            }
            matches!(
                event,
                Some(Event::Key(key))
                    if key.code == KeyCode::Char('q')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
            )
        },
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
    panes: Vec<PaneId>,
}

impl App {
    fn new() -> io::Result<Self> {
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let left =
            TerminalComponent::spawn(default_shell_command(), size).map_err(io::Error::other)?;
        let right =
            TerminalComponent::spawn(default_shell_command(), size).map_err(io::Error::other)?;
        let mut windows = WindowManager::new_managed(PaneId::Left);
        windows.set_focus_order(vec![PaneId::Left, PaneId::Right]);
        let panes = vec![PaneId::Left, PaneId::Right];
        windows.set_managed_layout(TilingLayout::new(build_layout(&panes)));
        Ok(Self {
            windows,
            left,
            right,
            panes,
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
    let left_exited = app.left.has_exited();
    let right_exited = app.right.has_exited();
    let panes = match (left_exited, right_exited) {
        (false, false) => vec![PaneId::Left, PaneId::Right],
        (false, true) => vec![PaneId::Left],
        (true, false) => vec![PaneId::Right],
        (true, true) => Vec::new(),
    };
    app.windows.set_focus_order(panes.clone());
    if panes != app.panes {
        app.windows
            .set_managed_layout(TilingLayout::new(build_layout(&panes)));
        app.panes = panes.clone();
    }

    if panes.is_empty() {
        frame
            .buffer_mut()
            .set_string(area.x, area.y, "all shells exited", ratatui::style::Style::default());
        return;
    }
    app.windows.register_managed_layout(area);

    let draw_order = app.windows.managed_draw_order().to_vec();
    for pane in draw_order {
        let rect = app.windows.region(pane);
        render_pane(frame, app, pane, rect);
    }
}

fn build_layout(panes: &[PaneId]) -> LayoutNode<PaneId> {
    match panes {
        [PaneId::Left, PaneId::Right] => LayoutNode::split(
            Direction::Horizontal,
            vec![Constraint::Percentage(50), Constraint::Percentage(50)],
            vec![
                LayoutNode::leaf(PaneId::Left),
                LayoutNode::leaf(PaneId::Right),
            ],
        ),
        [PaneId::Left] => LayoutNode::leaf(PaneId::Left),
        [PaneId::Right] => LayoutNode::leaf(PaneId::Right),
        _ => LayoutNode::leaf(PaneId::Left),
    }
}

fn render_pane(frame: &mut Frame, app: &mut App, id: PaneId, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let focused = app.windows.focus() == id;
    let title = if focused { "Shell (focus)" } else { "Shell" };
    frame.render_widget(Clear, area);
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
