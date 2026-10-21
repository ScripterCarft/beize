//! `beize-render-test`: checks Beize's renderer where it runs – in the VM
//! with lavapipe (ADR 0027 §6), on hardware with the GPU's driver. Draws
//! fixed scenes offscreen, reads them back and compares every pixel with
//! what it must show, computed on the CPU (the transforms with Smithay's
//! own `transform_point_in`, not the renderer's arithmetic). Not in the
//! image: it reaches the VM on bz's test medium.
//!
//! Prints one line per check and how long a desktop-like frame takes;
//! exits non-zero if a check fails.

use std::process::ExitCode;
use std::time::Instant;

use beize_compositor::error::{Context, Result, error};
use beize_compositor::log;
use beize_compositor::render::{Target, VkRenderer, VkTexture};
use smithay::backend::allocator::dmabuf::{AsDmabuf, DmabufMappingMode, DmabufSyncFlags};
use smithay::backend::allocator::dumb::DumbAllocator;
use smithay::backend::allocator::{Allocator, Fourcc, Modifier};
use smithay::backend::drm::DrmDeviceFd;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::{
    Bind, Color32F, ExportMem, Frame, ImportDma, ImportMem, Renderer, Texture,
};
use smithay::backend::vulkan::{Instance, PhysicalDevice, version::Version};
use smithay::reexports::ash::vk;
use smithay::utils::{Buffer, DeviceFd, Physical, Point, Rectangle, Size, Transform};

type Rgb = [u8; 3];
/// A check: its name and what it runs.
type Check = (&'static str, fn(&mut VkRenderer) -> Result<()>);

const W: i32 = 160;
const H: i32 = 120;

fn main() -> ExitCode {
    log::init();
    match run() {
        Ok(0) => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(e) => {
            println!("FAIL setting up: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u32> {
    let instance = Instance::new(Version::VERSION_1_3, None).context("Vulkan")?;
    let devices: Vec<PhysicalDevice> = PhysicalDevice::enumerate(&instance)
        .context("Vulkan devices")?
        .collect();
    // The GPU's own driver where there is one, else lavapipe.
    let phd = devices
        .iter()
        .find(|d| d.ty() != vk::PhysicalDeviceType::CPU)
        .or(devices.first())
        .context("no Vulkan device")?;
    let mut r = VkRenderer::new(phd)?;
    println!("device: {}", r.name());

    let checks: [Check; 12] = [
        ("clear", clear),
        ("texture 1:1 is exact", one_to_one),
        ("all eight buffer transforms", transforms),
        ("premultiplied alpha", premultiplied),
        ("opaque region drawn without blending", opaque_region),
        ("alpha of a whole element", element_alpha),
        ("damage leaves the rest", damage),
        ("scaled texture", scaled),
        ("partial upload", partial_upload),
        ("64 textures in one frame", many_textures),
        ("nothing changed, nothing drawn", idle),
        ("dmabuf import", dmabuf),
    ];
    let mut failed = 0;
    for (name, check) in checks {
        match check(&mut r) {
            Ok(()) => println!("ok   {name}"),
            Err(e) => {
                println!("FAIL {name}: {e}");
                failed += 1;
            }
        }
    }
    if let Err(e) = timing(&mut r) {
        println!("FAIL timing: {e}");
        failed += 1;
    }
    println!(
        "{} of {} checks passed",
        checks.len() as u32 + 1 - failed,
        checks.len() + 1
    );
    Ok(failed)
}

// --- Helpers ---------------------------------------------------------------

fn color(c: Rgb) -> Color32F {
    Color32F::new(
        f32::from(c[0]) / 255.0,
        f32::from(c[1]) / 255.0,
        f32::from(c[2]) / 255.0,
        1.0,
    )
}

fn full() -> Rectangle<i32, Physical> {
    Rectangle::from_size((W, H).into())
}

/// A pattern where every pixel of a small texture differs.
fn pattern(x: i32, y: i32) -> Rgb {
    [
        (x * 5 % 256) as u8,
        (y * 7 % 256) as u8,
        ((x * 3 + y * 11) % 256) as u8,
    ]
}

/// XRGB8888 bytes (B, G, R, X in memory) of `size`, from `f`.
fn xrgb(size: (i32, i32), f: impl Fn(i32, i32) -> Rgb) -> Vec<u8> {
    let mut data = Vec::with_capacity((size.0 * size.1 * 4) as usize);
    for y in 0..size.1 {
        for x in 0..size.0 {
            let [r, g, b] = f(x, y);
            data.extend_from_slice(&[b, g, r, 255]);
        }
    }
    data
}

fn texture(r: &mut VkRenderer, size: (i32, i32), f: impl Fn(i32, i32) -> Rgb) -> Result<VkTexture> {
    r.import_memory(&xrgb(size, f), Fourcc::Xrgb8888, size.into(), false)
}

fn target(r: &mut VkRenderer) -> Result<Target> {
    r.offscreen_target(Fourcc::Xrgb8888, (W, H).into())
}

/// Draws with `draw` into `target` and waits for the frame.
fn frame(
    r: &mut VkRenderer,
    target: &mut Target,
    draw: impl FnOnce(&mut beize_compositor::render::VkFrame<'_, '_>) -> Result<()>,
) -> Result<()> {
    let mut fb = r.bind(target)?;
    let mut frame = r.render(&mut fb, (W, H).into(), Transform::Normal)?;
    draw(&mut frame)?;
    frame
        .finish()?
        .wait()
        .map_err(|e| error(format!("waiting for the frame: {e}")))
}

/// The target's pixels, row by row.
fn read(r: &mut VkRenderer, target: &mut Target) -> Result<Vec<Rgb>> {
    let fb = r.bind(target)?;
    let mapping = r.copy_framebuffer(&fb, Rectangle::from_size((W, H).into()), Fourcc::Xrgb8888)?;
    let bytes = r.map_texture(&mapping)?;
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| [p[2], p[1], p[0]])
        .collect())
}

/// Compares every pixel with `expected` (None: not checked), each channel
/// within `tolerance`; names the first mismatches.
fn compare(got: &[Rgb], expected: impl Fn(i32, i32) -> Option<Rgb>, tolerance: u8) -> Result<()> {
    let mut wrong = 0;
    let mut first = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let Some(want) = expected(x, y) else { continue };
            let have = got[(y * W + x) as usize];
            if have
                .iter()
                .zip(want)
                .any(|(&h, w)| h.abs_diff(w) > tolerance)
            {
                wrong += 1;
                if first.len() < 3 {
                    first.push(format!("({x},{y}) {have:?} instead of {want:?}"));
                }
            }
        }
    }
    if wrong == 0 {
        Ok(())
    } else {
        Err(error(format!("{wrong} pixels wrong: {}", first.join(", "))))
    }
}

fn inside(rect: Rectangle<i32, Physical>, x: i32, y: i32) -> bool {
    rect.contains(Point::from((x, y)))
}

const BACK: Rgb = [20, 40, 200];

// --- Checks ----------------------------------------------------------------

fn clear(r: &mut VkRenderer) -> Result<()> {
    let mut t = target(r)?;
    frame(r, &mut t, |f| f.clear(color([12, 34, 56]), &[full()]))?;
    compare(&read(r, &mut t)?, |_, _| Some([12, 34, 56]), 0)
}

fn one_to_one(r: &mut VkRenderer) -> Result<()> {
    let tex = texture(r, (48, 32), pattern)?;
    let dst = Rectangle::new((13, 21).into(), (48, 32).into());
    let mut t = target(r)?;
    frame(r, &mut t, |f| {
        f.clear(color(BACK), &[full()])?;
        f.render_texture_at(
            &tex,
            dst.loc,
            1,
            1.0,
            Transform::Normal,
            &[Rectangle::from_size(dst.size)],
            &[],
            1.0,
        )
    })?;
    compare(
        &read(r, &mut t)?,
        |x, y| {
            Some(if inside(dst, x, y) {
                pattern(x - 13, y - 21)
            } else {
                BACK
            })
        },
        0,
    )
}

fn transforms(r: &mut VkRenderer) -> Result<()> {
    let size: Size<i32, Buffer> = (48, 32).into();
    let tex = texture(r, (size.w, size.h), pattern)?;
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
        let shown = transform.transform_size(size);
        let dst = Rectangle::new((7, 9).into(), (shown.w, shown.h).into());
        let mut t = target(r)?;
        frame(r, &mut t, |f| {
            f.clear(color(BACK), &[full()])?;
            f.render_texture_from_to(
                &tex,
                Rectangle::from_size(size.to_f64()),
                dst,
                &[Rectangle::from_size(dst.size)],
                &[],
                transform,
                1.0,
            )
        })?;
        let shown_f = shown.to_f64();
        compare(
            &read(r, &mut t)?,
            |x, y| {
                if !inside(dst, x, y) {
                    return Some(BACK);
                }
                let center: Point<f64, Buffer> =
                    (f64::from(x - 7) + 0.5, f64::from(y - 9) + 0.5).into();
                let b = transform.invert().transform_point_in(center, &shown_f);
                Some(pattern(b.x.floor() as i32, b.y.floor() as i32))
            },
            0,
        )
        .map_err(|e| error(format!("{transform:?}: {e}")))?;
    }
    Ok(())
}

/// ARGB8888, premultiplied: half-covering red.
fn half_red(r: &mut VkRenderer) -> Result<VkTexture> {
    let data: Vec<u8> = (0..16 * 16).flat_map(|_| [0u8, 0, 128, 128]).collect();
    r.import_memory(&data, Fourcc::Argb8888, (16, 16).into(), false)
}

fn premultiplied(r: &mut VkRenderer) -> Result<()> {
    let tex = half_red(r)?;
    let dst = Rectangle::new((30, 30).into(), (16, 16).into());
    let mut t = target(r)?;
    frame(r, &mut t, |f| {
        f.clear(color([0, 0, 255]), &[full()])?;
        f.render_texture_at(
            &tex,
            dst.loc,
            1,
            1.0,
            Transform::Normal,
            &[Rectangle::from_size(dst.size)],
            &[],
            1.0,
        )
    })?;
    // src + dst * (1 - 128/255)
    compare(
        &read(r, &mut t)?,
        |x, y| {
            Some(if inside(dst, x, y) {
                [128, 0, 127]
            } else {
                [0, 0, 255]
            })
        },
        1,
    )
}

fn opaque_region(r: &mut VkRenderer) -> Result<()> {
    let tex = half_red(r)?;
    let dst = Rectangle::new((30, 30).into(), (16, 16).into());
    // The left half claimed opaque: drawn as it is, without blending.
    let opaque = Rectangle::new((0, 0).into(), (8, 16).into());
    let mut t = target(r)?;
    frame(r, &mut t, |f| {
        f.clear(color([0, 0, 255]), &[full()])?;
        f.render_texture_at(
            &tex,
            dst.loc,
            1,
            1.0,
            Transform::Normal,
            &[Rectangle::from_size(dst.size)],
            &[opaque],
            1.0,
        )
    })?;
    compare(
        &read(r, &mut t)?,
        |x, y| {
            Some(if !inside(dst, x, y) {
                [0, 0, 255]
            } else if x < 38 {
                [128, 0, 0]
            } else {
                [128, 0, 127]
            })
        },
        1,
    )
}

fn element_alpha(r: &mut VkRenderer) -> Result<()> {
    let tex = texture(r, (20, 20), |_, _| [200, 100, 0])?;
    let dst = Rectangle::new((50, 10).into(), (20, 20).into());
    let mut t = target(r)?;
    frame(r, &mut t, |f| {
        f.clear(color([0, 0, 255]), &[full()])?;
        f.render_texture_at(
            &tex,
            dst.loc,
            1,
            1.0,
            Transform::Normal,
            &[Rectangle::from_size(dst.size)],
            &[],
            0.5,
        )
    })?;
    compare(
        &read(r, &mut t)?,
        |x, y| {
            Some(if inside(dst, x, y) {
                [100, 50, 128]
            } else {
                [0, 0, 255]
            })
        },
        1,
    )
}

fn damage(r: &mut VkRenderer) -> Result<()> {
    let tex = texture(r, (W, H), pattern)?;
    let mut t = target(r)?;
    frame(r, &mut t, |f| f.clear(color(BACK), &[full()]))?;
    // Only this part is damaged: the texture shows there, the rest stays.
    let part = Rectangle::new((40, 30).into(), (25, 17).into());
    frame(r, &mut t, |f| {
        f.render_texture_at(
            &tex,
            (0, 0).into(),
            1,
            1.0,
            Transform::Normal,
            &[part],
            &[],
            1.0,
        )
    })?;
    compare(
        &read(r, &mut t)?,
        |x, y| {
            Some(if inside(part, x, y) {
                pattern(x, y)
            } else {
                BACK
            })
        },
        0,
    )
}

fn scaled(r: &mut VkRenderer) -> Result<()> {
    // A uniform texture stays uniform however it is filtered.
    let tex = texture(r, (8, 8), |_, _| [90, 180, 30])?;
    let dst = Rectangle::new((10, 10).into(), (37, 29).into());
    let mut t = target(r)?;
    frame(r, &mut t, |f| {
        f.clear(color(BACK), &[full()])?;
        f.render_texture_from_to(
            &tex,
            Rectangle::from_size((8.0, 8.0).into()),
            dst,
            &[Rectangle::from_size(dst.size)],
            &[],
            Transform::Normal,
            1.0,
        )
    })?;
    compare(
        &read(r, &mut t)?,
        |x, y| {
            Some(if inside(dst, x, y) {
                [90, 180, 30]
            } else {
                BACK
            })
        },
        0,
    )?;
    // Black | white, stretched: values rise from left to right.
    let tex = texture(
        r,
        (2, 1),
        |x, _| if x == 0 { [0, 0, 0] } else { [255, 255, 255] },
    )?;
    let dst = Rectangle::new((0, 60).into(), (W, 4).into());
    frame(r, &mut t, |f| {
        f.render_texture_from_to(
            &tex,
            Rectangle::from_size((2.0, 1.0).into()),
            dst,
            &[Rectangle::from_size(dst.size)],
            &[],
            Transform::Normal,
            1.0,
        )
    })?;
    let got = read(r, &mut t)?;
    let row: Vec<u8> = (0..W).map(|x| got[(61 * W + x) as usize][0]).collect();
    let rising = row.windows(2).all(|w| w[1] >= w[0]);
    let (low, high) = (row[0], row[W as usize - 1]);
    if rising && low < 10 && high > 245 && row[W as usize / 2].abs_diff(128) < 20 {
        Ok(())
    } else {
        Err(error(format!(
            "not a gradient: {low} … {} … {high}",
            row[W as usize / 2]
        )))
    }
}

fn partial_upload(r: &mut VkRenderer) -> Result<()> {
    let tex = texture(r, (32, 32), pattern)?;
    let region = Rectangle::new((8, 4).into(), (10, 6).into());
    r.update_memory(&tex, &xrgb((32, 32), |_, _| [255, 255, 255]), region)?;
    let mut t = target(r)?;
    frame(r, &mut t, |f| {
        f.clear(color(BACK), &[full()])?;
        f.render_texture_at(
            &tex,
            (0, 0).into(),
            1,
            1.0,
            Transform::Normal,
            &[Rectangle::from_size((32, 32).into())],
            &[],
            1.0,
        )
    })?;
    compare(
        &read(r, &mut t)?,
        |x, y| {
            Some(if x >= 32 || y >= 32 {
                BACK
            } else if region.contains(Point::<i32, Buffer>::from((x, y))) {
                [255, 255, 255]
            } else {
                pattern(x, y)
            })
        },
        0,
    )
}

fn many_textures(r: &mut VkRenderer) -> Result<()> {
    let tile = |i: i32| [(i * 4) as u8, (255 - i * 3) as u8, (i * 7 % 256) as u8];
    let textures: Vec<VkTexture> = (0..64)
        .map(|i| texture(r, (10, 10), move |_, _| tile(i)))
        .collect::<Result<_>>()?;
    let mut t = target(r)?;
    frame(r, &mut t, |f| {
        f.clear(color(BACK), &[full()])?;
        for (i, tex) in textures.iter().enumerate() {
            let i = i as i32;
            let at = (i % 16 * 10, i / 16 * 10);
            f.render_texture_at(
                tex,
                at.into(),
                1,
                1.0,
                Transform::Normal,
                &[Rectangle::from_size((10, 10).into())],
                &[],
                1.0,
            )?;
        }
        Ok(())
    })?;
    compare(
        &read(r, &mut t)?,
        |x, y| {
            Some(if y < 40 {
                tile(y / 10 * 16 + x / 10)
            } else {
                BACK
            })
        },
        0,
    )
}

/// Smithay's damage tracker over the renderer: a second frame of the same
/// scene on an image of age 1 draws nothing.
fn idle(r: &mut VkRenderer) -> Result<()> {
    let mut buffer =
        MemoryRenderBuffer::new(Fourcc::Xrgb8888, (32, 32), 1, Transform::Normal, None);
    {
        let mut ctx = buffer.render();
        ctx.draw(|data| {
            data.copy_from_slice(&xrgb((32, 32), pattern));
            Ok::<_, std::convert::Infallible>(vec![Rectangle::from_size((32, 32).into())])
        })
        .map_err(|e| error(format!("{e:?}")))?;
    }
    let element = MemoryRenderBufferRenderElement::from_buffer(
        r,
        (5.0, 5.0),
        &buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    )?;
    let mut damage = OutputDamageTracker::new((W, H), 1.0, Transform::Normal);
    let mut t = target(r)?;
    for age in [0, 1] {
        let mut fb = r.bind(&mut t)?;
        let result = damage
            .render_output(r, &mut fb, age, &[&element], color(BACK))
            .map_err(|e| error(format!("{e:?}")))?;
        let drew = result.damage.is_some();
        result
            .sync
            .wait()
            .map_err(|e| error(format!("waiting: {e}")))?;
        match (age, drew) {
            (0, false) => return Err(error("the first frame drew nothing")),
            (1, true) => return Err(error("an unchanged frame drew again")),
            _ => {}
        }
    }
    compare(
        &read(r, &mut t)?,
        |x, y| {
            let rect = Rectangle::<i32, Physical>::new((5, 5).into(), (32, 32).into());
            Some(if inside(rect, x, y) {
                pattern(x - 5, y - 5)
            } else {
                BACK
            })
        },
        0,
    )
}

/// A linear dumb buffer of the machine's KMS driver, as a client's dmabuf
/// would arrive, drawn twice (its ownership goes back between frames).
fn dmabuf(r: &mut VkRenderer) -> Result<()> {
    let card = (0..8)
        .map(|i| format!("/dev/dri/card{i}"))
        .find_map(|p| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&p)
                .ok()
        })
        .context("no DRM card to make a dmabuf with")?;
    let fd = DrmDeviceFd::new(DeviceFd::from(std::os::fd::OwnedFd::from(card)));
    let mut dumb = DumbAllocator::new(fd);
    let buffer = dumb
        .create_buffer(64, 32, Fourcc::Xrgb8888, &[Modifier::Linear])
        .context("dumb buffer")?;
    let dmabuf = buffer.export().context("exporting the dumb buffer")?;
    {
        let mapping = dmabuf
            .map_plane(0, DmabufMappingMode::WRITE)
            .context("mapping")?;
        let stride = dmabuf.strides().next().context("plane")? as usize;
        dmabuf
            .sync_plane(0, DmabufSyncFlags::START | DmabufSyncFlags::WRITE)
            .context("sync")?;
        let data = xrgb((64, 32), pattern);
        for y in 0..32 {
            // SAFETY: the row lies within the mapping (stride × 32 bytes).
            unsafe {
                std::ptr::copy_nonoverlapping(
                    data.as_ptr().add(y * 256),
                    (mapping.ptr() as *mut u8).add(y * stride),
                    256,
                )
            };
        }
        dmabuf
            .sync_plane(0, DmabufSyncFlags::END | DmabufSyncFlags::WRITE)
            .context("sync")?;
    }
    let tex = r.import_dmabuf(&dmabuf, None)?;
    if tex.width() != 64 {
        return Err(error("wrong size"));
    }
    let dst = Rectangle::new((3, 4).into(), (64, 32).into());
    let mut t = target(r)?;
    for _ in 0..2 {
        frame(r, &mut t, |f| {
            f.clear(color(BACK), &[full()])?;
            f.render_texture_at(
                &tex,
                dst.loc,
                1,
                1.0,
                Transform::Normal,
                &[Rectangle::from_size(dst.size)],
                &[],
                1.0,
            )
        })?;
    }
    compare(
        &read(r, &mut t)?,
        |x, y| {
            Some(if inside(dst, x, y) {
                pattern(x - 3, y - 4)
            } else {
                BACK
            })
        },
        0,
    )
}

/// A desktop-like frame at 1280x800: a full-screen wallpaper, four
/// translucent windows; all of it, then a small part damaged.
fn timing(r: &mut VkRenderer) -> Result<()> {
    let (w, h) = (1280, 800);
    let wallpaper = texture(r, (w, h), pattern)?;
    let window = half_red(r)?;
    let mut t = r.offscreen_target(Fourcc::Xrgb8888, (w, h).into())?;
    let full: Rectangle<i32, Physical> = Rectangle::from_size((w, h).into());
    let mut measure = |r: &mut VkRenderer, damage: Rectangle<i32, Physical>| -> Result<f64> {
        let start = Instant::now();
        let rounds = 20;
        for _ in 0..rounds {
            let mut fb = r.bind(&mut t)?;
            let mut f = r.render(&mut fb, (w, h).into(), Transform::Normal)?;
            f.render_texture_from_to(
                &wallpaper,
                Rectangle::from_size((w as f64, h as f64).into()),
                full,
                &[damage],
                &[],
                Transform::Normal,
                1.0,
            )?;
            for i in 0..4 {
                let dst = Rectangle::new((100 + i * 250, 100 + i * 100).into(), (300, 200).into());
                let rel = damage
                    .intersection(dst)
                    .map(|d| Rectangle::new(d.loc - dst.loc, d.size));
                f.render_texture_from_to(
                    &window,
                    Rectangle::from_size((16.0, 16.0).into()),
                    dst,
                    rel.as_slice(),
                    &[],
                    Transform::Normal,
                    1.0,
                )?;
            }
            f.finish()?
                .wait()
                .map_err(|e| error(format!("waiting: {e}")))?;
        }
        Ok(start.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds))
    };
    let all = measure(r, full)?;
    let gpu_all = r.gpu_ms();
    let small = measure(r, Rectangle::new((600, 380).into(), (80, 40).into()))?;
    let gpu_small = r.gpu_ms();
    println!(
        "time   1280x800, wallpaper + 4 windows: whole frame {all:.2} ms (GPU {}), 80x40 damaged {small:.2} ms (GPU {})",
        gpu_all.map_or("–".into(), |g| format!("{g:.2} ms")),
        gpu_small.map_or("–".into(), |g| format!("{g:.2} ms")),
    );
    Ok(())
}
