//! Snapping, maximizing and back: the frame springs from the old
//! rectangle to the new, the picture from before fades into the new.

use super::*;

impl Reshape {
    /// The frame as shown at `at`.
    pub(super) fn shown(&mut self, at: Duration) -> Option<Rectangle<i32, Physical>> {
        let edges = self.edges.as_mut()?;
        let [left, top, right, bottom] = [0, 1, 2, 3].map(|i| edges[i].at(at).round() as i32);
        Some(Rectangle::new(
            (left, top).into(),
            ((right - left).max(1), (bottom - top).max(1)).into(),
        ))
    }

    /// Drawn as a reshape: from the commit in its new place until frame
    /// and crossfade rest and no new size is awaited.
    pub(super) fn shows(&self) -> bool {
        self.edges.is_some()
    }

    /// Asks for frames: while an edge or the crossfade moves. Waiting at
    /// rest for an app's answer to a new aim asks for none – a slow or
    /// hung app costs no frames.
    pub(super) fn moving(&self) -> bool {
        self.aim
            || self.fade_aim
            || self.fade.moving()
            || self
                .edges
                .as_ref()
                .is_some_and(|edges| edges.iter().any(Motion::moving))
    }

    pub(super) fn done(&self) -> bool {
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
pub(super) fn edges_of(frame: Rectangle<i32, Physical>) -> [f64; 4] {
    [
        f64::from(frame.loc.x),
        f64::from(frame.loc.y),
        f64::from(frame.loc.x + frame.size.w),
        f64::from(frame.loc.y + frame.size.h),
    ]
}

/// `place`, which lies about the frame `from`, stretched as `from` is to
/// `to`: a picture with its shadow follows the frame.
pub(super) fn stretched(
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

impl Animations {
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
        let released = &mut self.released;
        self.reshapes.retain_mut(|r| {
            let stops = r.window == *window;
            if stops {
                if r.shows() {
                    info!("animation: {} stopped moving", r.app);
                }
                // Its picture may be in the frame on its way: its buffers
                // go back with that frame (`drawn`), not before.
                released.append(&mut r.kept);
            }
            !stops
        });
    }
}
