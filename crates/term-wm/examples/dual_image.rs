use std::fs;
use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::{Constraint, Direction, Rect};
use ratatui::widgets::{Block, Borders};
use ratatui::{Frame, Terminal};

use term_wm::components::{AsciiImage, Component};
use term_wm::layout::{LayoutNode, TilingLayout};
use term_wm::runner::{run_app, HasWindowManager};
use term_wm::window::WindowManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PaneId {
    Left,
    Right,
}

fn main() -> io::Result<()> {
    let mut app = App::new(std::env::args().skip(1).collect())?;
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
            if matches!(event, Event::Mouse(_)) && app.layout.handle_event(event, app.layout_area) {
                return true;
            }
            match app.windows.focus() {
                PaneId::Left => app.left.handle_event(event),
                PaneId::Right => app.right.handle_event(event),
            }
        },
        |event, _app| {
            matches!(event, Some(Event::Key(key)) if key.code == KeyCode::Esc)
                || matches!(
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
    layout: TilingLayout<PaneId>,
    layout_area: Rect,
    left: AsciiImage,
    right: AsciiImage,
}

impl App {
    fn new(mut paths: Vec<String>) -> io::Result<Self> {
        let mut left = AsciiImage::new();
        let mut right = AsciiImage::new();
        left.set_keep_aspect(true);
        right.set_keep_aspect(true);
        left.set_colorize(true);
        right.set_colorize(true);
        if paths.is_empty() {
            paths.push("assets/zenOSmosis-logo.svg".to_string());
        }
        if paths.len() == 1 {
            paths.push(paths[0].clone());
        }
        load_into(&mut left, &paths[0])?;
        load_into(&mut right, &paths[1])?;
        let mut windows = WindowManager::new(PaneId::Left);
        windows.set_focus_order(vec![PaneId::Left, PaneId::Right]);
        let layout = TilingLayout::new(LayoutNode::split(
            Direction::Horizontal,
            vec![Constraint::Percentage(50), Constraint::Percentage(50)],
            vec![LayoutNode::leaf(PaneId::Left), LayoutNode::leaf(PaneId::Right)],
        ));
        Ok(Self {
            windows,
            layout,
            layout_area: Rect::default(),
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
    app.layout_area = area;
    app.windows
        .register_tiling_layout(&app.layout, area);
    let left_rect = app.windows.region(PaneId::Left);
    let right_rect = app.windows.region(PaneId::Right);

    render_pane(frame, &mut app.left, left_rect);
    render_pane(frame, &mut app.right, right_rect);
}

fn render_pane(frame: &mut Frame, image: &mut AsciiImage, area: Rect) {
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    image.render(frame, inner, false);
}

fn load_into(component: &mut AsciiImage, path: &str) -> io::Result<()> {
    if path.ends_with(".svg") {
        return component
            .load_svg_from_path(path)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err));
    }
    let bytes = fs::read(path)?;
    let image = decode_pnm(&bytes)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unsupported image"))?;
    match image {
        Pnm::Luma { width, height, data } => component.set_luma8(width, height, data),
        Pnm::Rgba { width, height, data } => component.set_rgba8(width, height, data),
    }
    Ok(())
}

enum Pnm {
    Luma { width: u32, height: u32, data: Vec<u8> },
    Rgba { width: u32, height: u32, data: Vec<u8> },
}

fn decode_pnm(bytes: &[u8]) -> Option<Pnm> {
    let mut idx = 0;
    let magic = next_token(bytes, &mut idx)?;
    let width: u32 = next_token(bytes, &mut idx)?.parse().ok()?;
    let height: u32 = next_token(bytes, &mut idx)?.parse().ok()?;
    let maxval: u32 = next_token(bytes, &mut idx)?.parse().ok()?;
    if maxval == 0 || maxval > 255 {
        return None;
    }
    if magic == "P5" {
        let count = (width * height) as usize;
        let data = bytes.get(idx..idx + count)?.to_vec();
        if maxval != 255 {
            let data = data
                .into_iter()
                .map(|v| ((v as u32 * 255) / maxval) as u8)
                .collect();
            return Some(Pnm::Luma {
                width,
                height,
                data,
            });
        }
        return Some(Pnm::Luma {
            width,
            height,
            data,
        });
    }
    if magic == "P6" {
        let count = (width * height * 3) as usize;
        let raw = bytes.get(idx..idx + count)?.to_vec();
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for chunk in raw.chunks_exact(3) {
            let r = scale_max(chunk[0], maxval);
            let g = scale_max(chunk[1], maxval);
            let b = scale_max(chunk[2], maxval);
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
        return Some(Pnm::Rgba {
            width,
            height,
            data: rgba,
        });
    }
    None
}

fn scale_max(value: u8, maxval: u32) -> u8 {
    if maxval == 255 {
        value
    } else {
        ((value as u32 * 255) / maxval) as u8
    }
}

fn next_token<'a>(bytes: &'a [u8], idx: &mut usize) -> Option<&'a str> {
    while *idx < bytes.len() {
        let b = bytes[*idx];
        if b == b'#' {
            while *idx < bytes.len() && bytes[*idx] != b'\n' {
                *idx += 1;
            }
            continue;
        }
        if b.is_ascii_whitespace() {
            *idx += 1;
            continue;
        }
        break;
    }
    let start = *idx;
    while *idx < bytes.len() && !bytes[*idx].is_ascii_whitespace() {
        *idx += 1;
    }
    std::str::from_utf8(bytes.get(start..*idx)?).ok()
}
