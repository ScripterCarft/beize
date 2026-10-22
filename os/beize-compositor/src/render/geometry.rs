//! Where a quad lands in the framebuffer and which texels it shows: pure
//! arithmetic, tested without a GPU.
//!
//! Smithay hands the renderer rectangles in the output's space (the
//! output's transform not yet applied) and, for textures, a source
//! rectangle in buffer coordinates with the buffer's transform. Every
//! rectangle a frame draws is cut to its damage here and mapped into the
//! framebuffer, so the GPU only ever sees axis-aligned quads with a linear
//! texture mapping – one instance each, without scissors.

use smithay::utils::{Buffer, Physical, Point, Rectangle, Size, Transform};

pub use beize_vulkan::pipeline::{EXACT, Quad, SOLID};

/// The framebuffer as the frame sees it: its size and the output's
/// transform.
#[derive(Clone, Copy, Debug)]
pub struct Space {
    /// The framebuffer's own size.
    pub framebuffer: Size<i32, Physical>,
    pub transform: Transform,
}

impl Space {
    /// The size of the output's space, where Smithay's rectangles live.
    fn output(&self) -> Size<i32, Physical> {
        self.transform.transform_size(self.framebuffer)
    }

    /// A rectangle of the output's space in the framebuffer.
    pub fn framebuffer_rect(&self, rect: Rectangle<i32, Physical>) -> Rectangle<i32, Physical> {
        self.transform
            .invert()
            .transform_rect_in(rect, &self.output())
    }

    /// A framebuffer point back in the output's space.
    fn output_point(&self, point: Point<f64, Physical>) -> Point<f64, Physical> {
        self.transform
            .transform_point_in(point, &self.framebuffer.to_f64())
    }
}

/// How a texture's source rectangle covers its destination.
#[derive(Clone, Copy, Debug)]
pub struct Mapping {
    pub src: Rectangle<f64, Buffer>,
    /// In the output's space.
    pub dst: Rectangle<i32, Physical>,
    /// The buffer's transform.
    pub transform: Transform,
}

impl Mapping {
    /// The buffer point under `point` of the output's space – as Smithay's
    /// GLES renderer maps it (its `build_texture_mat`).
    fn buffer_point(&self, point: Point<f64, Physical>) -> Point<f64, Buffer> {
        let shown = self.transform.transform_size(self.src.size);
        let dst = self.dst.to_f64();
        let scaled: Point<f64, Buffer> = (
            (point.x - dst.loc.x) * shown.w / dst.size.w,
            (point.y - dst.loc.y) * shown.h / dst.size.h,
        )
            .into();
        self.transform.invert().transform_point_in(scaled, &shown) + self.src.loc
    }
}

/// The quad for `part` (in the output's space, inside the destination) of
/// a texture of `size`, or of a solid colour without a mapping.
pub fn quad(
    space: &Space,
    part: Rectangle<i32, Physical>,
    mapping: Option<(&Mapping, Size<i32, Buffer>)>,
) -> Quad {
    let rect = space.framebuffer_rect(part);
    let mut quad = Quad {
        pos: [rect.loc.x as f32, rect.loc.y as f32],
        size: [rect.size.w as f32, rect.size.h as f32],
        ..Quad::default()
    };
    let Some((mapping, size)) = mapping else {
        quad.flags = SOLID;
        return quad;
    };
    let fb = rect.to_f64();
    let corner = |dx: f64, dy: f64| {
        let point = (fb.loc.x + dx * fb.size.w, fb.loc.y + dy * fb.size.h).into();
        mapping.buffer_point(space.output_point(point))
    };
    let (origin, right, down) = (corner(0.0, 0.0), corner(1.0, 0.0), corner(0.0, 1.0));
    let uv_x = right - origin;
    let uv_y = down - origin;
    quad.uv = [origin.x as f32, origin.y as f32];
    quad.uv_x = [uv_x.x as f32, uv_x.y as f32];
    quad.uv_y = [uv_y.x as f32, uv_y.y as f32];
    quad.texel = [1.0 / size.w as f32, 1.0 / size.h as f32];
    // One texel per pixel along each edge, starting on a texel border.
    let unit = |d: Point<f64, Buffer>, len: f64| {
        (d.x.abs() == len && d.y == 0.0) || (d.y.abs() == len && d.x == 0.0)
    };
    if unit(uv_x, fb.size.w)
        && unit(uv_y, fb.size.h)
        && origin.x.fract() == 0.0
        && origin.y.fract() == 0.0
    {
        quad.flags = EXACT;
    }
    quad
}

/// `rects`, relative to `dst`, clipped to it and made absolute: Smithay's
/// damage and opaque regions are relative to the element.
pub fn absolute(
    dst: Rectangle<i32, Physical>,
    rects: &[Rectangle<i32, Physical>],
) -> impl Iterator<Item = Rectangle<i32, Physical>> + '_ {
    rects
        .iter()
        .filter_map(move |r| Rectangle::new(dst.loc + r.loc, r.size).intersection(dst))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal(size: (i32, i32)) -> Space {
        Space {
            framebuffer: size.into(),
            transform: Transform::Normal,
        }
    }

    /// The texel coordinate at a corner of the quad (0 or 1 along each).
    fn at(q: &Quad, dx: f32, dy: f32) -> [f32; 2] {
        [
            q.uv[0] + dx * q.uv_x[0] + dy * q.uv_y[0],
            q.uv[1] + dx * q.uv_x[1] + dy * q.uv_y[1],
        ]
    }

    #[test]
    fn a_texture_shown_one_to_one_is_exact() {
        let mapping = Mapping {
            src: Rectangle::from_size((100.0, 50.0).into()),
            dst: Rectangle::new((10, 20).into(), (100, 50).into()),
            transform: Transform::Normal,
        };
        let q = quad(
            &normal((800, 600)),
            mapping.dst,
            Some((&mapping, (100, 50).into())),
        );
        assert_eq!(q.pos, [10.0, 20.0]);
        assert_eq!(q.size, [100.0, 50.0]);
        assert_eq!(q.uv, [0.0, 0.0]);
        assert_eq!(at(&q, 1.0, 1.0), [100.0, 50.0]);
        assert_eq!(q.flags, EXACT);
    }

    #[test]
    fn damage_cuts_the_texture_with_it() {
        let mapping = Mapping {
            src: Rectangle::from_size((100.0, 50.0).into()),
            dst: Rectangle::new((10, 20).into(), (100, 50).into()),
            transform: Transform::Normal,
        };
        let part = Rectangle::new((40, 30).into(), (20, 10).into());
        let q = quad(
            &normal((800, 600)),
            part,
            Some((&mapping, (100, 50).into())),
        );
        assert_eq!(q.pos, [40.0, 30.0]);
        assert_eq!(q.uv, [30.0, 10.0]);
        assert_eq!(at(&q, 1.0, 1.0), [50.0, 20.0]);
        assert_eq!(q.flags, EXACT);
    }

    #[test]
    fn a_scaled_texture_is_filtered() {
        let mapping = Mapping {
            src: Rectangle::from_size((200.0, 100.0).into()),
            dst: Rectangle::new((0, 0).into(), (100, 50).into()),
            transform: Transform::Normal,
        };
        let q = quad(
            &normal((800, 600)),
            mapping.dst,
            Some((&mapping, (200, 100).into())),
        );
        assert_eq!(at(&q, 1.0, 1.0), [200.0, 100.0]);
        assert_eq!(q.flags, 0);
    }

    #[test]
    fn half_a_texel_off_is_filtered() {
        let mapping = Mapping {
            src: Rectangle::new((0.5, 0.0).into(), (100.0, 50.0).into()),
            dst: Rectangle::new((0, 0).into(), (100, 50).into()),
            transform: Transform::Normal,
        };
        let q = quad(
            &normal((800, 600)),
            mapping.dst,
            Some((&mapping, (101, 50).into())),
        );
        assert_eq!(q.flags, 0);
    }

    /// Smithay's GLES test `texture_90`: the same corners.
    #[test]
    fn a_buffer_turned_by_90_degrees_as_smithay_maps_it() {
        let mapping = Mapping {
            src: Rectangle::from_size((250.0, 500.0).into()),
            dst: Rectangle::new((442, 144).into(), (500, 250).into()),
            transform: Transform::_90,
        };
        let q = quad(
            &normal((1920, 1080)),
            mapping.dst,
            Some((&mapping, (250, 500).into())),
        );
        assert_eq!(at(&q, 0.0, 0.0), [0.0, 500.0]);
        assert_eq!(at(&q, 1.0, 0.0), [0.0, 0.0]);
        assert_eq!(at(&q, 1.0, 1.0), [250.0, 0.0]);
        assert_eq!(at(&q, 0.0, 1.0), [250.0, 500.0]);
        assert_eq!(q.flags, EXACT);
    }

    /// Every transform keeps the four corners of the source on the four
    /// corners of the destination, one texel per pixel.
    #[test]
    fn every_buffer_transform_covers_the_source_exactly() {
        for transform in [
            Transform::Normal,
            Transform::_90,
            Transform::_180,
            Transform::_270,
            Transform::Flipped,
            Transform::Flipped90,
            Transform::Flipped180,
            Transform::Flipped270,
        ] {
            let src: Size<i32, Buffer> = (64, 32).into();
            let shown = transform.transform_size(src);
            let mapping = Mapping {
                src: Rectangle::from_size(src.to_f64()),
                dst: Rectangle::new((5, 7).into(), (shown.w, shown.h).into()),
                transform,
            };
            let q = quad(&normal((800, 600)), mapping.dst, Some((&mapping, src)));
            let mut corners: Vec<[f32; 2]> = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
                .iter()
                .map(|&(x, y)| at(&q, x, y))
                .collect();
            corners.sort_by(|a, b| a.partial_cmp(b).unwrap());
            assert_eq!(
                corners,
                [[0.0, 0.0], [0.0, 32.0], [64.0, 0.0], [64.0, 32.0]],
                "{transform:?}"
            );
            assert_eq!(q.flags, EXACT, "{transform:?}");
        }
    }

    #[test]
    fn a_turned_output_moves_the_quad_and_keeps_the_texels() {
        let space = Space {
            framebuffer: (800, 600).into(),
            transform: Transform::_90,
        };
        let mapping = Mapping {
            src: Rectangle::from_size((100.0, 50.0).into()),
            dst: Rectangle::new((0, 0).into(), (100, 50).into()),
            transform: Transform::Normal,
        };
        let q = quad(&space, mapping.dst, Some((&mapping, (100, 50).into())));
        assert_eq!(q.size, [50.0, 100.0]);
        assert_eq!(q.flags, EXACT);
        // The output's top-left texel lands in a corner of the framebuffer
        // quad, and the quad stays inside the framebuffer.
        assert!(q.pos[0] >= 0.0 && q.pos[0] + q.size[0] <= 800.0);
        assert!(q.pos[1] >= 0.0 && q.pos[1] + q.size[1] <= 600.0);
    }

    #[test]
    fn a_solid_needs_no_texels() {
        let part = Rectangle::new((1, 2).into(), (3, 4).into());
        let q = quad(&normal((800, 600)), part, None);
        assert_eq!(q.flags, SOLID);
        assert_eq!(q.pos, [1.0, 2.0]);
    }

    #[test]
    fn damage_is_relative_to_the_element_and_clipped() {
        let dst = Rectangle::new((100, 100).into(), (50, 50).into());
        let damage = [
            Rectangle::new((10, 10).into(), (10, 10).into()),
            Rectangle::new((40, 40).into(), (30, 30).into()),
            Rectangle::new((60, 0).into(), (5, 5).into()),
        ];
        let got: Vec<_> = absolute(dst, &damage).collect();
        assert_eq!(
            got,
            [
                Rectangle::new((110, 110).into(), (10, 10).into()),
                Rectangle::new((140, 140).into(), (10, 10).into()),
            ]
        );
    }
}
