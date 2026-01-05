use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::Terminal;

use crate::window::{LayoutContract, WindowManager};

pub trait HasWindowManager<W: Copy + Eq + Ord, R: Copy + Eq + Ord> {
    fn windows(&mut self) -> &mut WindowManager<W, R>;
    fn wm_new_window(&mut self) {}
}

pub fn run_app<B, A, W, R, FDraw, FDispatch, FQuit, FMap, FFocus, E>(
    terminal: &mut Terminal<B>,
    app: &mut A,
    focus_regions: &[R],
    map_region: FMap,
    map_focus: FFocus,
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
    FFocus: Fn(W) -> Option<R>,
    E: From<io::Error> + From<<B as Backend>::Error>,
{
    let capture_timeout = Duration::from_millis(500);
    loop {
        if should_quit(None, app) {
            return Ok(());
        }
        let wm_mode = app.windows().layout_contract() == LayoutContract::WindowManaged;
        app.windows().begin_frame();
        terminal.draw(|frame| {
            draw(frame, app);
            app.windows().render_overlays(frame);
        })?;
        if event::poll(poll_interval)? {
            let evt = normalize_event(event::read()?);
            if wm_mode {
                if let Event::Key(key) = evt {
                    if key.code == KeyCode::Esc {
                        if app.windows().wm_overlay_visible() {
                            let passthrough = app.windows().esc_passthrough_active();
                            app.windows().close_wm_overlay();
                            if passthrough {
                                let _ = dispatch(&Event::Key(key), app);
                            }
                        } else {
                            app.windows().open_wm_overlay();
                        }
                        continue;
                    }
                    if app.windows().wm_overlay_visible()
                        && key.code == KeyCode::Char('n')
                        && key.modifiers.is_empty()
                    {
                        app.wm_new_window();
                        app.windows().close_wm_overlay();
                        continue;
                    }
                }
            }
            if should_quit(Some(&evt), app) {
                return Ok(());
            }
            match &evt {
                Event::Key(key) if key.code == KeyCode::BackTab => {
                    if app.windows().capture_active() {
                        if wm_mode {
                            app.windows().arm_capture(capture_timeout);
                        }
                        let _ = app
                            .windows()
                            .handle_focus_event(&evt, focus_regions, &map_region);
                        app.windows().bring_focus_to_front(&map_focus);
                        continue;
                    }
                    if dispatch(&evt, app) {
                        continue;
                    }
                    let _ = app
                        .windows()
                        .handle_focus_event(&evt, focus_regions, &map_region);
                    app.windows().bring_focus_to_front(&map_focus);
                    continue;
                }
                Event::Key(key) if key.code == KeyCode::Tab => {
                    if app.windows().capture_active() {
                        if wm_mode {
                            app.windows().arm_capture(capture_timeout);
                        }
                        let _ = app
                            .windows()
                            .handle_focus_event(&evt, focus_regions, &map_region);
                        app.windows().bring_focus_to_front(&map_focus);
                        continue;
                    }
                    if dispatch(&evt, app) {
                        continue;
                    }
                    let _ = app
                        .windows()
                        .handle_focus_event(&evt, focus_regions, &map_region);
                    app.windows().bring_focus_to_front(&map_focus);
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
