//! The clipboard through wl_data_device (ADR 0027 §9): text only, for
//! now. Copying offers the text; whoever pastes gets it written to a pipe
//! by a short thread of its own, so pasting into the same window cannot
//! wait on itself. Pasting asks for the offer's text and hands the app the
//! pipe's reading end to wait for in its poll.

use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::sync::Arc;

use rustix::pipe::PipeFlags;
use tracing::{debug, warn};
use wayland_client::protocol::{
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::WlDataDeviceManager,
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
    wl_seat::WlSeat,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};

use crate::app::State;

/// Text, in the order a paste asks for it.
const TEXT: [&str; 2] = ["text/plain;charset=utf-8", "text/plain"];

#[derive(Default)]
pub(crate) struct Clipboard {
    manager: Option<WlDataDeviceManager>,
    device: Option<WlDataDevice>,
    /// Offers whose types are still arriving; the selection names one.
    offers: Vec<(WlDataOffer, Vec<String>)>,
    /// What the clipboard holds now, from any app.
    selection: Option<(WlDataOffer, Vec<String>)>,
    /// What this window put there.
    source: Option<(WlDataSource, Arc<str>)>,
}

impl Clipboard {
    pub(crate) fn new(manager: Option<WlDataDeviceManager>) -> Clipboard {
        if manager.is_none() {
            warn!("the compositor has no clipboard (wl_data_device_manager)");
        }
        Clipboard {
            manager,
            ..Clipboard::default()
        }
    }

    pub(crate) fn attach(&mut self, seat: &WlSeat, qh: &QueueHandle<State>) {
        if let Some(manager) = &self.manager {
            self.device = Some(manager.get_data_device(seat, qh, ()));
        }
    }

    /// Puts `text` on the clipboard; `serial` is the input that asked.
    pub(crate) fn copy(&mut self, text: String, serial: u32, qh: &QueueHandle<State>) {
        let (Some(manager), Some(device)) = (&self.manager, &self.device) else {
            return;
        };
        let source = manager.create_data_source(qh, ());
        for mime in TEXT {
            source.offer(mime.into());
        }
        device.set_selection(Some(&source), serial);
        if let Some((old, _)) = self.source.replace((source, text.into())) {
            old.destroy();
        }
    }

    /// The clipboard's text, to be read from the returned pipe until it
    /// ends; None if it holds no text.
    pub(crate) fn paste(&self) -> std::io::Result<Option<OwnedFd>> {
        let Some((offer, types)) = &self.selection else {
            return Ok(None);
        };
        let Some(mime) = TEXT.iter().find(|t| types.iter().any(|m| m == *t)) else {
            return Ok(None);
        };
        let (read, write) = rustix::pipe::pipe_with(PipeFlags::CLOEXEC)?;
        // Only our end: the source writes to its own as it likes.
        rustix::fs::fcntl_setfl(&read, rustix::fs::OFlags::NONBLOCK)?;
        offer.receive((*mime).into(), std::os::fd::AsFd::as_fd(&write));
        // The writing end goes with the request; ours closes, so the read
        // ends when the source has written all.
        drop(write);
        Ok(Some(read))
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut State,
        _: &WlDataDeviceManager,
        _: <WlDataDeviceManager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        state: &mut State,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let clipboard = &mut state.clipboard;
        match event {
            wl_data_device::Event::DataOffer { id } => clipboard.offers.push((id, Vec::new())),
            wl_data_device::Event::Selection { id } => {
                let chosen = id.and_then(|id| {
                    let at = clipboard.offers.iter().position(|(o, _)| *o == id)?;
                    Some(clipboard.offers.swap_remove(at))
                });
                // Offers not chosen (drag and drop, which Beize UI does not
                // take yet) and the old selection go.
                for (offer, _) in clipboard.offers.drain(..) {
                    offer.destroy();
                }
                if let Some((old, _)) = std::mem::replace(&mut clipboard.selection, chosen) {
                    old.destroy();
                }
            }
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(
        state: &mut State,
        offer: &WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event
            && let Some((_, types)) = state.clipboard.offers.iter_mut().find(|(o, _)| o == offer)
        {
            types.push(mime_type);
        }
    }
}

impl Dispatch<WlDataSource, ()> for State {
    fn event(
        state: &mut State,
        source: &WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let clipboard = &mut state.clipboard;
        let ours = clipboard.source.as_ref().filter(|(s, _)| s == source);
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                let Some((_, text)) = ours else {
                    return;
                };
                debug!("sending the clipboard as {mime_type}");
                let text = Arc::clone(text);
                let spawned = std::thread::Builder::new()
                    .name("clipboard".into())
                    .spawn(move || write_all(fd, &text));
                if let Err(e) = spawned {
                    warn!("sending the clipboard: {e}");
                }
            }
            // Another app took the clipboard.
            wl_data_source::Event::Cancelled => {
                if ours.is_some() {
                    clipboard.source = None;
                }
                source.destroy();
            }
            _ => {}
        }
    }
}

fn write_all(fd: OwnedFd, text: &str) {
    let mut pipe = std::fs::File::from(fd);
    if let Err(e) = pipe.write_all(text.as_bytes()) {
        // The reader may stop early (a paste cancelled).
        debug!("writing the clipboard: {e}");
    }
}
