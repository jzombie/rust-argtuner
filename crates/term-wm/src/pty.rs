use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

pub type PtyResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub struct TermPane {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    buffer: Arc<Mutex<TextBuffer>>,
    _reader: JoinHandle<()>,
    size: PtySize,
}

impl TermPane {
    pub fn spawn(command: CommandBuilder, size: PtySize) -> PtyResult<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(size)?;
        let _child = pair.slave.spawn_command(command)?;
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let buffer = Arc::new(Mutex::new(TextBuffer::new(size)));
        let reader_buffer = Arc::clone(&buffer);
        let reader_handle = thread::spawn(move || read_loop(reader, reader_buffer));
        Ok(Self {
            master: pair.master,
            writer,
            buffer,
            _reader: reader_handle,
            size,
        })
    }

    pub fn resize(&mut self, size: PtySize) -> PtyResult<()> {
        self.master.resize(size)?;
        self.size = size;
        if let Ok(mut buffer) = self.buffer.lock() {
            buffer.resize(size);
        }
        Ok(())
    }

    pub fn write_str(&mut self, input: &str) -> std::io::Result<()> {
        self.writer.write_all(input.as_bytes())
    }

    pub fn lines(&self) -> Vec<String> {
        self.buffer.lock().map(|buf| buf.lines()).unwrap_or_default()
    }

    pub fn size(&self) -> PtySize {
        self.size
    }
}

fn read_loop(mut reader: Box<dyn Read + Send>, buffer: Arc<Mutex<TextBuffer>>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Ok(mut buffer) = buffer.lock() {
                    buffer.push_bytes(&buf[..n]);
                }
            }
            Err(_) => break,
        }
    }
}

#[derive(Debug)]
struct TextBuffer {
    lines: Vec<String>,
    current: String,
    max_lines: usize,
    width: usize,
}

impl TextBuffer {
    fn new(size: PtySize) -> Self {
        Self {
            lines: Vec::new(),
            current: String::new(),
            max_lines: size.rows as usize,
            width: size.cols as usize,
        }
    }

    fn resize(&mut self, size: PtySize) {
        self.max_lines = size.rows as usize;
        self.width = size.cols as usize;
        self.trim_lines();
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        for ch in text.chars() {
            match ch {
                '\n' => {
                    self.push_line();
                }
                '\r' => {
                    self.current.clear();
                }
                '\t' => {
                    for _ in 0..4 {
                        self.push_char(' ');
                    }
                }
                _ => self.push_char(ch),
            }
        }
    }

    fn push_char(&mut self, ch: char) {
        self.current.push(ch);
        if self.width > 0 && self.current.chars().count() >= self.width {
            self.push_line();
        }
    }

    fn push_line(&mut self) {
        let line = std::mem::take(&mut self.current);
        self.lines.push(line);
        self.trim_lines();
    }

    fn trim_lines(&mut self) {
        if self.max_lines == 0 {
            self.lines.clear();
            return;
        }
        while self.lines.len() > self.max_lines {
            self.lines.remove(0);
        }
    }

    fn lines(&self) -> Vec<String> {
        let mut out = self.lines.clone();
        if !self.current.is_empty() {
            out.push(self.current.clone());
        }
        out
    }
}
