//! Windows opening and closing (ADR 0032 §2, §5): a window grows in from
//! 94 % as it fades in, and shrinks to 94 % as it fades out, as on macOS 26
//! – the model first: it has the keyboard and takes clicks from its first
//! frame on, and once closed, it is gone from the space at once; only its
//! picture is left to fade.
//!
//! A window that opens is drawn into a picture of its own each frame
//! (`Picture`), as niri draws one offscreen, and the picture is shown,
//! scaled about its frame's centre and faded: one image, so its title bar,
//! content and shadow move as one and nothing behind shows through where
//! they meet. Its surfaces count as shown – the picture's states join the
//! frame's –, so its app draws at the full pace while it opens. A window
//! that closes is drawn into its picture once, before its buffers go (its
//! commit without a buffer, or its toplevel destroyed); its buffers are
//! kept until that drawing is done, so its app cannot draw into them
//! before (`syncobj::keep`).
//!
//! The shell's dialogs (it marks them, `beize_shell_v1.dialog`) appear as
//! windows open, live – their glass is blurred anew each frame from what
//! lies behind it, it cannot be a picture: their content, glass and its
//! shadows are drawn scaled and faded (`Pose`). Closed, their content is
//! drawn into a picture and their glass's shapes kept (`backdrop::Ghost`),
//! and both fade out. A wrong password shakes the dialog: a push to the
//! side on a spring that swings back, its first swing 10 px.
//!
//! A window minimized goes into its app's symbol in the dock as a genie
//! (`genie`), and comes back out of it; a window an app starts from the
//! dock grows out of it. Going, it is drawn into its picture once, before it
//! leaves the screen – the model: it is gone at once, a click hits what lay
//! below – and its picture bends into the symbol in front of the windows
//! that lay behind it. Coming back, it is on the screen again and its
//! picture is drawn each frame, as when it opens. Its shadow fades as it
//! starts to bend: it belongs to a rectangle.
//!
//! A window snapped, maximized or back from either changes its place with
//! the commit of its new size (ADR 0028 §2); from that commit its frame
//! springs from the old rectangle to the new, and its picture from before,
//! taken as the configure went out (gnome-shell clones it so), fades into
//! the new, both stretched to the frame as it moves. The model has the new
//! place at once: clicks hit it there.
//!
//! Each runs on a spring (`beize_draw::spring`), from the vblank before the
//! first frame that shows it, and says in the journal when it begins and
//! ends (§7). `BEIZE_ANIMATIONS_SLOWER` makes every spring that many times
//! longer, for the smoke test, as GNOME's `slow-down-factor`.

use std::time::Duration;

use beize_draw::{Motion, Spring};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::element::{Element, Id, RenderElement, RenderElementStates};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{Bind, Frame, RendererSuper, Texture};
use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Serial, Transform};
use smithay::wayland::compositor::with_states;
use tracing::{info, warn};

use crate::backdrop::{Backdrop, Ghost};
use crate::corners::{Effects, Shadow};
use crate::error::{Result, error};
use crate::genie::{self, END};
use crate::render::geometry::Mesh;
use crate::render::{Target, VkRenderer, VkTexture};
use crate::scene;
use crate::syncobj::Held;

mod dialog;
mod minimize;
mod picture;
mod reshape;
mod window;

pub use dialog::{centre, grows, is_dialog, mark_dialog, mark_grows};
#[cfg(test)]
use minimize::genie_unseen;
use picture::scaled;
use reshape::{edges_of, stretched};

/// Opening in place and closing, closing faster (macOS 26; gnome-shell
/// takes 150 ms). 0.3 / 0.25 until 0.5.130: too slow on hardware (the owner).
const OPEN: Spring = Spring::new(0.25, 0.0);
const CLOSE: Spring = Spring::new(0.2, 0.0);
/// A dialog appears with a light pop and goes in a quick fade (ADR 0032
/// §5). 0.3 / 0.2 until 0.5.130: heavy on hardware (the owner).
const DIALOG_OPEN: Spring = Spring::new(0.2, 0.0);
const DIALOG_CLOSE: Spring = Spring::new(0.15, 0.0);
/// A menu appears at once and fades out as it goes (macOS); 0.2 until
/// 0.5.130.
const MENU_CLOSE: Spring = Spring::new(0.15, 0.0);
/// The control center appears under its button as a menu does, with a
/// small pop (from 97 %, `State::grown_from`), and fades back. Until
/// 0.5.130 it grew out of the button, 0.4 / 0.15 swinging past: "like a
/// window out of the corner" (the owner on hardware; macOS' does not).
const GROW: Spring = Spring::new(0.2, 0.0);
const SHRINK_BACK: Spring = Spring::new(0.15, 0.0);
/// The shake after a wrong password: its first swing to the left, 10 px
/// (some 3 % of a dialog's width), then 0.37 of the one before each half
/// swing (macOS' authorization dialog).
const SHAKE: Spring = Spring::new(0.15, 0.7);
const SHAKE_REACH: f64 = 10.0;
/// A place moves less than a quarter of a pixel: the shake has ended.
const SHAKE_UNSEEN: f64 = 0.25;
/// Into the dock and back, and opening out of it: the farthest way
/// (ADR 0032 §5; gnome-shell minimizes in 400 ms).
const GENIE: Spring = Spring::new(0.55, 0.0);
/// A place moves less than a quarter of a pixel: the genie has ended.
const GENIE_UNSEEN: f64 = 0.25;
/// Where a window coming back aims: past its place by as much as one going
/// aims past its symbol (1 for `END`). It ends on reaching its place, still
/// moving, as going ends in the symbol: both take the same third of a second
/// (0.5.130 on hardware: aimed at its place, it crept the last pixels for
/// 0.6 s more, its shadow already there around the bent picture).
const BACK: f64 = END - 1.0;
/// Snapping and maximizing: the frame from the old rectangle to the new,
/// and the old picture fading into the new (gnome-shell 250 ms with a
/// clone, Windows 11): its picture is stretched meanwhile, so short.
/// 0.4 / 0.25 until 0.5.130.
const RESHAPE: Spring = Spring::new(0.3, 0.0);
const CROSSFADE: Spring = Spring::new(0.2, 0.0);
/// An edge moves less than a quarter of a pixel: the frame has ended.
const EDGE_UNSEEN: f64 = 0.25;
/// The size a window opens from and closes to, of its own.
const SMALL: f64 = 0.94;
/// The switcher and the snap preview fade in, without a scale: shown at
/// once until 0.5.130, they felt hard (the owner on hardware).
const FADE_IN: Spring = Spring::new(0.1, 0.0);
/// Below half an 8-bit step of its opacity nothing more shows: there the
/// motion ends. Its size moves less: 6 % of a window's half.
const UNSEEN: f64 = 0.5 / 255.0;

/// The windows opening and the pictures of those closing.
pub struct Animations {
    /// Every spring this many times longer (the smoke test's).
    slower: f64,
    opening: Vec<Opening>,
    closing: Vec<Closing>,
    dialogs: Vec<Dialog>,
    dialogs_closing: Vec<DialogClosing>,
    genies: Vec<Genie>,
    reshapes: Vec<Reshape>,
    /// Buffers of reshapes stopped, until the next frame went to the GPU.
    released: Vec<Held>,
    /// The snap preview fading in.
    preview: Motion,
}

/// A window snapped or maximized, or back: asked for its new size, then,
/// from the commit that has it, moving to its new place.
struct Reshape {
    window: Window,
    app: String,
    /// The window as it was when its new size was asked for, and its frame
    /// then.
    old: Picture,
    old_frame: Rectangle<i32, Physical>,
    /// Its buffers, until that picture is drawn.
    kept: Vec<Held>,
    /// The configure with the new size: the commit that acknowledges it
    /// moves the frame. None once it did.
    asked: Option<Serial>,
    /// The frame's edges as shown – left, top, right, bottom –, from its
    /// first commit in the new place on.
    edges: Option<[Motion; 4]>,
    /// Where the frame goes, the model's.
    frame: Rectangle<i32, Physical>,
    /// New springs for the edges and the crossfade with the next frame.
    aim: bool,
    fade_aim: bool,
    /// How much of the old picture shows over the new.
    fade: Motion,
    /// The window as it is, drawn anew each frame.
    live: Picture,
}

/// A window going into its app's symbol, or coming out of it.
struct Genie {
    window: Window,
    app: String,
    /// 0 the window where it is, 1 in its symbol.
    p: Motion,
    /// Where it goes: 1 into the symbol, `BACK` out of it.
    toward: f64,
    /// Its spring is set with the next frame.
    started: bool,
    /// The symbol, on the screen.
    icon: Rectangle<i32, Physical>,
    picture: Picture,
    /// Coming out: on the screen, its picture drawn anew each frame.
    live: bool,
    /// Going: its shadow as it was, the windows that lay behind it, its
    /// buffers until its picture is drawn, and whether it is in this frame
    /// already.
    shadow: Option<Shadow>,
    behind: Vec<Window>,
    kept: Vec<Held>,
    placed: bool,
    /// A new window growing out of the symbol: the journal says "opened".
    opening: bool,
    id: Id,
    commit: CommitCounter,
}

/// A dialog of the shell's appearing or shaking.
struct Dialog {
    surface: WlSurface,
    /// "dialog" or "control center", for the journal.
    what: &'static str,
    /// How it appears: from `small` of its size about `about` (a dialog:
    /// about its centre), on `open`.
    small: f64,
    about: Option<Point<f64, Physical>>,
    open: Spring,
    shown: Motion,
    started: bool,
    appearing: bool,
    /// Its place to the side.
    shake: Motion,
    shaking: bool,
    /// A push asked for, given with the next frame.
    kick: bool,
}

/// A dialog or a menu closed: its content's picture and its glass, fading
/// out where it lay (`place`); a dialog shrinks as it fades.
struct DialogClosing {
    /// "dialog", "menu" or "control center", for the journal.
    what: &'static str,
    /// It goes to `small` of its size about `about` (none: its centre), on
    /// `spring`.
    small: f64,
    about: Option<Point<f64, Physical>>,
    spring: Spring,
    shown: Motion,
    started: bool,
    picture: Picture,
    ghost: Option<Ghost>,
    place: Rectangle<i32, Logical>,
    kept: Vec<Held>,
}

/// How a dialog shows while it moves: scaled by `scale` about `about`,
/// faded to `opacity`, `shift` pixels to the side.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub scale: f64,
    pub about: Point<f64, Physical>,
    pub opacity: f32,
    pub shift: i32,
}

/// Marks a popup of the shell's as growing out of its button
/// (`beize_shell_v1.grows`).
struct GrowsMark;

/// Marks a surface of the shell's as a dialog (`beize_shell_v1.dialog`).
struct DialogMark;

struct Opening {
    window: Window,
    app: String,
    /// How far it is shown: 0 not at all, 1 whole.
    shown: Motion,
    started: bool,
    picture: Picture,
}

struct Closing {
    app: String,
    shown: Motion,
    started: bool,
    picture: Picture,
    /// The windows that lay behind it: it is drawn in front of them.
    behind: Vec<Window>,
    /// Its buffers, until its picture is drawn.
    kept: Vec<Held>,
    /// In this frame already.
    placed: bool,
}

impl Animations {
    /// With the test's factor from the environment.
    pub fn from_environment() -> Animations {
        let slower = std::env::var("BEIZE_ANIMATIONS_SLOWER")
            .ok()
            .and_then(|factor| factor.trim().parse::<f64>().ok())
            .filter(|factor| factor.is_finite() && *factor >= 1.0)
            .unwrap_or(1.0);
        if slower > 1.0 {
            info!("animations {slower} times slower");
        }
        Animations::slower(slower)
    }

    /// Every spring `slower` times longer.
    fn slower(slower: f64) -> Animations {
        Animations {
            slower,
            opening: Vec::new(),
            closing: Vec::new(),
            dialogs: Vec::new(),
            dialogs_closing: Vec::new(),
            genies: Vec::new(),
            reshapes: Vec::new(),
            released: Vec::new(),
            preview: Motion::new(1.0, UNSEEN),
        }
    }

    fn spring(&self, spring: Spring) -> Spring {
        Spring::new(spring.duration * self.slower, spring.bounce)
    }

    /// Before a frame shown at `at`: those new start a refresh before it,
    /// so its first frame shows them under way (`refresh`).
    pub fn start(&mut self, at: Duration, refresh: Duration) {
        let from = at.saturating_sub(refresh);
        let (open, close) = (self.spring(OPEN), self.spring(CLOSE));
        for opening in self.opening.iter_mut().filter(|o| !o.started) {
            opening.started = true;
            opening.shown.to(1.0, open, from);
        }
        for closing in &mut self.closing {
            closing.placed = false;
            if !closing.started {
                closing.started = true;
                closing.shown.to(0.0, close, from);
            }
        }
        let slower = self.slower;
        let longer = |spring: Spring| Spring::new(spring.duration * slower, spring.bounce);
        let shake = self.spring(SHAKE);
        for dialog in &mut self.dialogs {
            if !dialog.started {
                dialog.started = true;
                dialog.shown.to(1.0, longer(dialog.open), from);
            }
            if dialog.kick {
                dialog.kick = false;
                dialog.shake.kick(-shake.kick_for(SHAKE_REACH), shake, from);
            }
        }
        for closing in self.dialogs_closing.iter_mut().filter(|c| !c.started) {
            closing.started = true;
            closing.shown.to(0.0, longer(closing.spring), from);
        }
        let genie = self.spring(GENIE);
        for g in &mut self.genies {
            g.placed = false;
            if !g.started {
                g.started = true;
                g.p.to(g.toward, genie, from);
            }
        }
        let (reshape, crossfade) = (self.spring(RESHAPE), self.spring(CROSSFADE));
        for r in &mut self.reshapes {
            if r.aim
                && let Some(edges) = &mut r.edges
            {
                r.aim = false;
                for (edge, to) in edges.iter_mut().zip(edges_of(r.frame)) {
                    edge.to(to, reshape, from);
                }
            }
            if r.fade_aim {
                r.fade_aim = false;
                r.fade.to(0.0, crossfade, from);
            }
        }
    }

    /// What a frame shown `at` draws of `window`, its frame at `frame`: its
    /// `own` elements and its `shadow`, or while it opens its picture of
    /// them, while it comes out of its symbol its picture of them bent,
    /// while it moves to a new place its picture of them stretched to the
    /// frame under the old one fading – their states joining `states`.
    #[allow(clippy::too_many_arguments)]
    pub fn shown(
        &mut self,
        renderer: &mut VkRenderer,
        window: &Window,
        own: Vec<scene::Element>,
        shadow: Option<Shadow>,
        frame: Rectangle<i32, Physical>,
        at: Duration,
        states: &mut RenderElementStates,
    ) -> Vec<scene::Element> {
        let anchor = Point::from((
            f64::from(frame.loc.x) + f64::from(frame.size.w) / 2.0,
            f64::from(frame.loc.y) + f64::from(frame.size.h) / 2.0,
        ));
        if let Some(i) = self
            .reshapes
            .iter()
            .position(|r| r.window == *window && r.shows())
        {
            let reshape = &mut self.reshapes[i];
            // The model moved again without a commit (the panels changed
            // the area): there, from where the frame is.
            if frame != reshape.frame {
                reshape.frame = frame;
                reshape.aim = true;
            }
            let shown = reshape.shown(at).unwrap_or(frame);
            let fade = reshape.fade.at(at).clamp(0.0, 1.0) as f32;
            let own = own.into_iter().chain(shadow.map(scene::Element::Shadow));
            return match reshape.live.draw(renderer, own.collect(), anchor) {
                Ok(drawn) => {
                    for (id, state) in drawn.states {
                        states.states.entry(id).or_insert(state);
                    }
                    let old = &reshape.old;
                    let old = (fade > 0.0)
                        .then(|| {
                            old.element_at(stretched(old.place, reshape.old_frame, shown), fade)
                        })
                        .flatten();
                    let live = &reshape.live;
                    let live = live.element_at(stretched(live.place, frame, shown), 1.0);
                    old.into_iter()
                        .chain(live)
                        .map(scene::Element::Picture)
                        .collect()
                }
                // Shown as it is from the next frame on.
                Err(e) => {
                    warn!("animation: {} moves without its picture: {e}", reshape.app);
                    let gone = self.reshapes.remove(i);
                    self.released.extend(gone.kept);
                    Vec::new()
                }
            };
        }
        if let Some(i) = self
            .genies
            .iter()
            .position(|g| g.live && g.window == *window)
        {
            let genie = &mut self.genies[i];
            let p = genie.p.at(at);
            return match genie.picture.draw(renderer, own, anchor) {
                Ok(drawn) => {
                    for (id, state) in drawn.states {
                        states.states.entry(id).or_insert(state);
                    }
                    genie.elements(p, shadow)
                }
                // Shown as it is from the next frame on.
                Err(e) => {
                    warn!(
                        "animation: {} comes back without its picture: {e}",
                        genie.app
                    );
                    self.genies.remove(i);
                    Vec::new()
                }
            };
        }
        let own: Vec<scene::Element> = own
            .into_iter()
            .chain(shadow.map(scene::Element::Shadow))
            .collect();
        let Some(i) = self.opening.iter().position(|o| o.window == *window) else {
            return own;
        };
        let opening = &mut self.opening[i];
        let shown = opening.shown.at(at);
        if !opening.shown.moving() {
            return own;
        }
        match opening.picture.draw(renderer, own, anchor) {
            Ok(drawn) => {
                for (id, state) in drawn.states {
                    states.states.entry(id).or_insert(state);
                }
                opening
                    .picture
                    .element(shown)
                    .map(scene::Element::Picture)
                    .into_iter()
                    .collect()
            }
            // Shown as it is from the next frame on.
            Err(e) => {
                warn!("animation: {} opens without its picture: {e}", opening.app);
                self.opening.remove(i);
                Vec::new()
            }
        }
    }

    /// The pictures of closed windows, and of those going into their
    /// symbols, a frame shown `at` draws in front of `window` – or, with
    /// None, of all windows left –, each once.
    pub fn closing_before(&mut self, window: Option<&Window>, at: Duration) -> Vec<scene::Element> {
        let mut elements = Vec::new();
        for genie in &mut self.genies {
            if genie.live || genie.placed || window.is_some_and(|w| !genie.behind.contains(w)) {
                continue;
            }
            genie.placed = true;
            let p = genie.p.at(at);
            if !genie.done(p) {
                let shadow = genie.shadow.clone();
                elements.extend(genie.elements(p, shadow));
            }
        }
        for closing in &mut self.closing {
            if closing.placed || window.is_some_and(|w| !closing.behind.contains(w)) {
                continue;
            }
            closing.placed = true;
            let shown = closing.shown.at(at);
            if closing.shown.moving() {
                elements.extend(closing.picture.element(shown).map(scene::Element::Picture));
            }
        }
        elements
    }

    /// Once a frame went to the GPU: the buffers of closed windows, whose
    /// pictures it drew, to be kept until it is done.
    pub fn drawn(&mut self) -> Vec<Held> {
        let windows = self
            .closing
            .iter_mut()
            .flat_map(|c| std::mem::take(&mut c.kept));
        let dialogs = self
            .dialogs_closing
            .iter_mut()
            .flat_map(|c| std::mem::take(&mut c.kept));
        let genies = self
            .genies
            .iter_mut()
            .flat_map(|g| std::mem::take(&mut g.kept));
        let reshapes = self
            .reshapes
            .iter_mut()
            .flat_map(|r| std::mem::take(&mut r.kept));
        let released = std::mem::take(&mut self.released);
        windows
            .chain(dialogs)
            .chain(genies)
            .chain(reshapes)
            .chain(released)
            .collect()
    }

    /// Ends what is done by `at`.
    pub fn end(&mut self, at: Duration) {
        self.opening.retain_mut(|o| {
            o.shown.at(at);
            let going = !o.started || o.shown.moving();
            if !going {
                info!("animation: {} opened", o.app);
            }
            going
        });
        self.closing.retain_mut(|c| {
            c.shown.at(at);
            let going = !c.started || c.shown.moving();
            if !going {
                info!("animation: {} closed", c.app);
            }
            going
        });
        self.dialogs.retain_mut(|d| {
            d.shown.at(at);
            d.shake.at(at);
            if d.appearing && d.started && !d.shown.moving() {
                d.appearing = false;
                info!("animation: {} appeared", d.what);
            }
            if d.shaking && !d.kick && !d.shake.moving() {
                d.shaking = false;
                info!("animation: dialog shook");
            }
            d.appearing || d.shaking
        });
        self.dialogs_closing.retain_mut(|c| {
            c.shown.at(at);
            let going = !c.started || c.shown.moving();
            if !going {
                info!("animation: {} closed", c.what);
            }
            going
        });
        self.genies.retain_mut(|g| {
            let p = g.p.at(at);
            let done = g.done(p);
            if done {
                g.ended();
            }
            !done
        });
        self.reshapes.retain_mut(|r| {
            r.shown(at);
            r.fade.at(at);
            let done = r.done();
            if done {
                info!("animation: {} moved", r.app);
            }
            !done
        });
    }

    /// Without a screen nothing shows them: all end at once (ADR 0032 §3).
    pub fn finish(&mut self) {
        for o in self.opening.drain(..) {
            info!("animation: {} opened", o.app);
        }
        for c in self.closing.drain(..) {
            info!("animation: {} closed", c.app);
        }
        for d in self.dialogs.drain(..) {
            if d.appearing {
                info!("animation: {} appeared", d.what);
            }
            if d.shaking {
                info!("animation: dialog shook");
            }
        }
        for c in self.dialogs_closing.drain(..) {
            info!("animation: {} closed", c.what);
        }
        for g in self.genies.drain(..) {
            g.ended();
        }
        // Waiting for its new size, the window moves at once when it comes.
        for r in self.reshapes.drain(..) {
            if r.shows() {
                info!("animation: {} moved", r.app);
            }
        }
    }

    /// Whether one is under way: it asks for the next frame.
    pub fn moving(&self) -> bool {
        !self.opening.is_empty()
            || !self.closing.is_empty()
            || !self.dialogs.is_empty()
            || !self.dialogs_closing.is_empty()
            || !self.genies.is_empty()
            || self.reshapes.iter().any(Reshape::moving)
            || self.preview.moving()
    }

    /// The snap preview shows anew at `now`: it fades in.
    pub fn preview_appears(&mut self, now: Duration) {
        self.preview.jump(0.0);
        self.preview.to(1.0, self.spring(FADE_IN), now);
    }

    /// How far the snap preview shows at `at`.
    pub fn preview_shown(&mut self, at: Duration) -> f32 {
        self.preview.at(at).clamp(0.0, 1.0) as f32
    }

    /// The switcher shows for the first time: it fades in.
    pub fn fade_in(&mut self, surface: &WlSurface) {
        if self.dialogs.iter().any(|d| d.surface == *surface) {
            return;
        }
        info!("animation: switcher appears");
        self.dialogs.push(Dialog {
            surface: surface.clone(),
            what: "switcher",
            small: 1.0,
            about: None,
            open: FADE_IN,
            shown: Motion::new(0.0, UNSEEN),
            started: false,
            appearing: true,
            shake: Motion::new(0.0, SHAKE_UNSEEN),
            shaking: false,
            kick: false,
        });
    }
}

/// A window's picture: its elements drawn into a texture of the
/// compositor's own (`VkRenderer::picture_target`), where they lay on the
/// screen (`place`), shown scaled about `anchor`.
pub struct Picture {
    target: Option<Target>,
    /// What the target holds, so a picture drawn each frame (a window
    /// coming back, reshaping) draws only what changed – new each frame,
    /// it drew everything (audit of 0.5.130: 2.7-3.2 ms on the GPU a frame
    /// while a window snapped).
    damage: Option<OutputDamageTracker>,
    id: Id,
    commit: CommitCounter,
    place: Rectangle<i32, Physical>,
    anchor: Point<f64, Physical>,
}

/// An element of a dialog as `Pose` shows it: scaled about its origin with
/// the edges rounded (`scaled`), as its glass is (`Pose::rect`). Smithay's
/// `RescaleRenderElement` rounds place and size apart: the content then lay
/// up to a pixel off its glass, a different one each frame, and its edges
/// went back and forth while it grew (0.5.131 on hardware: the confirmation
/// dialogs jerked as they appeared).
pub struct Posed<E> {
    element: E,
    origin: Point<f64, Physical>,
    scale: f64,
}

/// A picture on the screen.
pub struct PictureElement {
    id: Id,
    commit: CommitCounter,
    texture: VkTexture,
    dst: Rectangle<i32, Physical>,
    alpha: f32,
}

/// A picture bent along a mesh (the genie): drawn whole each frame it
/// changes, over all it covers.
pub struct MeshElement {
    id: Id,
    commit: CommitCounter,
    texture: VkTexture,
    mesh: Mesh,
    bounds: Rectangle<i32, Physical>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn a_picture_whole_lies_where_its_window_lay() {
        let place = rect(100, 80, 801, 601);
        let anchor = Point::from((500.5, 380.5));
        assert_eq!(scaled(place, anchor, 1.0), place);
    }

    #[test]
    fn it_grows_from_the_centre_of_its_frame() {
        // A frame 1000 x 500 at (0, 0), its shadow reaching 40 below.
        let place = rect(-40, -40, 1080, 620);
        let anchor = Point::from((500.0, 250.0));
        let small = scaled(place, anchor, SMALL);
        // 94 % about the centre: 3 % of the frame's width off each side.
        assert_eq!(small, rect(-8, -23, 1016, 583));
        let (centre_x, centre_y) = (
            f64::from(small.loc.x) + f64::from(small.size.w) / 2.0,
            f64::from(small.loc.y) + f64::from(small.size.h) / 2.0,
        );
        // The picture's own centre lies lower: the shadow falls down.
        assert!((centre_x - 500.0).abs() < 0.5, "{centre_x}");
        assert!(centre_y > 250.0);
    }

    #[test]
    fn a_window_minimized_ends_in_its_symbol_after_a_third_of_a_second() {
        // ADR 0032 §5: at p = 0.9 it lies in its symbol, 0.62 of the
        // spring's 0.55 s; coming back mirrors it, at its place as soon.
        let (place, icon) = (rect(500, 200, 1200, 800), rect(1200, 1357, 56, 56));
        let mut going = Motion::new(0.0, genie_unseen(place, icon));
        going.to(1.0, GENIE, ms(0));
        let end = (1..2000).find(|&n| going.at(ms(n)) >= END).unwrap_or(0);
        assert!((320..360).contains(&end), "{end} ms");
        let mut back = Motion::new(END, genie_unseen(place, icon));
        back.to(BACK, GENIE, ms(0));
        let landed = (1..3000).find(|&n| back.at(ms(n)) <= 0.0).unwrap_or(0);
        assert!((320..360).contains(&landed), "{landed} ms");
    }

    #[test]
    fn a_dialog_pose_moves_scales_and_stays() {
        let place = rect(100, 100, 400, 200);
        assert_eq!(Pose::still().rect(place), place);
        assert!(Pose::still().is_still());
        let shaken = Pose {
            shift: -10,
            about: Point::from((300.0, 200.0)),
            ..Pose::still()
        };
        assert_eq!(shaken.rect(place), rect(90, 100, 400, 200));
        assert!(!shaken.is_still());
        // Small, about its centre: 94 % of 400 x 200.
        let small = Pose {
            scale: SMALL,
            about: Point::from((300.0, 200.0)),
            ..Pose::still()
        };
        assert_eq!(small.rect(place), rect(112, 106, 376, 188));
    }

    fn closing(animations: &mut Animations) {
        animations.close("app".into(), Picture::new(), None, Vec::new(), Vec::new());
    }

    /// When a closing started at 0 ends, a frame each millisecond.
    fn ends(animations: &mut Animations) -> u64 {
        animations.start(ms(1), ms(1));
        (1..10_000)
            .find(|&n| {
                animations.closing_before(None, ms(n));
                animations.end(ms(n));
                !animations.moving()
            })
            .unwrap_or(u64::MAX)
    }

    #[test]
    fn a_closing_ends_after_its_spring_and_the_test_slows_it() {
        let mut animations = Animations::slower(1.0);
        closing(&mut animations);
        assert!(animations.moving());
        let end = ends(&mut animations);
        // 0.25 s, till half an 8-bit step is left: some 0.35 s.
        assert!((250..500).contains(&end), "{end} ms");
        let mut slower = Animations::slower(10.0);
        closing(&mut slower);
        let slow = ends(&mut slower);
        assert!(
            slow.abs_diff(end * 10) < 20,
            "{slow} ms, not ten times {end} ms"
        );
    }

    #[test]
    fn nothing_shown_ends_all_at_once() {
        let mut animations = Animations::slower(1.0);
        closing(&mut animations);
        animations.start(ms(1), ms(1));
        animations.finish();
        assert!(!animations.moving());
    }

    #[test]
    fn a_closing_is_placed_once_a_frame() {
        let mut animations = Animations::slower(1.0);
        closing(&mut animations);
        animations.start(ms(16), ms(16));
        animations.closing_before(None, ms(16));
        assert!(animations.closing.iter().all(|c| c.placed));
        animations.start(ms(32), ms(16));
        assert!(animations.closing.iter().all(|c| !c.placed));
    }
    #[test]
    fn a_picture_follows_its_frame_stretched() {
        // A frame 400 x 300 at (100, 100), its shadow 20 past each side.
        let (frame, place) = (rect(100, 100, 400, 300), rect(80, 80, 440, 340));
        assert_eq!(stretched(place, frame, frame), place);
        // The frame moved and twice as wide: the shadow's part too.
        let to = rect(10, 50, 800, 300);
        assert_eq!(stretched(place, frame, to), rect(-30, 30, 880, 340));
    }

    #[test]
    fn a_frame_snapped_rests_after_its_spring() {
        // ADR 0032 §5: 0.4 s; a frame's edge 600 px away rests where a
        // quarter pixel is left, some 1.06 times the spring's duration.
        let mut edge = Motion::new(0.0, EDGE_UNSEEN);
        edge.to(600.0, RESHAPE, ms(0));
        let rest = (1..3000)
            .find(|&n| {
                edge.at(ms(n));
                !edge.moving()
            })
            .unwrap_or(0);
        assert!((400..700).contains(&rest), "{rest} ms");
        // The old picture is gone sooner than the frame rests.
        let mut fade = Motion::new(1.0, UNSEEN);
        fade.to(0.0, CROSSFADE, ms(0));
        let gone = (1..3000)
            .find(|&n| {
                fade.at(ms(n));
                !fade.moving()
            })
            .unwrap_or(0);
        assert!(gone < rest, "{gone} ms, the frame {rest} ms");
    }
}
