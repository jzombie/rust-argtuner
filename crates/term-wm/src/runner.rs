use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::Terminal;

use crate::window::WindowManager;

pub trait HasWindowManager<W: Copy + Eq + Ord, R: Copy + Eq + Ord> {
    fn windows(&mut self) -> &mut WindowManager<W, R>;
}

pub fn run_app<B, A, W, R, FDraw, FDispatch, FQuit, FMap, E>(
    terminal: &mut Terminal<B>,
    app: &mut A,
    focus_regions: &[R],
    map_region: FMap,
    poll_interval: Duration,
    mut draw: FDraw,
    mut dispatch: FDispatch,
    mut should_quit: FQuit,
) -> Result<(), E>
where
    B: Backend,
    A: HasWindowManager<W, R>,
    W: Copy + Eq + Ord,
    R: Copy + Eq + Ord,
    FDraw: FnMut(&mut ratatui::Frame, &mut A),
    FDispatch: FnMut(&Event, &mut A) -> bool,
    FQuit: FnMut(Option<&Event>, &mut A) -> bool,
    FMap: Fn(R) -> W,
    E: From<io::Error> + From<<B as Backend>::Error>,
{
    loop {
        if should_quit(None, app) {
            return Ok(());
        }
        app.windows().begin_frame();
        terminal.draw(|frame| {
            draw(frame, app);
            app.windows().render_overlays(frame);
        })?;
        if event::poll(poll_interval)? {
            let evt = normalize_event(event::read()?);
            if should_quit(Some(&evt), app) {
                return Ok(());
            }
            match &evt {
                Event::Key(key) if key.code == KeyCode::Esc => {
                    app.windows().arm_capture(Duration::from_millis(500));
                    continue;
                }
                Event::Key(key) if key.code == KeyCode::BackTab => {
                    if app.windows().capture_active() {
                        app.windows().clear_capture();
                        let _ = app
                            .windows()
                            .handle_focus_event(&evt, focus_regions, &map_region);
                        continue;
                    }
                    if dispatch(&evt, app) {
                        continue;
                    }
                    let _ = app
                        .windows()
                        .handle_focus_event(&evt, focus_regions, &map_region);
                    continue;
                }
                Event::Key(key) if key.code == KeyCode::Tab => {
                    if app.windows().capture_active() {
                        app.windows().clear_capture();
                        let _ = app
                            .windows()
                            .handle_focus_event(&evt, focus_regions, &map_region);
                        continue;
                    }
                    if dispatch(&evt, app) {
                        continue;
                    }
                    let _ = app
                        .windows()
                        .handle_focus_event(&evt, focus_regions, &map_region);
                }
                Event::Key(_) if app.windows().capture_active() => {
                    app.windows().clear_capture();
                    let _ = dispatch(&evt, app);
                }
                _ => {
                    let _ = app
                        .windows()
                        .handle_focus_event(&evt, focus_regions, &map_region);
                    let _ = dispatch(&evt, app);
                }
            }
        }
    }
}

fn normalize_event(evt: Event) -> Event {
    match evt {
        Event::Key(mut key) if key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT) => {
            key.code = KeyCode::BackTab;
            key.modifiers.remove(KeyModifiers::SHIFT);
            Event::Key(key)
        }
        _ => evt,
    }
}
