//! The keyboard layout: the XKB layout localed keeps in /etc/vconsole.conf
//! (`XKBLAYOUT=` and friends, vconsole.conf(5)) – the same file and keys
//! beize-setup and `localectl set-x11-keymap` write (ADR 0017). Read
//! directly instead of asking localed over D-Bus: no bus client in the
//! compositor, and the file is localed's documented store. A change is
//! applied at once (inotify on /etc, as localed replaces the file by
//! renaming), as macOS switches input sources without logging out.

use std::fs;
use std::io::ErrorKind;
use std::os::fd::{AsFd, OwnedFd};

use rustix::fs::inotify;
use smithay::input::keyboard::XkbConfig;
use tracing::warn;

use beize_compositor::error::{Context, Result};

/// Where localed keeps the layout.
const FILE: &str = "/etc/vconsole.conf";

/// The XKB settings, owned (Smithay's `XkbConfig` borrows).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub layout: String,
    pub model: String,
    pub variant: String,
    pub options: String,
}

impl Layout {
    /// The layout localed set, or XKB's default (us) if it set none.
    pub fn current() -> Layout {
        match fs::read_to_string(FILE) {
            Ok(text) => parse(&text),
            Err(e) if e.kind() == ErrorKind::NotFound => Layout::default(),
            Err(e) => {
                warn!("reading {FILE}: {e}; using XKB's default layout");
                Layout::default()
            }
        }
    }

    pub fn xkb(&self) -> XkbConfig<'_> {
        XkbConfig {
            rules: "",
            model: &self.model,
            layout: &self.layout,
            variant: &self.variant,
            options: (!self.options.is_empty()).then(|| self.options.clone()),
        }
    }
}

impl std::fmt::Display for Layout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let layout = if self.layout.is_empty() {
            "us (XKB's default)"
        } else {
            &self.layout
        };
        f.write_str(layout)?;
        if !self.variant.is_empty() {
            write!(f, " ({})", self.variant)?;
        }
        Ok(())
    }
}

/// The XKB keys of an environment-style file as systemd reads it: one
/// `KEY=value` per line, `#` and `;` start comments, values optionally in
/// single or double quotes.
fn parse(text: &str) -> Layout {
    let mut layout = Layout::default();
    for line in text.lines().map(str::trim) {
        if line.starts_with(['#', ';']) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = ['"', '\'']
            .iter()
            .find_map(|q| value.strip_prefix(*q)?.strip_suffix(*q))
            .unwrap_or(value)
            .to_owned();
        match key.trim() {
            "XKBLAYOUT" => layout.layout = value,
            "XKBMODEL" => layout.model = value,
            "XKBVARIANT" => layout.variant = value,
            "XKBOPTIONS" => layout.options = value,
            _ => {}
        }
    }
    layout
}

/// Watches /etc for localed replacing vconsole.conf.
pub struct Watch(OwnedFd);

impl Watch {
    pub fn new() -> Result<Watch> {
        let fd = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK)
            .context("inotify")?;
        inotify::add_watch(
            &fd,
            "/etc",
            inotify::WatchFlags::CLOSE_WRITE | inotify::WatchFlags::MOVED_TO,
        )
        .context("watching /etc")?;
        Ok(Watch(fd))
    }

    /// Reads the pending events; true if vconsole.conf was among them.
    pub fn changed(&self) -> bool {
        let mut buffer = [std::mem::MaybeUninit::uninit(); 4096];
        let mut events = inotify::Reader::new(&self.0, &mut buffer);
        let mut changed = false;
        while let Ok(event) = events.next() {
            changed |= event
                .file_name()
                .is_some_and(|name| name.to_bytes() == b"vconsole.conf");
        }
        changed
    }
}

impl AsFd for Watch {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_localed_writes() {
        // As localed writes it after `localectl set-x11-keymap de pc105 nodeadkeys`.
        let text = "KEYMAP=de-nodeadkeys\nXKBLAYOUT=de\nXKBMODEL=pc105\nXKBVARIANT=nodeadkeys\n";
        assert_eq!(
            parse(text),
            Layout {
                layout: "de".into(),
                model: "pc105".into(),
                variant: "nodeadkeys".into(),
                options: String::new(),
            }
        );
    }

    #[test]
    fn takes_quotes_and_skips_comments() {
        let text = "# keyboard\n; old\nXKBLAYOUT=\"fr\"\nXKBOPTIONS='compose:ralt'\nKEYMAP=fr\n";
        let layout = parse(text);
        assert_eq!(layout.layout, "fr");
        assert_eq!(layout.options, "compose:ralt");
        assert_eq!(layout.xkb().options.as_deref(), Some("compose:ralt"));
    }

    #[test]
    fn without_a_layout_is_xkbs_default() {
        let layout = parse("KEYMAP=us\n");
        assert_eq!(layout, Layout::default());
        assert_eq!(layout.xkb().options, None);
        assert_eq!(layout.to_string(), "us (XKB's default)");
    }
}
