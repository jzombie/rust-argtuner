use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::{Constraint, Direction, Line, Rect, Span};
use ratatui::style::{Color as TColor, Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph};
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

    let (lines, exited) = match id {
        PaneId::Left => (
            render_screen_lines(app.left.screen(), inner.height, inner.width),
            app.left.has_exited(),
        ),
        PaneId::Right => (
            render_screen_lines(app.right.screen(), inner.height, inner.width),
            app.right.has_exited(),
        ),
    };
    if exited {
        frame.render_widget(Paragraph::new("shell exited"), inner);
        return;
    }
    frame.render_widget(Paragraph::new(lines), inner);
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

fn render_screen_lines(screen: &vt100::Screen, rows: u16, cols: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(rows as usize);
    for row in 0..rows {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut current = String::new();
        let mut current_style = Style::default();
        let mut have_style = false;

        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else {
                push_span(&mut spans, &mut current, have_style, current_style);
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let (fg, bg) = resolve_colors(cell);
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

            let contents = cell.contents();
            if !have_style {
                have_style = true;
                current_style = style;
                current.push_str(if contents.is_empty() { " " } else { contents });
                continue;
            }
            if style == current_style {
                current.push_str(if contents.is_empty() { " " } else { contents });
            } else {
                push_span(&mut spans, &mut current, true, current_style);
                current_style = style;
                current.push_str(if contents.is_empty() { " " } else { contents });
            }
        }
        push_span(&mut spans, &mut current, have_style, current_style);
        lines.push(Line::from(spans));
    }
    lines
}

fn push_span(
    spans: &mut Vec<Span<'static>>,
    current: &mut String,
    have_style: bool,
    style: Style,
) {
    if current.is_empty() {
        return;
    }
    let content = std::mem::take(current);
    if have_style {
        spans.push(Span::styled(content, style));
    } else {
        spans.push(Span::raw(content));
    }
}

fn resolve_colors(cell: &vt100::Cell) -> (Option<TColor>, Option<TColor>) {
    let mut fg = vt_color_to_ratatui(cell.fgcolor());
    let mut bg = vt_color_to_ratatui(cell.bgcolor());
    if cell.inverse() {
        std::mem::swap(&mut fg, &mut bg);
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
