use std::io;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::drivers::InputDriver;
use crate::event_loop::{ControlFlow, EventLoop};
use crate::window::{LayoutContract, WindowManager, WmMenuAction};

pub trait HasWindowManager<W: Copy + Eq + Ord, R: Copy + Eq + Ord> {
    fn windows(&mut self) -> &mut WindowManager<W, R>;
    fn wm_new_window(&mut self) {}
}

#[allow(clippy::too_many_arguments)]
pub fn run_app<B, D, A, W, R, FDraw, FDispatch, FQuit, FMap, FFocus, E>(
    terminal: &mut Terminal<B>,
    driver: &mut D,
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
    D: InputDriver,
    A: HasWindowManager<W, R>,
    W: Copy + Eq + Ord,
    R: Copy + Eq + Ord + PartialEq<W> + std::fmt::Debug,
    FDraw: FnMut(&mut ratatui::Frame, &mut A),
    FDispatch: FnMut(&Event, &mut A) -> bool,
    FQuit: FnMut(Option<&Event>, &mut A) -> bool,
    FMap: Fn(R) -> W,
    FFocus: Fn(W) -> Option<R>,
    E: From<io::Error> + From<<B as Backend>::Error>,
{
    let capture_timeout = Duration::from_millis(500);
    let mut event_loop = EventLoop::new(driver, poll_interval);

    event_loop.run(|_driver, event| {
        if let Some(evt) = event {
            let wm_mode = app.windows().layout_contract() == LayoutContract::WindowManaged;
            if wm_mode && let Event::Key(key) = evt {
                if key.code == KeyCode::Esc && key.kind == KeyEventKind::Press {
                    if app.windows().wm_overlay_visible() {
                        let passthrough = app.windows().esc_passthrough_active();
                        app.windows().close_wm_overlay();
                        if passthrough {
                            let _ = dispatch(&Event::Key(key), app);
                        }
                    } else {
                        app.windows().open_wm_overlay();
                    }
                    return Ok(ControlFlow::Continue);
                }
            }
            if wm_mode && app.windows().wm_overlay_visible() {
                if let Some(action) = app.windows().handle_wm_menu_event(&evt) {
                    match action {
                        WmMenuAction::CloseMenu => {
                            app.windows().close_wm_overlay();
                        }
                        WmMenuAction::NewWindow => {
                            app.wm_new_window();
                            app.windows().close_wm_overlay();
                        }
                        WmMenuAction::ExitUi => {
                            return Ok(ControlFlow::Quit);
                        }
                    }
                    return Ok(ControlFlow::Continue);
                }
                if app.windows().wm_menu_consumes_event(&evt) {
                    return Ok(ControlFlow::Continue);
                }
                if let Event::Key(key) = evt
                    && key.code == KeyCode::Char('n')
                    && key.modifiers.is_empty()
                {
                    app.wm_new_window();
                    app.windows().close_wm_overlay();
                    return Ok(ControlFlow::Continue);
                }
            }
            if should_quit(Some(&evt), app) {
                return Ok(ControlFlow::Quit);
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
                        return Ok(ControlFlow::Continue);
                    }
                    if dispatch(&evt, app) {
                        return Ok(ControlFlow::Continue);
                    }
                    let _ = app
                        .windows()
                        .handle_focus_event(&evt, focus_regions, &map_region);
                    app.windows().bring_focus_to_front(&map_focus);
                    return Ok(ControlFlow::Continue);
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
                        return Ok(ControlFlow::Continue);
                    }
                    if dispatch(&evt, app) {
                        return Ok(ControlFlow::Continue);
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
        } else {
            if should_quit(None, app) {
                return Ok(ControlFlow::Quit);
            }
            app.windows().begin_frame();
            terminal
                .draw(|frame| {
                    draw(frame, app);
                    app.windows().render_overlays(frame);
                })
                .map_err(|e| io::Error::other(e.to_string()))?;
        }
        Ok(ControlFlow::Continue)
    })?;

    Ok(())
}
