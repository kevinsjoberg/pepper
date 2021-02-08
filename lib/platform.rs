use std::{io, process::Command};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    None,
    Backspace,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    Delete,
    F(u8),
    Char(char),
    Ctrl(char),
    Alt(char),
    Esc,
}

#[derive(Clone, Copy)]
pub enum ServerPlatformEvent {
    Redraw,
    Idle,
    ConnectionOpen { index: usize },
    ConnectionClose { index: usize },
    ConnectionMessage { index: usize, len: usize },
    ProcessStdout { index: usize, len: usize },
    ProcessStderr { index: usize, len: usize },
    ProcessExit { index: usize, success: bool },
}

#[derive(Clone, Copy)]
pub enum ClientPlatformEvent {
    Resize(usize, usize),
    Key(Key),
    Message(usize),
}

pub trait Args: Sized {
    fn parse() -> Option<Self>;
    fn session(&self) -> Option<&str>;
    fn print_session(&self) -> bool;
}

pub trait ServerPlatform {
    fn request_redraw(&mut self);

    fn read_from_clipboard(&mut self, text: &mut String) -> bool;
    fn write_to_clipboard(&mut self, text: &str);

    fn read_from_connection(&mut self, index: usize, len: usize) -> &[u8];
    fn write_to_connection(&mut self, index: usize, buf: &[u8]) -> bool;
    fn close_connection(&mut self, index: usize);

    fn spawn_process(
        &mut self,
        command: Command,
        stdout_buf_len: usize,
        stderr_buf_len: usize,
    ) -> io::Result<usize>;
    fn read_from_process_stdout(&mut self, index: usize, len: usize) -> &[u8];
    fn read_from_process_stderr(&mut self, index: usize, len: usize) -> &[u8];
    fn write_to_process(&mut self, index: usize, buf: &[u8]) -> bool;
    fn kill_process(&mut self, index: usize);
}

pub trait ClientPlatform {
    fn read(&self, len: usize) -> &[u8];
    fn write(&mut self, buf: &[u8]) -> bool;
}