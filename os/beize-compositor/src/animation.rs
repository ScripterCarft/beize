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
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Transform};
use tracing::{info, warn};

use crate::corners::Effects;
use crate::error::{Result, error};
use crate::render::{Target, VkRenderer, VkTexture};
use crate::scene;
use crate::syncobj::Held;

/// Opening in place and closing (macOS 26; gnome-shell takes 150 ms).
const OPEN: Spring = Spring::new(0.3, 0.0);
const CLOSE: Spring = Spring::new(0.25, 0.0);
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
        Animations {
            slower,
            opening: Vec::new(),
            closing: Vec::new(),
        }
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

    /// `window` stops opening (closed, minimized, unmapped): how far it
    /// was shown, for what follows.
    pub fn stop(&mut self, window: &Window) -> Option<Motion> {
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
    }

    /// What a frame shown `at` draws of `window`: its `own` elements, or
    /// while it opens its picture of them – their states joining `states`.
    pub fn shown(
        &mut self,
        renderer: &mut VkRenderer,
        window: &Window,
        own: Vec<scene::Element>,
        anchor: Point<f64, Physical>,
        at: Duration,
        states: &mut RenderElementStates,
    ) -> Vec<scene::Element> {
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

    /// The pictures of closed windows a frame shown `at` draws in front of
    /// `window` – or, with None, of all windows left –, each once.
    pub fn closing_before(&mut self, window: Option<&Window>, at: Duration) -> Vec<scene::Element> {
        let mut elements = Vec::new();
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
        self.closing
            .iter_mut()
            .flat_map(|c| std::mem::take(&mut c.kept))
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
    }

    /// Without a screen nothing shows them: all end at once (ADR 0032 §3).
    pub fn finish(&mut self) {
        for o in self.opening.drain(..) {
            info!("animation: {} opened", o.app);
        }
        for c in self.closing.drain(..) {
            info!("animation: {} closed", c.app);
        }
    }

    /// Whether one is under way: it asks for the next frame.
    pub fn moving(&self) -> bool {
        !self.opening.is_empty() || !self.closing.is_empty()
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
        let mut animations = Animations {
            slower: 1.0,
            opening: Vec::new(),
            closing: Vec::new(),
        };
        closing(&mut animations);
        assert!(animations.moving());
        let end = ends(&mut animations);
        // 0.25 s, till half an 8-bit step is left: some 0.35 s.
        assert!((250..500).contains(&end), "{end} ms");
        let mut slower = Animations {
            slower: 10.0,
            opening: Vec::new(),
            closing: Vec::new(),
        };
        closing(&mut slower);
        let slow = ends(&mut slower);
        assert!(
            slow.abs_diff(end * 10) < 20,
            "{slow} ms, not ten times {end} ms"
        );
    }

    #[test]
    fn nothing_shown_ends_all_at_once() {
        let mut animations = Animations {
            slower: 1.0,
            opening: Vec::new(),
            closing: Vec::new(),
        };
        closing(&mut animations);
        animations.start(ms(1), ms(1));
        animations.finish();
        assert!(!animations.moving());
    }

    #[test]
    fn a_closing_is_placed_once_a_frame() {
        let mut animations = Animations {
            slower: 1.0,
            opening: Vec::new(),
            closing: Vec::new(),
        };
        closing(&mut animations);
        animations.start(ms(16), ms(16));
        animations.closing_before(None, ms(16));
        assert!(animations.closing.iter().all(|c| c.placed));
        animations.start(ms(32), ms(16));
        assert!(animations.closing.iter().all(|c| !c.placed));
    }
}
