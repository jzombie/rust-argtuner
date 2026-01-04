use std::io;
use std::time::Duration;

use crossterm::event::{self, Event};
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
    FDispatch: FnMut(&Event, bool, &mut A),
    FQuit: FnMut(Option<&Event>, &mut A) -> bool,
    FMap: Fn(R) -> W,
    E: From<io::Error> + From<<B as Backend>::Error>,
{
    loop {
        if should_quit(None, app) {
            return Ok(());
        }
        terminal.draw(|frame| draw(frame, app))?;
        if event::poll(poll_interval)? {
            let evt = event::read()?;
            if should_quit(Some(&evt), app) {
                return Ok(());
            }
            let focus_handled = app
                .windows()
                .handle_focus_event(&evt, focus_regions, &map_region);
            dispatch(&evt, focus_handled, app);
        }
    }
}
