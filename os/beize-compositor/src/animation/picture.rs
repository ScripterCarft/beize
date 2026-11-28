//! A window's picture (`Picture`): drawn offscreen, shown scaled about
//! its frame's centre and faded; the render elements that place it
//! (`PictureElement`, `MeshElement` for the genie) and that pose a live
//! surface (`Posed`).

use super::*;

impl Picture {
    pub fn new() -> Picture {
        Picture {
            target: None,
            damage: None,
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
            self.damage = None;
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
        // The target keeps what was drawn into it last: age 1 once drawn.
        let age = usize::from(self.damage.is_some());
        let damage = self.damage.get_or_insert_with(|| {
            OutputDamageTracker::new((place.size.w, place.size.h), 1.0, Transform::Normal)
        });
        let mut framebuffer = renderer.bind(target)?;
        let drawn = damage
            .render_output(renderer, &mut framebuffer, age, &elements, [0.0; 4])
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

impl<E: Element> Posed<E> {
    pub fn new(element: E, pose: &Pose) -> Posed<E> {
        Posed {
            element,
            origin: pose.origin().to_f64(),
            scale: pose.scale,
        }
    }
}

impl<E: Element> Element for Posed<E> {
    fn id(&self) -> &Id {
        self.element.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.element.current_commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.element.src()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        scaled(self.element.geometry(scale), self.origin, self.scale)
    }

    fn transform(&self) -> Transform {
        self.element.transform()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.element
            .damage_since(scale, commit)
            .into_iter()
            .map(|rect| rect.to_f64().upscale(self.scale).to_i32_up())
            .collect()
    }

    /// None while it moves: it covers nothing for sure at its rounded edges.
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.element.alpha()
    }

    fn kind(&self) -> Kind {
        self.element.kind()
    }
}

impl<R: smithay::backend::renderer::Renderer, E: RenderElement<R>> RenderElement<R> for Posed<E> {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> std::result::Result<(), R::Error> {
        self.element.draw(frame, src, dst, damage, opaque_regions)
    }
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

/// `place` scaled by `scale` about `anchor`, its edges on whole pixels.
pub(super) fn scaled(
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
