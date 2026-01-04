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
    FDispatch: FnMut(&Event, &mut A),
    FQuit: FnMut(&Event) -> bool,
    FMap: Fn(R) -> W,
    E: From<io::Error> + From<<B as Backend>::Error>,
{
    loop {
        terminal.draw(|frame| draw(frame, app))?;
        if event::poll(poll_interval)? {
            let evt = event::read()?;
            if should_quit(&evt) {
                return Ok(());
            }
            if app
                .windows()
                .handle_focus_event(&evt, focus_regions, &map_region)
            {
                continue;
            }
            dispatch(&evt, app);
        }
    }
}
