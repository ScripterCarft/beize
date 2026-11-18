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
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::element::{Element, Id, RenderElement, RenderElementStates};
use smithay::backend::renderer::utils::CommitCounter;
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

/// Opening in place and closing (macOS 26; gnome-shell takes 150 ms).
const OPEN: Spring = Spring::new(0.3, 0.0);
const CLOSE: Spring = Spring::new(0.25, 0.0);
/// A dialog appears as a window opens and goes faster (ADR 0032 §5).
const DIALOG_OPEN: Spring = Spring::new(0.3, 0.0);
const DIALOG_CLOSE: Spring = Spring::new(0.2, 0.0);
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
/// Snapping and maximizing: the frame from the old rectangle to the new,
/// and the old picture fading into the new (gnome-shell 250 ms with a
/// clone, Windows 11).
const RESHAPE: Spring = Spring::new(0.4, 0.0);
const CROSSFADE: Spring = Spring::new(0.25, 0.0);
/// An edge moves less than a quarter of a pixel: the frame has ended.
const EDGE_UNSEEN: f64 = 0.25;
/// The size a window opens from and closes to, of its own.
const SMALL: f64 = 0.94;
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

impl Reshape {
    /// The frame as shown at `at`.
    fn shown(&mut self, at: Duration) -> Option<Rectangle<i32, Physical>> {
        let edges = self.edges.as_mut()?;
        let [left, top, right, bottom] = [0, 1, 2, 3].map(|i| edges[i].at(at).round() as i32);
        Some(Rectangle::new(
            (left, top).into(),
            ((right - left).max(1), (bottom - top).max(1)).into(),
        ))
    }

    /// Moving: from the commit in its new place until frame and crossfade
    /// rest.
    fn moving(&self) -> bool {
        self.edges.is_some()
    }

    fn done(&self) -> bool {
        self.asked.is_none()
            && !self.aim
            && !self.fade_aim
            && self
                .edges
                .as_ref()
                .is_some_and(|edges| edges.iter().all(|e| !e.moving()))
            && !self.fade.moving()
    }
}

/// The four edges of `frame`: left, top, right, bottom.
fn edges_of(frame: Rectangle<i32, Physical>) -> [f64; 4] {
    [
        f64::from(frame.loc.x),
        f64::from(frame.loc.y),
        f64::from(frame.loc.x + frame.size.w),
        f64::from(frame.loc.y + frame.size.h),
    ]
}

/// `place`, which lies about the frame `from`, stretched as `from` is to
/// `to`: a picture with its shadow follows the frame.
fn stretched(
    place: Rectangle<i32, Physical>,
    from: Rectangle<i32, Physical>,
    to: Rectangle<i32, Physical>,
) -> Rectangle<i32, Physical> {
    let sx = f64::from(to.size.w) / f64::from(from.size.w.max(1));
    let sy = f64::from(to.size.h) / f64::from(from.size.h.max(1));
    let x = |v: i32| (f64::from(to.loc.x) + f64::from(v - from.loc.x) * sx).round() as i32;
    let y = |v: i32| (f64::from(to.loc.y) + f64::from(v - from.loc.y) * sy).round() as i32;
    let (left, top) = (x(place.loc.x), y(place.loc.y));
    let (right, bottom) = (x(place.loc.x + place.size.w), y(place.loc.y + place.size.h));
    Rectangle::new((left, top).into(), (right - left, bottom - top).into())
}

/// A window going into its app's symbol, or coming out of it.
struct Genie {
    window: Window,
    app: String,
    /// 0 the window where it is, 1 in its symbol.
    p: Motion,
    /// Where it goes: 1 into the symbol, 0 out of it.
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

impl Genie {
    /// Its picture bent as at `p`.
    fn element(&mut self, p: f64) -> Option<MeshElement> {
        let texture = self.picture.target.as_ref()?.picture()?.clone();
        let mesh = genie::Genie::new(self.picture.place, self.icon)?.mesh(p);
        self.commit.increment();
        Some(MeshElement {
            id: self.id.clone(),
            commit: self.commit,
            bounds: genie::bounds(&mesh),
            texture,
            mesh,
        })
    }

    /// What it shows at `p`: its picture bent, its shadow fading.
    fn elements(&mut self, p: f64, shadow: Option<Shadow>) -> Vec<scene::Element> {
        let mut elements: Vec<scene::Element> = self
            .element(p)
            .map(scene::Element::Mesh)
            .into_iter()
            .collect();
        let faded = genie::Genie::shadow(p);
        if faded > 0.0 {
            elements.extend(shadow.map(|s| scene::Element::Shadow(s.faded(faded))));
        }
        elements
    }

    /// Whether it has ended by its last value: going, in its symbol;
    /// coming out, at rest.
    fn done(&self, p: f64) -> bool {
        self.started && (!self.p.moving() || (self.toward >= 1.0 && p >= END))
    }

    fn ended(&self) {
        let app = &self.app;
        match (self.toward >= 1.0, self.opening) {
            (true, _) => info!("animation: {app} minimized"),
            (false, true) => info!("animation: {app} opened"),
            (false, false) => info!("animation: {app} came back"),
        }
    }
}

/// How far a quarter pixel is of the genie from `place` into `icon`.
fn genie_unseen(place: Rectangle<i32, Physical>, icon: Rectangle<i32, Physical>) -> f64 {
    genie::Genie::new(place, icon).map_or(GENIE_UNSEEN, |g| GENIE_UNSEEN / g.longest_way())
}

/// A dialog of the shell's appearing or shaking.
struct Dialog {
    surface: WlSurface,
    shown: Motion,
    started: bool,
    appearing: bool,
    /// Its place to the side.
    shake: Motion,
    shaking: bool,
    /// A push asked for, given with the next frame.
    kick: bool,
}

/// A dialog closed: its content's picture and its glass, fading out where
/// it lay (`place`).
struct DialogClosing {
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

impl Pose {
    /// As it is.
    pub fn still() -> Pose {
        Pose {
            scale: 1.0,
            about: Point::default(),
            opacity: 1.0,
            shift: 0,
        }
    }

    pub fn is_still(&self) -> bool {
        self.scale == 1.0 && self.opacity == 1.0 && self.shift == 0
    }

    /// Where `rect` shows: moved by the shift, scaled about the anchor moved
    /// with it.
    pub fn rect(&self, rect: Rectangle<i32, Physical>) -> Rectangle<i32, Physical> {
        let mut moved = rect;
        moved.loc.x += self.shift;
        scaled(moved, self.origin().to_f64(), self.scale)
    }

    /// The anchor, moved by the shift, on a whole pixel: what a surface's
    /// elements scale about (`RescaleRenderElement`).
    pub fn origin(&self) -> Point<i32, Physical> {
        Point::from((
            (self.about.x + f64::from(self.shift)).round() as i32,
            self.about.y.round() as i32,
        ))
    }
}

/// The centre of `place`, on the screen.
pub fn centre(place: Rectangle<i32, Logical>) -> Point<f64, Physical> {
    let place = place.to_physical(1);
    Point::from((
        f64::from(place.loc.x) + f64::from(place.size.w) / 2.0,
        f64::from(place.loc.y) + f64::from(place.size.h) / 2.0,
    ))
}

/// Marks a surface of the shell's as a dialog (`beize_shell_v1.dialog`).
struct DialogMark;

pub fn mark_dialog(surface: &WlSurface) {
    with_states(surface, |states| {
        states.data_map.insert_if_missing_threadsafe(|| DialogMark);
    });
}

pub fn is_dialog(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        states.data_map.get::<DialogMark>().is_some()
    })
}

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
        }
    }

    /// `window` of `app` was asked for a new size, snapped, maximized or
    /// back (`asked`): its `picture` from before, of its frame at `frame`,
    /// its buffers `kept` until it is drawn. Moving already, it goes on
    /// from where its frame is, and its picture is the new one.
    pub fn reshape(
        &mut self,
        window: &Window,
        app: String,
        picture: Picture,
        frame: Rectangle<i32, Physical>,
        kept: Vec<Held>,
        asked: Serial,
    ) {
        if let Some(reshape) = self.reshapes.iter_mut().find(|r| r.window == *window) {
            reshape.old = picture;
            reshape.old_frame = frame;
            reshape.kept.extend(kept);
            reshape.asked = Some(asked);
            reshape.fade = Motion::new(1.0, UNSEEN);
            reshape.fade_aim = false;
            return;
        }
        self.reshapes.push(Reshape {
            window: window.clone(),
            app,
            old: picture,
            old_frame: frame,
            kept,
            asked: Some(asked),
            edges: None,
            frame,
            aim: false,
            fade_aim: false,
            fade: Motion::new(1.0, UNSEEN),
            live: Picture::new(),
        });
    }

    /// Whether `window` waits for its new place or moves to it.
    pub fn reshaping(&self, window: &Window) -> bool {
        self.reshapes.iter().any(|r| r.window == *window)
    }

    /// `window` committed, its frame now at `frame`, with the configure
    /// `acked` acknowledged: the commit with its new size moves the frame
    /// there from where it was; a frame moving goes on to where the model
    /// has it. An answer in the old place is no move.
    pub fn placed(
        &mut self,
        window: &Window,
        frame: Rectangle<i32, Physical>,
        acked: Option<Serial>,
    ) {
        let Some(i) = self.reshapes.iter().position(|r| r.window == *window) else {
            return;
        };
        let reshape = &mut self.reshapes[i];
        if let Some(asked) = reshape.asked {
            if !acked.is_some_and(|acked| acked.is_no_older_than(&asked)) {
                return;
            }
            reshape.asked = None;
            if reshape.edges.is_none() && frame == reshape.old_frame {
                self.reshapes.remove(i);
                return;
            }
            info!("animation: {} moves", reshape.app);
            if reshape.edges.is_none() {
                reshape.edges =
                    Some(edges_of(reshape.old_frame).map(|e| Motion::new(e, EDGE_UNSEEN)));
            }
            reshape.fade_aim = true;
        }
        if reshape.edges.is_some() && (frame != reshape.frame || reshape.fade_aim) {
            reshape.frame = frame;
            reshape.aim = true;
        }
    }

    /// `window` shows in its model's place at once: taken by the pointer,
    /// or gone.
    pub fn stop_reshape(&mut self, window: &Window) {
        self.reshapes.retain(|r| {
            let stops = r.window == *window;
            if stops && r.moving() {
                info!("animation: {} stopped moving", r.app);
            }
            !stops
        });
    }

    /// `window` of `app` was minimized: drawn into `picture` (without its
    /// shadow, which is `shadow`), it goes into its symbol at `icon` in
    /// front of the windows that lay `behind` it, its buffers `kept` until
    /// the picture is drawn. From where it was coming back, if it was.
    #[allow(clippy::too_many_arguments)]
    pub fn minimize(
        &mut self,
        window: &Window,
        app: String,
        icon: Rectangle<i32, Physical>,
        picture: Picture,
        shadow: Option<Shadow>,
        behind: Vec<Window>,
        kept: Vec<Held>,
    ) {
        self.stop_opening(window);
        self.stop_reshape(window);
        let unseen = genie_unseen(picture.place, icon);
        let p = match self.genies.iter().position(|g| g.window == *window) {
            Some(i) => self.genies.remove(i).p,
            None => Motion::new(0.0, unseen),
        };
        info!("animation: {app} minimizes");
        self.genies.push(Genie {
            window: window.clone(),
            app,
            p,
            toward: 1.0,
            started: false,
            icon,
            picture,
            live: false,
            shadow,
            behind,
            kept,
            placed: false,
            opening: false,
            id: Id::new(),
            commit: CommitCounter::default(),
        });
    }

    /// `window` of `app`, at `place` on the screen, comes back out of its
    /// symbol at `icon` – or, `opening`, a new window grows out of it –,
    /// drawn anew each frame. From where it was going, if it was.
    pub fn come_back(
        &mut self,
        window: &Window,
        app: String,
        place: Rectangle<i32, Physical>,
        icon: Rectangle<i32, Physical>,
        opening: bool,
    ) {
        self.stop_opening(window);
        if opening {
            info!("animation: {app} opens from its symbol");
        } else {
            info!("animation: {app} comes back");
        }
        if let Some(genie) = self.genies.iter_mut().find(|g| g.window == *window) {
            genie.toward = 0.0;
            genie.started = false;
            genie.icon = icon;
            genie.live = true;
            genie.shadow = None;
            genie.behind.clear();
            genie.opening |= opening;
            return;
        }
        self.genies.push(Genie {
            window: window.clone(),
            app,
            p: Motion::new(END, genie_unseen(place, icon)),
            toward: 0.0,
            started: false,
            icon,
            picture: Picture::new(),
            live: true,
            shadow: None,
            behind: Vec::new(),
            kept: Vec::new(),
            placed: false,
            opening,
            id: Id::new(),
            commit: CommitCounter::default(),
        });
    }

    /// Whether `window` grows in from its next frame, not yet under way.
    pub fn opens_next(&self, window: &Window) -> bool {
        self.opening
            .iter()
            .any(|o| o.window == *window && !o.started)
    }

    /// `window` shows as it is at once: no genie of it, going or coming.
    pub fn stop_genie(&mut self, window: &Window) {
        self.genies.retain(|g| {
            let stops = g.window == *window;
            if stops {
                info!("animation: {} stopped its genie", g.app);
            }
            !stops
        });
    }

    fn stop_opening(&mut self, window: &Window) {
        if let Some(i) = self.opening.iter().position(|o| o.window == *window) {
            let opening = self.opening.remove(i);
            info!("animation: {} stopped opening", opening.app);
        }
    }

    /// A dialog of the shell's shows for the first time: it appears from
    /// its next frame.
    pub fn appear(&mut self, surface: &WlSurface) {
        if self.dialogs.iter().any(|d| d.surface == *surface) {
            return;
        }
        info!("animation: dialog appears");
        self.dialogs.push(Dialog {
            surface: surface.clone(),
            shown: Motion::new(0.0, UNSEEN),
            started: false,
            appearing: true,
            shake: Motion::new(0.0, SHAKE_UNSEEN),
            shaking: false,
            kick: false,
        });
    }

    /// The shell asks a dialog to shake: from its next frame.
    pub fn shake(&mut self, surface: &WlSurface) {
        info!("animation: dialog shakes");
        if let Some(dialog) = self.dialogs.iter_mut().find(|d| d.surface == *surface) {
            dialog.kick = true;
            dialog.shaking = true;
            return;
        }
        self.dialogs.push(Dialog {
            surface: surface.clone(),
            shown: Motion::new(1.0, UNSEEN),
            started: true,
            appearing: false,
            shake: Motion::new(0.0, SHAKE_UNSEEN),
            shaking: true,
            kick: true,
        });
    }

    /// How a frame shown `at` draws `surface`, a panel at `place`: as it
    /// is, or as its dialog moves.
    pub fn pose(
        &mut self,
        surface: &WlSurface,
        place: Rectangle<i32, Logical>,
        at: Duration,
    ) -> Pose {
        let Some(dialog) = self.dialogs.iter_mut().find(|d| d.surface == *surface) else {
            return Pose::still();
        };
        let shown = dialog.shown.at(at).clamp(0.0, 1.0);
        let shift = dialog.shake.at(at).round() as i32;
        Pose {
            scale: SMALL + (1.0 - SMALL) * shown,
            about: centre(place),
            opacity: shown as f32,
            shift,
        }
    }

    /// A dialog goes: how far it was shown, for its closing.
    pub fn stop_dialog(&mut self, surface: &WlSurface) -> Option<Motion> {
        let i = self.dialogs.iter().position(|d| d.surface == *surface)?;
        Some(self.dialogs.remove(i).shown)
    }

    /// A dialog closed at `place`: its content drawn into `picture`, its
    /// glass's `ghost`, its buffers `kept` until the picture is drawn.
    pub fn close_dialog(
        &mut self,
        picture: Picture,
        ghost: Option<Ghost>,
        from: Option<Motion>,
        place: Rectangle<i32, Logical>,
        kept: Vec<Held>,
    ) {
        info!("animation: dialog closes");
        self.dialogs_closing.push(DialogClosing {
            shown: from.unwrap_or_else(|| Motion::new(1.0, UNSEEN)),
            started: false,
            picture,
            ghost,
            place,
            kept,
        });
    }

    /// The closed dialogs as a frame shown `at` draws them, front to back,
    /// each with its place: content, then its glass's shadows and glass.
    pub fn dialogs_closing(
        &mut self,
        renderer: &mut VkRenderer,
        mut backdrop: Option<&mut Backdrop>,
        at: Duration,
    ) -> Vec<(Rectangle<i32, Logical>, Vec<scene::Element>)> {
        let mut drawn = Vec::new();
        for closing in self.dialogs_closing.iter_mut().rev() {
            let shown = closing.shown.at(at).clamp(0.0, 1.0);
            if !closing.shown.moving() {
                continue;
            }
            let pose = Pose {
                scale: SMALL + (1.0 - SMALL) * shown,
                about: centre(closing.place),
                opacity: shown as f32,
                shift: 0,
            };
            let mut elements: Vec<scene::Element> = closing
                .picture
                .element(shown)
                .map(scene::Element::Picture)
                .into_iter()
                .collect();
            if let (Some(backdrop), Some(ghost)) = (backdrop.as_deref_mut(), &mut closing.ghost)
                && let Some((glass, shadows)) =
                    backdrop.ghost_glass(renderer, ghost, closing.place.loc, pose)
            {
                elements.extend(shadows.into_iter().map(scene::Element::Shadow));
                elements.push(scene::Element::Glass(glass));
            }
            drawn.push((closing.place, elements));
        }
        drawn
    }

    fn spring(&self, spring: Spring) -> Spring {
        Spring::new(spring.duration * self.slower, spring.bounce)
    }

    /// `window` of `app` was mapped: it grows in from its next frame.
    pub fn open(&mut self, window: &Window, app: String) {
        if self.opening.iter().any(|o| o.window == *window) {
            return;
        }
        info!("animation: {app} opens");
        self.opening.push(Opening {
            window: window.clone(),
            app,
            shown: Motion::new(0.0, UNSEEN),
            started: false,
            picture: Picture::new(),
        });
    }

    /// `window` stops opening or coming out of its symbol (closed,
    /// unmapped): how far it was shown, for what follows. Going into its
    /// symbol it goes on: its picture is its own.
    pub fn stop(&mut self, window: &Window) -> Option<Motion> {
        self.stop_reshape(window);
        self.genies.retain(|g| {
            let stops = g.live && g.window == *window;
            if stops {
                info!("animation: {} stopped coming back", g.app);
            }
            !stops
        });
        let i = self.opening.iter().position(|o| o.window == *window)?;
        let opening = self.opening.remove(i);
        info!("animation: {} stopped opening", opening.app);
        Some(opening.shown)
    }

    /// A window of `app` closed, drawn into `picture` with its buffers
    /// `kept`; it fades from where its opening was (`from`), or from whole.
    pub fn close(
        &mut self,
        app: String,
        picture: Picture,
        from: Option<Motion>,
        behind: Vec<Window>,
        kept: Vec<Held>,
    ) {
        info!("animation: {app} closes");
        self.closing.push(Closing {
            app,
            shown: from.unwrap_or_else(|| Motion::new(1.0, UNSEEN)),
            started: false,
            picture,
            behind,
            kept,
            placed: false,
        });
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
        let (appear, go, shake) = (
            self.spring(DIALOG_OPEN),
            self.spring(DIALOG_CLOSE),
            self.spring(SHAKE),
        );
        for dialog in &mut self.dialogs {
            if !dialog.started {
                dialog.started = true;
                dialog.shown.to(1.0, appear, from);
            }
            if dialog.kick {
                dialog.kick = false;
                dialog.shake.kick(-shake.kick_for(SHAKE_REACH), shake, from);
            }
        }
        for closing in self.dialogs_closing.iter_mut().filter(|c| !c.started) {
            closing.started = true;
            closing.shown.to(0.0, go, from);
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
            .position(|r| r.window == *window && r.moving())
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
                    self.reshapes.remove(i);
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
        windows
            .chain(dialogs)
            .chain(genies)
            .chain(reshapes)
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
                info!("animation: dialog appeared");
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
                info!("animation: dialog closed");
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
                info!("animation: dialog appeared");
            }
            if d.shaking {
                info!("animation: dialog shook");
            }
        }
        for _ in self.dialogs_closing.drain(..) {
            info!("animation: dialog closed");
        }
        for g in self.genies.drain(..) {
            g.ended();
        }
        // Waiting for its new size, the window moves at once when it comes.
        for r in self.reshapes.drain(..) {
            if r.moving() {
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
    }
}

/// A window's picture: its elements drawn into a texture of the
/// compositor's own (`VkRenderer::picture_target`), where they lay on the
/// screen (`place`), shown scaled about `anchor`.
pub struct Picture {
    target: Option<Target>,
    id: Id,
    commit: CommitCounter,
    place: Rectangle<i32, Physical>,
    anchor: Point<f64, Physical>,
}

impl Picture {
    pub fn new() -> Picture {
        Picture {
            target: None,
            id: Id::new(),
            commit: CommitCounter::default(),
            place: Rectangle::default(),
            anchor: Point::default(),
        }
    }

    /// Draws `elements` into it, scaled about `anchor` when shown; the
    /// states of what it drew.
    pub fn draw(
        &mut self,
        renderer: &mut VkRenderer,
        elements: Vec<scene::Element>,
        anchor: Point<f64, Physical>,
    ) -> Result<RenderElementStates> {
        let scale = Scale::from(1.0);
        let place = elements
            .iter()
            .map(|e| e.geometry(scale))
            .reduce(|a, b| a.merge(b))
            .filter(|p| !p.is_empty())
            .ok_or_else(|| error("nothing to draw"))?;
        let size = (place.size.w, place.size.h).into();
        if self.target.as_ref().is_none_or(|t| t.size() != size) {
            self.target = Some(renderer.picture_target(size)?);
        }
        let target = self.target.as_mut().ok_or_else(|| error("no picture"))?;
        let elements: Vec<_> = elements
            .into_iter()
            .map(|e| {
                RelocateRenderElement::from_element(
                    e,
                    Point::default() - place.loc,
                    Relocate::Relative,
                )
            })
            .collect();
        let mut damage =
            OutputDamageTracker::new((place.size.w, place.size.h), 1.0, Transform::Normal);
        let mut framebuffer = renderer.bind(target)?;
        let drawn = damage
            .render_output(renderer, &mut framebuffer, 0, &elements, [0.0; 4])
            .map_err(|e| error(format!("drawing a picture: {e:?}")))?;
        self.place = place;
        self.anchor = anchor;
        self.commit.increment();
        Ok(drawn.states)
    }

    /// Where what it holds lay on the screen when it was drawn.
    pub fn place(&self) -> Rectangle<i32, Physical> {
        self.place
    }

    /// Stretched onto `dst`, at `alpha`.
    pub fn element_at(&self, dst: Rectangle<i32, Physical>, alpha: f32) -> Option<PictureElement> {
        let texture = self.target.as_ref()?.picture()?.clone();
        Some(PictureElement {
            id: self.id.clone(),
            commit: self.commit,
            texture,
            dst,
            alpha,
        })
    }

    /// As shown `shown` (0 to 1): from 94 % to whole, faded in as much.
    pub fn element(&self, shown: f64) -> Option<PictureElement> {
        let texture = self.target.as_ref()?.picture()?.clone();
        let shown = shown.clamp(0.0, 1.0);
        Some(PictureElement {
            id: self.id.clone(),
            commit: self.commit,
            texture,
            dst: scaled(self.place, self.anchor, SMALL + (1.0 - SMALL) * shown),
            alpha: shown as f32,
        })
    }
}

/// `place` scaled by `scale` about `anchor`, its edges on whole pixels.
fn scaled(
    place: Rectangle<i32, Physical>,
    anchor: Point<f64, Physical>,
    scale: f64,
) -> Rectangle<i32, Physical> {
    let x = |v: i32| (anchor.x + (f64::from(v) - anchor.x) * scale).round() as i32;
    let y = |v: i32| (anchor.y + (f64::from(v) - anchor.y) * scale).round() as i32;
    let (left, top) = (x(place.loc.x), y(place.loc.y));
    let (right, bottom) = (x(place.loc.x + place.size.w), y(place.loc.y + place.size.h));
    Rectangle::new((left, top).into(), (right - left, bottom - top).into())
}

/// A picture on the screen.
pub struct PictureElement {
    id: Id,
    commit: CommitCounter,
    texture: VkTexture,
    dst: Rectangle<i32, Physical>,
    alpha: f32,
}

impl Element for PictureElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        let size = self.texture.size();
        Rectangle::from_size((f64::from(size.w), f64::from(size.h)).into())
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.dst
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }
}

impl<R: Effects> RenderElement<R> for PictureElement {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> std::result::Result<(), <R as RendererSuper>::Error> {
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            self.alpha,
        )
    }
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

impl Element for MeshElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        let size = self.texture.size();
        Rectangle::from_size((f64::from(size.w), f64::from(size.h)).into())
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.bounds
    }
}

impl<R: Effects> RenderElement<R> for MeshElement {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        _damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
    ) -> std::result::Result<(), <R as RendererSuper>::Error> {
        // Where `dst` puts it: the same, but for a move between geometry
        // and drawing.
        let (dx, dy) = (
            f64::from(dst.loc.x - self.bounds.loc.x),
            f64::from(dst.loc.y - self.bounds.loc.y),
        );
        if dx == 0.0 && dy == 0.0 {
            R::draw_mesh(frame, &self.texture, &self.mesh, 1.0);
        } else {
            let mut moved = self.mesh.clone();
            for line in &mut moved.lines {
                line.left += dx;
                line.right += dx;
                line.y += dy;
            }
            R::draw_mesh(frame, &self.texture, &moved, 1.0);
        }
        Ok(())
    }
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
        // spring's 0.55 s; coming back, the spring runs till a quarter
        // pixel of the longest way is left.
        let (place, icon) = (rect(500, 200, 1200, 800), rect(1200, 1357, 56, 56));
        let mut going = Motion::new(0.0, genie_unseen(place, icon));
        going.to(1.0, GENIE, ms(0));
        let end = (1..2000).find(|&n| going.at(ms(n)) >= END).unwrap_or(0);
        assert!((320..360).contains(&end), "{end} ms");
        let mut back = Motion::new(END, genie_unseen(place, icon));
        back.to(0.0, GENIE, ms(0));
        let rest = (1..3000)
            .find(|&n| {
                back.at(ms(n));
                !back.moving()
            })
            .unwrap_or(0);
        assert!((800..1100).contains(&rest), "{rest} ms");
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
