use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::{Constraint, Direction, Rect};
use ratatui::style::{Color as TColor, Modifier, Style};
use ratatui::widgets::{Block, Borders};
use ratatui::{Frame, Terminal};

use term_wm::layout::LayoutNode;
use term_wm::terminal::TerminalPane;
use term_wm::window::WindowManager;
use portable_pty::{CommandBuilder, PtySize};

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

    let result = run_app(&mut terminal, &mut app);

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
    left: TerminalPane,
    right: TerminalPane,
    last_left: (u16, u16),
    last_right: (u16, u16),
}

impl App {
    fn new() -> io::Result<Self> {
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        let left = TerminalPane::spawn(CommandBuilder::new(default_shell()), size)
            .map_err(io::Error::other)?;
        let right = TerminalPane::spawn(CommandBuilder::new(default_shell()), size)
            .map_err(io::Error::other)?;
        Ok(Self {
            windows: WindowManager::new(PaneId::Left),
            left,
            right,
            last_left: (0, 0),
            last_right: (0, 0),
        })
    }
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> io::Result<()> {
    loop {
        terminal.draw(|frame| draw_ui(frame, app))?;
        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(key) => match key.code {
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Tab => {
                        app.windows.set_focus_order(vec![PaneId::Left, PaneId::Right]);
                        app.windows.advance_focus(true);
                    }
                    KeyCode::BackTab => {
                        app.windows.set_focus_order(vec![PaneId::Left, PaneId::Right]);
                        app.windows.advance_focus(false);
                    }
                    _ => {
                        let bytes = key_to_bytes(key);
                        if !bytes.is_empty() {
                            match app.windows.focus() {
                                PaneId::Left => {
                                    let _ = app.left.write_bytes(&bytes);
                                }
                                PaneId::Right => {
                                    let _ = app.right.write_bytes(&bytes);
                                }
                            }
                        }
                    }
                },
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Down(_) => {
                        if let Some(pane) = app.windows.hit_test_region(
                            mouse.column,
                            mouse.row,
                            &[PaneId::Left, PaneId::Right],
                        ) {
                            app.windows.set_focus(pane);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
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

    resize_panes(app, left_rect, right_rect);
    render_pane(frame, app, PaneId::Left, left_rect);
    render_pane(frame, app, PaneId::Right, right_rect);
}

fn resize_panes(app: &mut App, left: Rect, right: Rect) {
    let left_inner = Block::default().borders(Borders::ALL).inner(left);
    let right_inner = Block::default().borders(Borders::ALL).inner(right);
    let left_size = (left_inner.width, left_inner.height);
    let right_size = (right_inner.width, right_inner.height);
    if app.last_left != left_size {
        let _ = app.left.resize(PtySize {
            rows: left_inner.height,
            cols: left_inner.width,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
    if app.last_right != right_size {
        let _ = app.right.resize(PtySize {
            rows: right_inner.height,
            cols: right_inner.width,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
    app.last_left = left_size;
    app.last_right = right_size;
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
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let (screen, exited) = match id {
        PaneId::Left => {
            let exited = app.left.has_exited();
            let screen = app.left.screen();
            (screen, exited)
        }
        PaneId::Right => {
            let exited = app.right.has_exited();
            let screen = app.right.screen();
            (screen, exited)
        }
    };
    if exited {
        let buffer = frame.buffer_mut();
        let x = inner.x;
        let y = inner.y;
        buffer.set_string(x, y, "shell exited", Style::default());
        return;
    }
    render_screen_to_buffer(frame, inner, screen, focused);
}

fn key_to_bytes(key: KeyEvent) -> Vec<u8> {
    match key.code {
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                if let Some(byte) = ctrl_char(c) {
                    return vec![byte];
                }
            }
            c.to_string().into_bytes()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        _ => Vec::new(),
    }
}

fn ctrl_char(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    if ('a'..='z').contains(&c) {
        Some((c as u8) - b'a' + 1)
    } else {
        None
    }
}

#[cfg(unix)]
fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string())
}

#[cfg(windows)]
fn default_shell() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
}

fn render_screen_to_buffer(
    frame: &mut Frame,
    area: Rect,
    screen: &vt100::Screen,
    focused: bool,
) {
    let buffer = frame.buffer_mut();
    for row in 0..area.height {
        for col in 0..area.width {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            let mut symbol = cell.contents().chars().next().unwrap_or(' ');
            let (fg, bg) = resolve_colors(cell, screen);
            let mut style = Style::default();
            if let Some(fg) = fg {
                style = style.fg(fg);
            }
            if let Some(bg) = bg {
                style = style.bg(bg);
            }
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.dim() {
                style = style.add_modifier(Modifier::DIM);
            }
            if cell.italic() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if cell.underline() {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            if cell.inverse() {
                style = style.add_modifier(Modifier::REVERSED);
            }
            if cell.is_wide_continuation() {
                symbol = ' ';
            }
            let x = area.x + col;
            let y = area.y + row;
            if let Some(cell) = buffer.cell_mut((x, y)) {
                let mut buf = [0u8; 4];
                let sym = symbol.encode_utf8(&mut buf);
                cell.set_symbol(sym).set_style(style);
            }
        }
    }

    if focused && !screen.hide_cursor() {
        let (row, col) = screen.cursor_position();
        if row < area.height && col < area.width {
            if let Some(cell) = buffer.cell_mut((area.x + col, area.y + row)) {
                cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
            }
        }
    }
}

fn resolve_colors(cell: &vt100::Cell, screen: &vt100::Screen) -> (Option<TColor>, Option<TColor>) {
    let mut fg = resolve_color(cell.fgcolor(), screen.fgcolor());
    let mut bg = resolve_color(cell.bgcolor(), screen.bgcolor());
    if cell.bold() {
        fg = brighten_indexed(fg);
    }
    (fg, bg)
}

fn vt_color_to_ratatui(color: vt100::Color) -> Option<TColor> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(idx) => Some(TColor::Indexed(idx)),
        vt100::Color::Rgb(r, g, b) => Some(TColor::Rgb(r, g, b)),
    }
}

fn resolve_color(color: vt100::Color, screen_default: vt100::Color) -> Option<TColor> {
    match color {
        vt100::Color::Default => match screen_default {
            vt100::Color::Default => None,
            other => vt_color_to_ratatui(other),
        },
        other => vt_color_to_ratatui(other),
    }
}

fn brighten_indexed(color: Option<TColor>) -> Option<TColor> {
    match color {
        Some(TColor::Indexed(idx)) if idx < 8 => Some(TColor::Indexed(idx + 8)),
        _ => color,
    }
}
