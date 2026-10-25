//! `beize-terminal` – Beize's terminal, the first app on Beize UI (ADR 0027
//! §9). alacritty_terminal keeps the screen (escape sequences, grid,
//! 10 000 lines of scrollback, selection); the PTY, the drawing and the
//! input are Beize's. One thread waits for everything in the window's
//! poll: the compositor, the shell's output, the shell's end.
//!
//! Ctrl+Shift+C copies the selection, Ctrl+Shift+V pastes (ADR 0027 §9,
//! for now); programs may copy too (OSC 52). The window closes when the
//! shell ends; closing the window hangs up on the shell (SIGHUP when the
//! PTY goes).

mod boxes;
mod keys;
mod mouse;
mod prompt;
mod pty;
mod view;

use std::cell::RefCell;
use std::os::fd::{AsFd as _, OwnedFd};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::{ClipboardType, Config, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use beize_ui::{Appearance, CursorShape, Input, Key, KeyState, Keysym, WindowId};
use rustix::termios::Winsize;
use tracing::{debug, info, warn};

use crate::mouse::Mouse;
use crate::prompt::Prompt;
use crate::pty::Pty;
use crate::view::Fonts;

/// JetBrains Mono at 14 px (Beize UI's sample, sharp at the owner's
/// 27"/1440p); regular and bold.
const FONT_SIZE: f32 = 14.0;
const SCROLLBACK: usize = 10_000;
/// The grid a new window opens with: 80×24, as Terminal.app and every
/// terminal since the VT100.
const OPENS_WITH: (usize, usize) = (80, 24);
/// The shell's output read before the next frame is drawn: a fast program
/// (cat of a large file) does not wait for frames, the window still shows
/// its progress.
const READ_LIMIT: usize = 1 << 20;
/// How soon to try again when the PTY takes no more input (a large paste
/// into a busy program).
const WRITE_RETRY: Duration = Duration::from_millis(10);
const TITLE: &str = "Terminal";

fn main() -> ExitCode {
    beize_ui::log::init("BEIZE_TERMINAL_LOG");
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// What alacritty_terminal asks of the app, collected until the loop gets
/// to it.
#[derive(Clone, Default)]
struct Listener(Rc<RefCell<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.borrow_mut().push(event);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Size {
    columns: usize,
    lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

struct App {
    ui: beize_ui::App,
    window: WindowId,
    fonts: Fonts,
    term: Term<Listener>,
    events: Listener,
    parser: Processor,
    prompt: Prompt,
    pty: Pty,
    size: Size,
    mouse: Mouse,
    focused: bool,
    /// For the shell, not yet taken by the PTY.
    outgoing: Vec<u8>,
    /// A paste on its way: the clipboard's pipe and what came so far.
    paste: Option<(OwnedFd, Vec<u8>)>,
    /// Something changed since the last frame.
    dirty: bool,
}

fn run() -> Result<(), String> {
    let mut ui = beize_ui::App::connect().map_err(|e| e.to_string())?;
    let window = ui
        .open(TITLE, "beize-terminal")
        .map_err(|e| e.to_string())?;
    // Its title bar dark, as its content.
    ui.set_appearance(window, Appearance::Dark);
    // Over the text, the text cursor, as macOS' Terminal shows it.
    ui.set_cursor(window, CursorShape::Text);
    let text = ui.text();
    let fonts = Fonts {
        regular: text.cells(FONT_SIZE, 400.0).map_err(|e| e.to_string())?,
        bold: text.cells(FONT_SIZE, 700.0).map_err(|e| e.to_string())?,
    };
    ui.prefer_size(
        window,
        view::size_for(OPENS_WITH.0, OPENS_WITH.1, &fonts.regular),
    );
    info!("gpu {}", ui.gpu_name());
    // The first configure names the size, and the shell starts in it.
    loop {
        let events = ui
            .dispatch(None, &[])
            .map_err(|e| e.to_string())?
            .take(window);
        if events.closed {
            return Ok(());
        }
        if events.configured {
            break;
        }
    }
    let (columns, lines) = view::grid(ui.size(window), &fonts.regular);
    let size = Size { columns, lines };
    let events = Listener::default();
    let config = Config {
        scrolling_history: SCROLLBACK,
        ..Config::default()
    };
    let term = Term::new(config, &size, events.clone());
    let pty = Pty::spawn(winsize(size, &fonts)).map_err(|e| format!("starting the shell: {e}"))?;
    let mut app = App {
        ui,
        window,
        fonts,
        term,
        events,
        parser: Processor::new(),
        prompt: Prompt::default(),
        pty,
        size,
        mouse: Mouse::default(),
        focused: false,
        outgoing: Vec::new(),
        paste: None,
        dirty: true,
    };
    app.run()
}

impl App {
    fn run(&mut self) -> Result<(), String> {
        let mut output = Vec::new();
        loop {
            let now = Instant::now();
            let mut timeout = self
                .parser
                .sync_timeout()
                .sync_timeout()
                .map(|t| t.saturating_duration_since(now));
            if !self.outgoing.is_empty() {
                timeout = Some(timeout.map_or(WRITE_RETRY, |t| t.min(WRITE_RETRY)));
            }
            let mut fds = vec![self.pty.output(), self.pty.ended()];
            fds.extend(self.paste.as_ref().map(|(pipe, _)| pipe.as_fd()));
            let mut dispatched = self.ui.dispatch(timeout, &fds).map_err(|e| e.to_string())?;
            drop(fds);
            let events = dispatched.take(self.window);
            let ready = dispatched.ready;
            if events.closed {
                return Ok(());
            }
            if events.configured {
                self.resize();
            }
            for input in &events.input {
                self.input(input);
            }
            if ready.contains(&0) {
                output.clear();
                let open = self
                    .pty
                    .read(&mut output, READ_LIMIT)
                    .map_err(|e| format!("reading from the shell: {e}"))?;
                let answered = self.prompt.output(&output);
                self.parser.advance(&mut self.term, &output);
                self.dirty |= !output.is_empty();
                if answered {
                    self.resize();
                }
                if !open {
                    debug!("the PTY closed");
                }
            }
            if self
                .parser
                .sync_timeout()
                .sync_timeout()
                .is_some_and(|t| t <= Instant::now())
            {
                // A synchronized update that never ended: show it anyway.
                self.parser.stop_sync(&mut self.term);
                self.dirty = true;
            }
            if ready.contains(&2) {
                self.receive_paste();
            }
            self.answer();
            self.flush()?;
            if ready.contains(&1) {
                let status = self.pty.wait().map_err(|e| e.to_string())?;
                info!("the shell ended: {status}");
                return Ok(());
            }
            if self.dirty {
                self.dirty = false;
                let size = self.ui.size(self.window);
                let scene = view::draw(&self.term, &self.fonts, self.focused, size);
                self.ui
                    .draw(self.window, &scene)
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    /// A new window size: a new grid, and the shell learns of it – a shell
    /// at its prompt once it drew for the last (prompt.rs).
    fn resize(&mut self) {
        self.dirty = true;
        if self.prompt.answering() {
            return;
        }
        let (columns, lines) = view::grid(self.ui.size(self.window), &self.fonts.regular);
        let size = Size { columns, lines };
        if size == self.size {
            return;
        }
        self.size = size;
        self.prompt.resize(&mut self.term, size);
        if let Err(e) = self.pty.resize(winsize(size, &self.fonts)) {
            warn!("telling the shell the new size: {e}");
        }
    }

    fn input(&mut self, input: &Input) {
        match input {
            Input::Key(key) => self.key(key),
            Input::Focus(focused) => {
                self.focused = *focused;
                self.term.is_focused = *focused;
                self.dirty = true;
                if self.term.mode().contains(TermMode::FOCUS_IN_OUT) {
                    let report: &[u8] = if *focused { b"\x1b[I" } else { b"\x1b[O" };
                    self.outgoing.extend_from_slice(report);
                }
            }
            Input::Pointer(pointer) => {
                let mods = self.ui.modifiers();
                self.dirty |= self.mouse.event(
                    pointer,
                    mods,
                    &mut self.term,
                    &self.fonts.regular,
                    &mut self.outgoing,
                );
            }
        }
    }

    fn key(&mut self, key: &Key) {
        if key.state == KeyState::Released {
            return;
        }
        let mods = key.mods;
        if mods.ctrl && mods.shift && !mods.alt && !mods.logo {
            match key.sym {
                Keysym::C | Keysym::c => {
                    if let Some(text) = self.term.selection_to_string() {
                        self.ui.copy(text);
                    }
                    return;
                }
                Keysym::V | Keysym::v => {
                    match self.ui.paste() {
                        Ok(pipe) => self.paste = pipe.map(|p| (p, Vec::new())),
                        Err(e) => warn!("pasting: {e}"),
                    }
                    return;
                }
                _ => {}
            }
        }
        let alt_screen = self.term.mode().contains(TermMode::ALT_SCREEN);
        // Shift with the page keys moves through the scrollback, as in
        // xterm and VTE; on the alternate screen the program gets them.
        if key.mods.shift && !key.mods.ctrl && !alt_screen {
            let scroll = match key.sym {
                Keysym::Page_Up => Some(Scroll::PageUp),
                Keysym::Page_Down => Some(Scroll::PageDown),
                Keysym::Home => Some(Scroll::Top),
                Keysym::End => Some(Scroll::Bottom),
                _ => None,
            };
            if let Some(scroll) = scroll {
                self.term.scroll_display(scroll);
                self.dirty = true;
                return;
            }
        }
        let app_cursor = self.term.mode().contains(TermMode::APP_CURSOR);
        if let Some(bytes) = keys::bytes(key, app_cursor) {
            // Typing goes back to the bottom and ends a selection.
            self.term.scroll_display(Scroll::Bottom);
            self.term.selection = None;
            self.dirty = true;
            self.outgoing.extend_from_slice(&bytes);
        }
    }

    /// Answers what programs asked (cursor position, colours, size) and
    /// takes what alacritty_terminal reports.
    fn answer(&mut self) {
        let events = std::mem::take(&mut *self.events.0.borrow_mut());
        for event in events {
            match event {
                Event::PtyWrite(text) => self.outgoing.extend_from_slice(text.as_bytes()),
                Event::ColorRequest(index, format) => {
                    let color = view::palette(self.term.colors(), index);
                    self.outgoing.extend_from_slice(format(color).as_bytes());
                }
                Event::TextAreaSizeRequest(format) => {
                    let size = window_size(self.size, &self.fonts);
                    self.outgoing.extend_from_slice(format(size).as_bytes());
                }
                Event::ClipboardStore(ClipboardType::Clipboard, text) => self.ui.copy(text),
                Event::Title(title) => self.ui.set_title(self.window, &title),
                Event::ResetTitle => self.ui.set_title(self.window, TITLE),
                Event::MouseCursorDirty | Event::CursorBlinkingChange | Event::Wakeup => {
                    self.dirty = true
                }
                other => debug!("not handled: {other:?}"),
            }
        }
    }

    /// Reads what the clipboard sent; once all is there, types it.
    fn receive_paste(&mut self) {
        let Some((pipe, text)) = &mut self.paste else {
            return;
        };
        let mut chunk = [0; 65536];
        loop {
            match rustix::io::read(&*pipe, &mut chunk) {
                Ok(0) => break,
                Ok(n) => text.extend_from_slice(&chunk[..n]),
                Err(rustix::io::Errno::AGAIN) => return,
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => {
                    warn!("reading the clipboard: {e}");
                    self.paste = None;
                    return;
                }
            }
        }
        let Some((_, text)) = self.paste.take() else {
            return;
        };
        let text = String::from_utf8_lossy(&text);
        let bracketed = self.term.mode().contains(TermMode::BRACKETED_PASTE);
        self.outgoing
            .extend_from_slice(paste_bytes(&text, bracketed).as_bytes());
        self.term.scroll_display(Scroll::Bottom);
        self.term.selection = None;
        self.dirty = true;
    }

    /// Hands the shell what is waiting for it, as much as the PTY takes.
    fn flush(&mut self) -> Result<(), String> {
        if self.outgoing.is_empty() {
            return Ok(());
        }
        let written = self
            .pty
            .write(&self.outgoing)
            .map_err(|e| format!("writing to the shell: {e}"))?;
        self.outgoing.drain(..written);
        Ok(())
    }
}

/// Pasted text as the shell gets it: in brackets when the program asked
/// (it then knows the text is not typed), without ESC, so a paste cannot
/// end the brackets or send sequences of its own; else with line ends as
/// the Return key sends them. As Alacritty and VTE.
fn paste_bytes(text: &str, bracketed: bool) -> String {
    if bracketed {
        format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""))
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r")
    }
}

fn window_size(size: Size, fonts: &Fonts) -> WindowSize {
    WindowSize {
        num_lines: size.lines as u16,
        num_cols: size.columns as u16,
        cell_width: fonts.regular.width as u16,
        cell_height: fonts.regular.height as u16,
    }
}

fn winsize(size: Size, fonts: &Fonts) -> Winsize {
    let w = window_size(size, fonts);
    Winsize {
        ws_row: w.num_lines,
        ws_col: w.num_cols,
        ws_xpixel: w.num_cols.saturating_mul(w.cell_width),
        ws_ypixel: w.num_lines.saturating_mul(w.cell_height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paste_cannot_leave_its_brackets() {
        assert_eq!(
            paste_bytes("ls\x1b[201~rm -rf ~\n", true),
            "\x1b[200~ls[201~rm -rf ~\n\x1b[201~"
        );
        assert_eq!(paste_bytes("a\r\nb\nc", false), "a\rb\rc");
    }
}
