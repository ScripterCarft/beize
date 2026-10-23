//! Frames for other apps' windows (ADR 0028 §2): an app that asks for a
//! frame over xdg-decoration – Qt, Chromium, Electron – gets Beize's title
//! bar from the compositor, the same Beize UI draws on its own windows
//! (beize-draw): the same text, symbols and buttons, drawn the same way
//! into a picture of its own (`VkRenderer::draw_picture`), so the pixels
//! are the same. KWin draws its frames into textures too. Apps that ask
//! for none, or do not use the protocol (GTK, Beize UI), draw their own.
//!
//! The bar lies above the window's content, which keeps its place in the
//! space: the window's frame is its geometry and the bar (`frame`). It is
//! drawn again only when it changes – the title, the width, active or
//! maximized, the pointer on a button – and otherwise shown as it is.
//! Dragged, it moves the window; a double click maximizes it and back; its
//! buttons maximize and close, each on release over it, as Beize UI's.

use std::cell::RefCell;
use std::time::Instant;

use beize_compositor::render::{VkRenderer, VkTexture};
use beize_draw::titlebar::{self, Button, Press};
use beize_draw::{Glyphs, Text, TitleBar};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::texture::{TextureBuffer, TextureRenderElement};
use smithay::desktop::Window;
use smithay::desktop::space::SpaceElement;
use smithay::input::pointer::GrabStartData;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgState;
use smithay::utils::{Logical, Point, Rectangle, Serial, Transform};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::decoration::XdgDecorationHandler;
use smithay::wayland::shell::xdg::{ToplevelSurface, XdgToplevelSurfaceData};
use tracing::warn;

use crate::state::State;

/// The left button (linux/input-event-codes.h): the only one the bar acts on.
const BTN_LEFT: u32 = 0x110;

/// The bar's height, as Beize UI's.
pub const HEIGHT: i32 = titlebar::HEIGHT as i32;

/// What the bars share: the fonts and the glyph atlas, on the GPU as a
/// coverage texture. Made with the first bar.
#[derive(Default)]
pub struct Decorations {
    text: Option<Text>,
    /// The fonts could not be read: said once, no bars are drawn.
    failed: bool,
    glyphs: Glyphs,
    atlas: Option<VkTexture>,
}

/// A window's bar, in its user data.
#[derive(Default)]
struct Bar {
    bar: TitleBar,
    /// The width it was drawn for, and its picture, which the next drawing
    /// reuses.
    width: i32,
    picture: Option<TextureBuffer<VkTexture>>,
    texture: Option<VkTexture>,
    /// The pointer changed it: drawn again.
    stale: bool,
}

fn with_bar<T>(window: &Window, f: impl FnOnce(&mut Bar) -> T) -> T {
    let data = window.user_data();
    data.insert_if_missing(|| RefCell::new(Bar::default()));
    // As `windows::with_place`: inserted just above, borrowed only here.
    match data.get::<RefCell<Bar>>() {
        Some(bar) => f(&mut bar.borrow_mut()),
        None => f(&mut Bar::default()),
    }
}

/// The window has the compositor's frame: it asked for it, and its last
/// commit acknowledged it.
pub fn decorated(window: &Window) -> bool {
    window
        .toplevel()
        .is_some_and(|t| t.current_state().decoration_mode == Some(Mode::ServerSide))
}

/// How much the frame adds above the window's content.
pub fn bar_height(window: &Window) -> i32 {
    if decorated(window) { HEIGHT } else { 0 }
}

/// Where a window's content of `geometry` and its bar lie together.
pub fn frame(window: &Window, geometry: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    let bar = bar_height(window);
    Rectangle::new(
        (geometry.loc.x, geometry.loc.y - bar).into(),
        (geometry.size.w, geometry.size.h + bar).into(),
    )
}

/// What lies under the pointer of a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Content,
    Bar,
}

impl State {
    /// The topmost window under `at`, and whether its content or its bar.
    pub fn window_under(&self, at: Point<f64, Logical>) -> Option<(Window, Hit)> {
        self.space.elements().rev().find_map(|window| {
            let geometry = self.space.element_geometry(window)?;
            if self.bar_at(window, geometry, at).is_some() {
                return Some((window.clone(), Hit::Bar));
            }
            let origin = self.space.element_location(window)? - window.geometry().loc;
            window
                .is_in_input_region(&(at - origin.to_f64()))
                .then(|| (window.clone(), Hit::Content))
        })
    }

    /// `at` on the bar of `window` (content at `geometry`), relative to the
    /// bar's top left.
    fn bar_at(
        &self,
        window: &Window,
        geometry: Rectangle<i32, Logical>,
        at: Point<f64, Logical>,
    ) -> Option<(f64, f64)> {
        if !decorated(window) {
            return None;
        }
        let (x, y) = (
            at.x - f64::from(geometry.loc.x),
            at.y - f64::from(geometry.loc.y),
        );
        let inside =
            x >= 0.0 && x < f64::from(geometry.size.w) && (-f64::from(HEIGHT)..0.0).contains(&y);
        inside.then_some((x, y + f64::from(HEIGHT)))
    }

    /// The pointer moved to `at`: the button under it lights up, the others
    /// go out.
    pub fn bar_hover(&mut self, at: Point<f64, Logical>) {
        let top = self
            .window_under(at)
            .filter(|(_, hit)| *hit == Hit::Bar)
            .map(|(window, _)| window);
        for window in self.space.elements().filter(|w| decorated(w)) {
            let Some(geometry) = self.space.element_geometry(window) else {
                continue;
            };
            let on = (Some(window) == top.as_ref())
                .then(|| self.bar_at(window, geometry, at))
                .flatten();
            with_bar(window, |bar| {
                if bar.bar.hover(geometry.size.w as f32, on) {
                    bar.stale = true;
                }
            });
        }
    }

    /// A press on `window`'s bar, already active: a drag moves it, a double
    /// click maximizes it or back; on a button, the release decides.
    pub fn bar_press(&mut self, window: &Window, button: u32, serial: Serial) {
        let Some(geometry) = self.space.element_geometry(window) else {
            return;
        };
        let Some((x, y)) = self.bar_at(window, geometry, self.pointer) else {
            return;
        };
        if button != BTN_LEFT {
            return;
        }
        let press = with_bar(window, |bar| {
            bar.stale = true;
            bar.bar.press(geometry.size.w as f32, x, y, Instant::now())
        });
        let Some(toplevel) = window.toplevel().cloned() else {
            return;
        };
        match press {
            Press::Button => {}
            Press::Move => {
                let start = GrabStartData {
                    focus: None,
                    button,
                    location: self.pointer,
                };
                self.move_from_bar(window.clone(), start, serial);
            }
            Press::ToggleMaximized => self.toggle_maximized(&toplevel),
        }
    }

    /// The left button came up: a bar's button acts if it went down on it
    /// and the pointer is still there.
    pub fn bar_release(&mut self, button: u32) {
        if button != BTN_LEFT {
            return;
        }
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            let released = with_bar(&window, |bar| {
                let holding = bar.bar.holding();
                bar.stale |= holding;
                holding.then(|| bar.bar.release()).flatten()
            });
            let Some(toplevel) = window.toplevel().cloned() else {
                continue;
            };
            match released {
                Some(Button::Close) => toplevel.send_close(),
                Some(Button::Maximize) => self.toggle_maximized(&toplevel),
                None => {}
            }
        }
    }

    fn toggle_maximized(&mut self, toplevel: &ToplevelSurface) {
        if toplevel
            .current_state()
            .states
            .contains(XdgState::Maximized)
        {
            self.unmaximize(toplevel, None);
        } else {
            self.maximize(toplevel);
        }
    }
}

impl Decorations {
    /// The bars of the windows in `windows` (with their content's
    /// geometry, front to back), drawn where they changed, as elements
    /// for the frame. The atlas starting over invalidates the bars drawn
    /// before in this round: they are drawn again in the next.
    pub fn elements(
        &mut self,
        renderer: &mut VkRenderer,
        windows: &[(Window, Rectangle<i32, Logical>)],
    ) -> Vec<(Window, TextureRenderElement<VkTexture>)> {
        let mut elements = Vec::new();
        let mut drawn = Vec::new();
        for (window, geometry) in windows {
            if !decorated(window) {
                continue;
            }
            let generation = self.glyphs.generation();
            match self.picture(renderer, window, geometry.size.w) {
                Ok(Some(picture)) => {
                    let at = (geometry.loc.x, geometry.loc.y - HEIGHT);
                    elements.push((
                        window.clone(),
                        TextureRenderElement::from_texture_buffer(
                            Point::from(at).to_f64().to_physical(1.0),
                            &picture,
                            None,
                            None,
                            None,
                            Kind::Unspecified,
                        ),
                    ));
                }
                Ok(None) => {}
                Err(e) => warn!("drawing a title bar: {e}"),
            }
            if self.glyphs.generation() != generation {
                for earlier in &drawn {
                    with_bar(earlier, |bar: &mut Bar| bar.stale = true);
                }
            }
            drawn.push(window.clone());
        }
        elements
    }

    /// The bar's picture, drawn again if it changed; None without fonts.
    fn picture(
        &mut self,
        renderer: &mut VkRenderer,
        window: &Window,
        width: i32,
    ) -> beize_compositor::error::Result<Option<TextureBuffer<VkTexture>>> {
        let Some(toplevel) = window.toplevel() else {
            return Ok(None);
        };
        let states = toplevel.current_state().states;
        let title = with_states(toplevel.wl_surface(), |s| {
            s.data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|d| d.lock().ok().and_then(|d| d.title.clone()))
        })
        .unwrap_or_default();
        let active = states.contains(XdgState::Activated);
        let maximized = states.contains(XdgState::Maximized);
        let changed = with_bar(window, |bar| {
            let changed = bar.stale
                || bar.picture.is_none()
                || bar.width != width
                || bar.bar.title != title
                || bar.bar.active != active
                || bar.bar.maximized != maximized;
            bar.bar.title = title;
            bar.bar.active = active;
            bar.bar.maximized = maximized;
            changed
        });
        if !changed {
            return Ok(with_bar(window, |bar| bar.picture.clone()));
        }
        if width <= 0 {
            return Ok(None);
        }
        let Some(text) = self.text() else {
            return Ok(None);
        };
        let scene = with_bar(window, |bar| bar.bar.draw(text, width as f32));
        let atlas = match &self.atlas {
            Some(atlas) => atlas.clone(),
            None => {
                let atlas = renderer.coverage_texture(beize_draw::glyphs::ATLAS_SIZE)?;
                self.atlas = Some(atlas.clone());
                atlas
            }
        };
        let quads = scene
            .quads(&mut self.glyphs, atlas.slot())
            .ok_or_else(|| beize_compositor::error::error("the bar's glyphs do not fit"))?;
        for (placed, coverage) in self.glyphs.take_uploads() {
            let rect = Rectangle::new(
                (placed.x as i32, placed.y as i32).into(),
                (placed.w as i32, placed.h as i32).into(),
            );
            renderer.write_coverage(&atlas, rect, &coverage)?;
        }
        let size = (width, HEIGHT).into();
        let old = with_bar(window, |bar| bar.texture.clone());
        let texture = renderer.draw_picture(size, quads, scene.blend().to_vec(), old)?;
        // Opaque: the bar fills its rectangle (round corners come with b4).
        let picture = TextureBuffer::from_texture(
            renderer,
            texture.clone(),
            1,
            Transform::Normal,
            Some(vec![Rectangle::from_size(size)]),
        );
        with_bar(window, |bar| {
            bar.width = width;
            bar.stale = false;
            bar.picture = Some(picture.clone());
            bar.texture = Some(texture);
        });
        Ok(Some(picture))
    }

    fn text(&mut self) -> Option<&mut Text> {
        if self.text.is_none() && !self.failed {
            match Text::new() {
                Ok(text) => self.text = Some(text),
                Err(e) => {
                    warn!("the fonts for title bars: {e}; drawing none");
                    self.failed = true;
                }
            }
        }
        self.text.as_mut()
    }
}

/// Apps that ask get the compositor's frame; one that leaves the choice to
/// it gets it too, as KWin decides. Beize UI and GTK ask for none (they do
/// not use the protocol) and draw their own.
impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.set_decoration(&toplevel, Mode::ServerSide);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        self.set_decoration(&toplevel, mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.set_decoration(&toplevel, Mode::ServerSide);
    }
}

impl State {
    fn set_decoration(&mut self, toplevel: &ToplevelSurface, mode: Mode) {
        let area = self.window_area();
        let bar = if mode == Mode::ServerSide { HEIGHT } else { 0 };
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(mode);
            // What is left for the content, below the bar.
            state.bounds = Some((area.size.w, area.size.h - bar).into());
            if state.states.contains(XdgState::Maximized) {
                state.size = Some((area.size.w, area.size.h - bar).into());
            }
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}
