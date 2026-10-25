//! The shell's prompt across a resize. The terminal rewraps its lines to the
//! new width and the shell (SIGWINCH) draws its prompt again – but bash only
//! clears as many rows as the prompt had *before* the rewrap: once the
//! window grew narrower than the prompt, the rows above stay, and each
//! resize leaves another copy ("bebeiz… beize ~ $"). Ghostty and kitty empty
//! the prompt's rows before the shell draws it again; so does Beize's
//! terminal: the rows of the line being edited are emptied, not rewrapped,
//! the cursor keeps its distance to their first row (bash moves by that
//! distance), the shell fills them anew.
//!
//! A shell draws for the size it read when the signal came; a size that
//! follows before its drawing arrives (a drag gives one each frame) puts
//! that drawing on a grid of another width. kitty waits for the drag to
//! pause (`resize_debounce_time`); Beize's terminal waits for the shell
//! instead: a shell at its prompt is told one size at a time, the next once
//! it drew (it takes about a millisecond), the grid meanwhile stays at the
//! size the shell knows.
//!
//! The shell says when it waits at its prompt with the marks of OSC 133
//! (FinalTerm's, as kitty, Ghostty, VTE, iTerm2 and Windows Terminal read
//! them; `/usr/lib/beize/profile` sets them): `A` the prompt starts, `C` a
//! command runs. Only that switch is taken from them; where the prompt
//! starts the grid knows itself (its rows wrap into each other). Without the
//! marks (another shell, a PS1 of one's own) every size goes at once and the
//! lines rewrap, as in Alacritty.

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::Handler as _;

/// What starts every mark: OSC 133; its kind follows.
const MARK: &[u8] = b"\x1b]133;";

#[derive(Default)]
pub struct Prompt {
    /// The shell waits at its prompt.
    waiting: bool,
    /// How much of `MARK` the output ended with so far.
    matched: usize,
    /// The shell was told a size and has not drawn since.
    told: bool,
}

impl Prompt {
    /// Takes note of the shell's output, before the terminal reads it. True
    /// when it is the shell's answer to a size: the next may follow.
    pub fn output(&mut self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return false;
        }
        for &byte in bytes {
            if self.matched == MARK.len() {
                match byte {
                    b'A' => self.waiting = true,
                    b'C' => self.waiting = false,
                    _ => {}
                }
                self.matched = 0;
            }
            self.matched = if byte == MARK[self.matched] {
                self.matched + 1
            } else {
                usize::from(byte == MARK[0])
            };
        }
        // Any output: a shell that draws something else than its prompt
        // (bash's Ctrl+R) does not hold the window's size.
        std::mem::take(&mut self.told)
    }

    /// The shell has not yet drawn for the last size: the next waits.
    pub fn answering(&self) -> bool {
        self.told
    }

    /// Resizes `term`, a waiting shell's prompt emptied for it to draw
    /// again; the caller tells the shell the size.
    pub fn resize<L: EventListener, S: Dimensions>(&mut self, term: &mut Term<L>, size: S) {
        let at_prompt = self.waiting && !term.mode().contains(TermMode::ALT_SCREEN);
        let down = if at_prompt { empty(term) } else { None };
        term.resize(size);
        for _ in 0..down.unwrap_or(0) {
            term.linefeed();
        }
        self.told = at_prompt;
    }
}

/// Empties the rows from the prompt's first to the bottom and puts the
/// cursor at the start of the first; returns how far down it was.
fn empty<L: EventListener>(term: &mut Term<L>) -> Option<usize> {
    let grid = term.grid_mut();
    let cursor = grid.cursor.point.line;
    let first = first_row(grid, cursor);
    // A prompt taller than the window: its top is in the scrollback, where
    // no cursor goes; it rewraps.
    if first.0 < 0 {
        return None;
    }
    let bottom = grid.bottommost_line();
    let blank = Cell::default();
    let mut line = first;
    while line <= bottom {
        grid[line].reset(&blank);
        line += 1;
    }
    grid.cursor.point = Point::new(first, Column(0));
    grid.cursor.input_needs_wrap = false;
    Some((cursor - first).0 as usize)
}

/// The first row of the line that `row` is part of: the rows above that
/// wrap into the next.
fn first_row(grid: &Grid<Cell>, row: Line) -> Line {
    let last = Column(grid.columns() - 1);
    let mut first = row;
    while first > grid.topmost_line() && grid[first - 1i32][last].flags.contains(Flags::WRAPLINE) {
        first -= 1;
    }
    first
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::CommandExt as _;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::termios::Winsize;

    use super::*;
    use crate::Size;
    use crate::pty::Pty;

    const PROMPT: &str = "beize $";

    /// bash on a PTY and a grid, sized as `App` does.
    struct Shell {
        pty: Pty,
        term: Term<VoidListener>,
        parser: Processor,
        prompt: Prompt,
        size: Size,
        /// The window's size.
        window: Size,
    }

    impl Shell {
        /// bash without any setup of the system or the user, Beize's marks
        /// in its PS1 and PS0.
        fn start(columns: usize) -> Shell {
            let size = Size { columns, lines: 24 };
            let mut command = Command::new("bash");
            command
                .arg0("bash")
                .args(["--norc", "--noprofile", "-i"])
                .env_clear()
                .env("PS1", format!("\\[\\e]133;A\\a\\]{PROMPT} "))
                .env("PS0", "\\e]133;C\\a");
            let pty = Pty::start(command, winsize(size)).expect("starting bash");
            let mut shell = Shell {
                pty,
                term: Term::new(Config::default(), &size, VoidListener),
                parser: Processor::new(),
                prompt: Prompt::default(),
                size,
                window: size,
            };
            shell.wait_for_prompt();
            shell
        }

        /// The window gets `columns`; `wait`: until bash drew its prompt
        /// in it.
        fn drag(&mut self, columns: usize, wait: bool) {
            self.window = Size { columns, lines: 24 };
            self.resize();
            if wait {
                self.wait_for_prompt();
            }
        }

        fn resize(&mut self) {
            if self.prompt.answering() || self.window == self.size {
                return;
            }
            self.size = self.window;
            self.prompt.resize(&mut self.term, self.size);
            self.pty.resize(winsize(self.size)).unwrap();
        }

        /// Reads until the grid has the window's size and the line at the
        /// cursor is the prompt alone.
        fn wait_for_prompt(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.size != self.window || self.line_at_cursor() != PROMPT {
                let left = deadline.saturating_duration_since(Instant::now());
                assert!(!left.is_zero(), "no prompt, but {:?}", self.text());
                let timeout = Timespec::try_from(left).unwrap();
                let output = self.pty.output();
                let mut fds = [PollFd::new(&output, PollFlags::IN)];
                poll(&mut fds, Some(&timeout)).unwrap();
                let mut output = Vec::new();
                self.pty.read(&mut output, 1 << 16).unwrap();
                let answered = self.prompt.output(&output);
                self.parser.advance(&mut self.term, &output);
                if answered {
                    self.resize();
                }
            }
        }

        /// The logical lines with text, from the scrollback down.
        fn text(&self) -> Vec<String> {
            let grid = self.term.grid();
            let last = Column(grid.columns() - 1);
            let mut lines = vec![String::new()];
            let mut line = grid.topmost_line();
            while line <= grid.bottommost_line() {
                let row = &grid[line];
                let text = lines.last_mut().unwrap();
                text.extend((0..grid.columns()).map(|c| row[Column(c)].c));
                if !row[last].flags.contains(Flags::WRAPLINE) {
                    lines.push(String::new());
                }
                line += 1;
            }
            lines
                .into_iter()
                .map(|l| l.trim_end().to_owned())
                .filter(|l| !l.is_empty())
                .collect()
        }

        fn line_at_cursor(&self) -> String {
            let grid = self.term.grid();
            let last = Column(grid.columns() - 1);
            let mut text = String::new();
            let mut line = first_row(grid, grid.cursor.point.line);
            loop {
                let row = &grid[line];
                text.extend((0..grid.columns()).map(|c| row[Column(c)].c));
                if line == grid.bottommost_line() || !row[last].flags.contains(Flags::WRAPLINE) {
                    break;
                }
                line += 1;
            }
            text.trim_end().to_owned()
        }
    }

    fn winsize(size: Size) -> Winsize {
        Winsize {
            ws_row: size.lines as u16,
            ws_col: size.columns as u16,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }

    #[test]
    fn bash_keeps_one_prompt_through_resizes() {
        let mut shell = Shell::start(80);
        // Narrower than the prompt and wide again, some sizes following
        // before bash drew the last: the owner's drag on 0.5.104.
        for (columns, wait) in [
            (40, true),
            (4, false),
            (2, true),
            (3, true),
            (60, false),
            (5, true),
            (80, true),
        ] {
            shell.drag(columns, wait);
        }
        assert_eq!(shell.text(), [PROMPT]);
    }

    #[test]
    fn a_mark_split_between_reads_counts() {
        let mut prompt = Prompt::default();
        prompt.output(b"output\x1b]13");
        prompt.output(b"3;A\x07");
        assert!(prompt.waiting);
        prompt.output(b"\x1b\x1b]133;C\x07");
        assert!(!prompt.waiting);
    }
}
