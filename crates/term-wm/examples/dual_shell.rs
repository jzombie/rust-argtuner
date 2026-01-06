use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::{Constraint, Direction, Rect};
use ratatui::{Frame, Terminal};

use portable_pty::PtySize;
use term_wm::components::{Component, TerminalComponent, default_shell_command};
use term_wm::drivers::console::ConsoleDriver;
use term_wm::layout::{LayoutNode, TilingLayout};
use term_wm::runner::{HasWindowManager, run_app};
use term_wm::window::WindowManager;

type PaneId = usize;

const MAX_WINDOWS: usize = 8;

fn main() -> io::Result<()> {
    let mut app = App::new()?;
    let focus_regions: Vec<PaneId> = (0..MAX_WINDOWS).collect();
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    terminal::enable_raw_mode()?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut driver = ConsoleDriver::new();

    let result = run_app(
        &mut terminal,
        &mut driver,
        &mut app,
        &focus_regions,
        |id| id,
        Some,
        Duration::from_millis(16),
        draw_ui,
        |event, app| {
            if matches!(event, Event::Mouse(_)) && app.windows.handle_managed_event(event) {
                return true;
            }
            if let Some(pane) = app.terminals.get_mut(app.windows.focus()) {
                return pane.handle_event(event);
            }
            false
        },
        |event, app| {
            if app.terminals.iter_mut().all(|pane| pane.has_exited()) {
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
    terminals: Vec<TerminalComponent>,
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
        let mut windows = WindowManager::new_managed(0);
        windows.set_focus_order(vec![0, 1]);
        let panes = vec![0, 1];
        windows.set_managed_layout(TilingLayout::new(build_layout(&panes)));
        Ok(Self {
            windows,
            terminals: vec![left, right],
            panes,
        })
    }

}

impl HasWindowManager<PaneId, PaneId> for App {
    fn windows(&mut self) -> &mut WindowManager<PaneId, PaneId> {
        &mut self.windows
    }

    fn wm_new_window(&mut self) {
        if self.terminals.len() >= MAX_WINDOWS {
            return;
        }
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pane =
            TerminalComponent::spawn(default_shell_command(), size).map_err(io::Error::other);
        if let Ok(pane) = pane {
            let id = self.terminals.len();
            self.terminals.push(pane);
            self.panes.push(id);
            self.windows.set_focus_order(self.panes.clone());
            self.windows.set_focus(id);
            self.windows.tile_window(id);
        }
    }
}

fn draw_ui(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let mut panes = Vec::new();
    for (id, pane) in app.terminals.iter_mut().enumerate() {
        if !pane.has_exited() {
            panes.push(id);
        }
    }
    app.windows.set_focus_order(panes.clone());
    if panes != app.panes {
        app.windows
            .set_managed_layout(TilingLayout::new(build_layout(&panes)));
        app.panes = panes.clone();
    }

    if panes.is_empty() {
        frame.buffer_mut().set_string(
            area.x,
            area.y,
            "all shells exited",
            ratatui::style::Style::default(),
        );
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
    if panes.len() == 1 {
        return LayoutNode::leaf(panes[0]);
    }
    if panes.len() == 2 {
        return LayoutNode::split(
            Direction::Horizontal,
            vec![Constraint::Percentage(50), Constraint::Percentage(50)],
            vec![LayoutNode::leaf(panes[0]), LayoutNode::leaf(panes[1])],
        );
    }
    let mut constraints = Vec::with_capacity(panes.len());
    let base = 100 / panes.len() as u16;
    for i in 0..panes.len() {
        if i == panes.len() - 1 {
            let used = base.saturating_mul((panes.len() - 1) as u16);
            constraints.push(Constraint::Percentage(100u16.saturating_sub(used)));
        } else {
            constraints.push(Constraint::Percentage(base));
        }
    }
    let children = panes.iter().map(|id| LayoutNode::leaf(*id)).collect();
    LayoutNode::split(Direction::Vertical, constraints, children)
}

fn render_pane(frame: &mut Frame, app: &mut App, id: PaneId, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let focused = app.windows.focus() == id;
    if let Some(pane) = app.terminals.get_mut(id) {
        pane.resize(area);
        pane.render(frame, area, focused);
    }
}
