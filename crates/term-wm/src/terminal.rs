use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

pub type PtyResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub struct TerminalPane {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    pending: Arc<Mutex<Vec<u8>>>,
    parser: vt100::Parser,
    size: PtySize,
    scrollback_len: usize,
    child: Option<Box<dyn Child + Send + Sync>>,
    exited: bool,
    _reader: JoinHandle<()>,
}

impl TerminalPane {
    pub fn spawn(command: CommandBuilder, size: PtySize) -> PtyResult<Self> {
        Self::spawn_with_scrollback(command, size, 0)
    }

    pub fn spawn_with_scrollback(
        command: CommandBuilder,
        size: PtySize,
        scrollback_len: usize,
    ) -> PtyResult<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(size)
            .map_err(|err| wrap_err("openpty", err))?;
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|err| wrap_err("spawn_command", err))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| wrap_err("try_clone_reader", err))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| wrap_err("take_writer", err))?;
        let pending = Arc::new(Mutex::new(Vec::new()));
        let reader_pending = Arc::clone(&pending);
        let reader_handle = thread::spawn(move || read_loop(reader, reader_pending));
        let parser = vt100::Parser::new(size.rows, size.cols, scrollback_len);
        Ok(Self {
            master: pair.master,
            writer,
            pending,
            parser,
            size,
            scrollback_len,
            child: Some(child),
            exited: false,
            _reader: reader_handle,
        })
    }

    pub fn resize(&mut self, size: PtySize) -> PtyResult<()> {
        if size == self.size {
            return Ok(());
        }
        self.master
            .resize(size)
            .map_err(|err| wrap_err("resize", err))?;
        self.size = size;
        self.parser.screen_mut().set_size(size.rows, size.cols);
        Ok(())
    }

    pub fn write_bytes(&mut self, input: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(input)
    }

    pub fn write_str(&mut self, input: &str) -> std::io::Result<()> {
        self.write_bytes(input.as_bytes())
    }

    pub fn update(&mut self) {
        let mut pending = self.pending.lock().unwrap_or_else(|err| err.into_inner());
        if !pending.is_empty() {
            let bytes = pending.split_off(0);
            self.parser.process(&bytes);
        }
    }

    pub fn screen_lines(&mut self) -> Vec<String> {
        self.update();
        let contents = self.parser.screen().contents();
        let mut lines: Vec<String> = contents.lines().map(|line| line.to_string()).collect();
        if lines.len() < self.size.rows as usize {
            lines.resize(self.size.rows as usize, String::new());
        }
        lines
    }

    pub fn has_exited(&mut self) -> bool {
        if self.exited {
            return true;
        }
        let Some(child) = self.child.as_mut() else {
            return true;
        };
        match child.try_wait() {
            Ok(Some(_)) => {
                self.exited = true;
                self.child = None;
                true
            }
            Ok(None) => false,
            Err(_) => false,
        }
    }

    pub fn size(&self) -> PtySize {
        self.size
    }

    pub fn screen(&mut self) -> &vt100::Screen {
        self.update();
        self.parser.screen()
    }

    pub fn screen_mut(&mut self) -> &mut vt100::Screen {
        self.update();
        self.parser.screen_mut()
    }

    pub fn scrollback(&mut self) -> usize {
        self.screen().scrollback()
    }

    pub fn set_scrollback(&mut self, rows: usize) {
        self.screen_mut().set_scrollback(rows);
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback_len
    }

    pub fn alternate_screen(&mut self) -> bool {
        self.screen().alternate_screen()
    }
}

fn read_loop(mut reader: Box<dyn Read + Send>, pending: Arc<Mutex<Vec<u8>>>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Ok(mut pending) = pending.lock() {
                    pending.extend_from_slice(&buf[..n]);
                }
            }
            Err(_) => break,
        }
    }
}

fn wrap_err<E: std::fmt::Display>(
    stage: &'static str,
    err: E,
) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::new(
        std::io::ErrorKind::Other,
        format!("pty {stage} failed: {err}"),
    ))
}
