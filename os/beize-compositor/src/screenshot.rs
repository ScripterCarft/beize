//! Screenshots: Super+Shift+3, as macOS' ⌘⇧3, and the Print key, as on
//! Windows and GNOME – the whole screen, lossless, as a PNG in
//! `$XDG_RUNTIME_DIR/screenshots`, named by the local time. The
//! compositor reads the home but writes only below the runtime directory
//! (its sandbox, `beize-compositor.service`), so the pictures live until
//! the session ends; saving them into the home comes with a screenshot
//! app of its own, as macOS' Screenshot beside the WindowServer.

use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

use crate::error::{Context, Result};

/// Saves `rgb` (rows of `size` from the top, 8-bit RGB, sRGB) on a thread
/// of its own: encoding a 2560x1440 screen takes longer than a frame.
pub fn save(size: (u32, u32), rgb: Vec<u8>) {
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").map(|d| Path::new(&d).join("screenshots"))
    else {
        warn!("screenshot: no XDG_RUNTIME_DIR");
        return;
    };
    let name = name();
    let spawned = std::thread::Builder::new()
        .name("screenshot".into())
        .spawn(move || match write(&dir, &name, size, &rgb) {
            Ok(file) => info!("screenshot {}x{}: {}", size.0, size.1, file.display()),
            Err(e) => warn!("screenshot: {e}"),
        });
    if let Err(e) = spawned {
        warn!("screenshot: no thread: {e}");
    }
}

fn write(dir: &Path, name: &str, (w, h): (u32, u32), rgb: &[u8]) -> Result<PathBuf> {
    fs::create_dir_all(dir).context(format!("creating {}", dir.display()))?;
    // Two in one second: the second gets a number, as macOS numbers them.
    let file = (1..)
        .map(|n| match n {
            1 => dir.join(format!("{name}.png")),
            n => dir.join(format!("{name} {n}.png")),
        })
        .find(|f| !f.exists())
        .context("no free name")?;
    let out = BufWriter::new(fs::File::create_new(&file).context("creating the file")?);
    let mut encoder = png::Encoder::new(out, w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    encoder.set_compression(png::Compression::Fast);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(rgb))
        .context("encoding the PNG")?;
    Ok(file)
}

unsafe extern "C" {
    /// glibc: reads the zone again if /etc/localtime changed (as the
    /// shell's clock does).
    fn tzset();
}

/// "Screenshot 2026-10-25 at 09.30.12", as macOS names them; the seconds
/// since 1970 if the C library cannot say the local time.
fn name() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let t = libc::time_t::try_from(seconds).unwrap_or(0);
    // SAFETY: tm is plain data that localtime_r fills; tzset only reads the
    // environment and /etc/localtime.
    let local = unsafe {
        tzset();
        let mut tm: libc::tm = std::mem::zeroed();
        (!libc::localtime_r(&t, &mut tm).is_null()).then_some(tm)
    };
    match local {
        Some(tm) => format!(
            "Screenshot {:04}-{:02}-{:02} at {:02}.{:02}.{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        ),
        None => format!("Screenshot {seconds}"),
    }
}
