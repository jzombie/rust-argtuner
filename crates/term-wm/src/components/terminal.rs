use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{CommandBuilder, PtySize};
use ratatui::{
    layout::Rect,
    style::{Color as TColor, Modifier, Style},
    Frame,
};

use crate::terminal::TerminalPane;

pub struct TerminalComponent {
    pane: TerminalPane,
    last_size: (u16, u16),
}

impl TerminalComponent {
    pub fn spawn(command: CommandBuilder, size: PtySize) -> crate::terminal::PtyResult<Self> {
        let pane = TerminalPane::spawn(command, size)?;
        Ok(Self {
            pane,
            last_size: (size.cols, size.rows),
        })
    }

    pub fn write_bytes(&mut self, input: &[u8]) -> std::io::Result<()> {
        self.pane.write_bytes(input)
    }

    pub fn has_exited(&mut self) -> bool {
        self.pane.has_exited()
    }

    fn render_screen(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        let screen = self.pane.screen();
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
}

#[cfg(unix)]
pub fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string())
}

#[cfg(windows)]
pub fn default_shell() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
}

pub fn default_shell_command() -> CommandBuilder {
    CommandBuilder::new(default_shell())
}

impl super::Component for TerminalComponent {
    fn resize(&mut self, area: Rect) {
        let size = (area.width, area.height);
        if size != self.last_size {
            let _ = self.pane.resize(PtySize {
                rows: area.height,
                cols: area.width,
                pixel_width: 0,
                pixel_height: 0,
            });
            self.last_size = size;
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        if self.pane.has_exited() {
            frame
                .buffer_mut()
                .set_string(area.x, area.y, "shell exited", Style::default());
            return;
        }
        self.render_screen(frame, area, focused);
    }

    fn handle_event(&mut self, event: &Event) -> bool {
        if let Event::Key(key) = event {
            let bytes = key_to_bytes(*key);
            if bytes.is_empty() {
                return false;
            }
            let _ = self.pane.write_bytes(&bytes);
            return true;
        }
        false
    }
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

fn resolve_colors(cell: &vt100::Cell, screen: &vt100::Screen) -> (Option<TColor>, Option<TColor>) {
    let mut fg = resolve_color(cell.fgcolor(), screen.fgcolor());
    let bg = resolve_color(cell.bgcolor(), screen.bgcolor());
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
