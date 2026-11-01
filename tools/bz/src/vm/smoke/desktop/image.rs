//! Screens: QEMU screendumps as images, and waits for what they show.

use super::*;

/// What the screen shows now, kept in the logs as `name`.
pub(super) fn screen(machine: &Machine, name: &str) -> Result<Image> {
    let file = screen_file(machine, name);
    screendump(&machine.qmp(), &file)?;
    Image::read_ppm(&fs::read(&file)?)
}

/// The mean of the channels in the middle of the screen, 100 pixels a
/// side: the background, where no window opened.
pub(super) fn centre_mean(screen: &Image) -> f64 {
    let (x, y) = (screen.width / 2, screen.height / 2);
    let rows = y.saturating_sub(50)..(y + 50).min(screen.height);
    let n = rows.len().max(1) as f64;
    rows.map(|row| {
        row_mean(
            screen,
            row,
            x.saturating_sub(50)..(x + 50).min(screen.width),
        )
    })
    .sum::<f64>()
        / n
}

/// The screen's size, from a screendump.
pub(super) fn screen_size(machine: &Machine) -> Result<(usize, usize)> {
    let file = machine.logs.join("desktop-screen.ppm");
    screendump(&machine.qmp(), &file)?;
    let screen = Image::read_ppm(&fs::read(&file)?)?;
    Ok((screen.width, screen.height))
}

/// The content of a window of Beize UI: below its title bar.
pub(super) fn content(((x, y), (w, h)): Window) -> Window {
    ((x, y + TITLE_BAR), (w, h.saturating_sub(TITLE_BAR)))
}

/// Waits until the test client's window `at` shows exactly its pattern in
/// `size`; if it does not, the screen and the difference are in the logs.
pub(super) fn wait_for_pattern(
    shell: &Shell,
    machine: &Machine,
    at: (usize, usize),
    size: (usize, usize),
    name: &str,
) -> Result<()> {
    let difference = machine.logs.join(format!("{name}-difference.ppm"));
    let looked = poll_screen(machine, name, |screen| {
        compare(
            &screen.crop(at, size)?,
            |x, y| expected(x, y, size.0, size.1),
            "the client's pattern",
            &difference,
        )
    });
    let Err((_, e)) = looked? else {
        return Ok(());
    };
    let said = last_said(shell, &["beize-testclient", "beize-compositor"])?;
    Err(e.context(format!(
        "the client's window, {}x{} at {},{}, on {}; the client and the compositor last said: \
         {said:?}",
        size.0,
        size.1,
        at.0,
        at.1,
        screen_file(machine, name).display()
    )))
}

/// How often a wait looks at the screen again.
pub(super) const SCREEN_POLL: Duration = Duration::from_millis(100);

/// Where the screen a wait named `name` looked at last lies.
pub(super) fn screen_file(machine: &Machine, name: &str) -> PathBuf {
    machine.logs.join(format!("{name}-screen.ppm"))
}

/// Looks at the screen until `check` passes on it – at once, then every
/// `SCREEN_POLL`, for at most `SCREEN_TIMEOUT` – and returns what it gave;
/// if it never does, the last screen and why. Each look is
/// `<name>-screen.ppm` in the logs. Looked at at once, a check must not
/// pass on the screen before what it waits for.
pub(super) fn poll_screen<T>(
    machine: &Machine,
    name: &str,
    check: impl Fn(&Image) -> Result<T>,
) -> Result<std::result::Result<T, (Image, anyhow::Error)>> {
    let file = screen_file(machine, name);
    let deadline = Instant::now() + SCREEN_TIMEOUT;
    loop {
        screendump(&machine.qmp(), &file)?;
        let screen = Image::read_ppm(&fs::read(&file)?)?;
        match check(&screen) {
            Ok(value) => return Ok(Ok(value)),
            Err(e) if Instant::now() >= deadline => return Ok(Err((screen, e))),
            Err(_) => thread::sleep(SCREEN_POLL),
        }
    }
}

/// Waits until `window` on the screen shows exactly what `reference`
/// (relative to the repository) has, as openQA waits for a needle. If it
/// does not, the last window is `<name>-screen.png` in the logs, to look at
/// and to take over if the new look is wanted.
pub(super) fn wait_for_screen(
    paths: &Paths,
    machine: &Machine,
    window: Window,
    reference: &str,
    name: &str,
) -> Result<()> {
    let wanted = match fs::read(paths.root.join(reference)) {
        Ok(data) => Some(Image::read_png(&data).with_context(|| format!("reading {reference}"))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {reference}")),
    };
    wait_for_image(machine, window, wanted.as_ref(), reference, name)
}

/// Waits until `window` on the screen shows exactly `wanted` (named `what`
/// in errors); if it does not, the last window is `<name>-screen.png` in
/// the logs.
pub(super) fn wait_for_image(
    machine: &Machine,
    (at, size): Window,
    wanted: Option<&Image>,
    what: &str,
    name: &str,
) -> Result<()> {
    let difference = machine.logs.join(format!("{name}-difference.ppm"));
    let looked = poll_screen(machine, name, |screen| {
        let window = screen.crop(at, size)?;
        match wanted {
            None => Err(anyhow!("there is no {what} yet")),
            Some(r) if (r.width, r.height) != (window.width, window.height) => Err(anyhow!(
                "the window is {}x{}, {what} {}x{}",
                window.width,
                window.height,
                r.width,
                r.height
            )),
            Some(r) => compare(&window, |x, y| r.pixel(x, y), what, &difference),
        }
    });
    let Err((screen, e)) = looked? else {
        return Ok(());
    };
    // The window alone: to look at, and to take over as the reference.
    let shown = machine.logs.join(format!("{name}-screen.png"));
    screen.crop(at, size)?.write_png(&shown)?;
    Err(e.context(format!("the window is {}", shown.display())))
}

/// Waits until the menu bar lies along the top of the screen with the time
/// on its right, and the dock along the bottom; if they do not, the last
/// screen is in the logs.
pub(super) fn wait_for_panels(machine: &Machine) -> Result<()> {
    wait_until_screen(machine, "panels", |screen| {
        menu_bar(screen).and_then(|()| dock(screen).map(|_| ()))
    })
}

/// Waits until the screen passes `check`; if it does not, the last screen
/// is `<name>-screen.png` in the logs.
pub(super) fn wait_until_screen(
    machine: &Machine,
    name: &str,
    check: impl Fn(&Image) -> Result<()>,
) -> Result<()> {
    let Err((screen, e)) = poll_screen(machine, name, check)? else {
        return Ok(());
    };
    let shown = machine.logs.join(format!("{name}-screen.png"));
    screen.write_png(&shown)?;
    Err(e.context(format!("the screen is {}", shown.display())))
}

/// Whether the bar of the window at `window` shows its circles' marks
/// (`MARKS`): each pixel nearer the mark's colour than the fill's.
pub(super) fn marks(screen: &Image, window: (usize, usize)) -> Result<()> {
    for Mark {
        at: (x, y),
        fill,
        ink,
    } in MARKS
    {
        let pixel = screen.pixel(window.0 + x, window.1 + y);
        let from = |c: [u8; 3]| -> u32 {
            pixel
                .iter()
                .zip(c)
                .map(|(p, c)| u32::from(p.abs_diff(c)))
                .sum()
        };
        ensure!(
            from(ink) < from(fill),
            "the circle's pixel {x},{y} on the bar shows {pixel:?}, not its mark {ink:?} \
             (the fill is {fill:?})"
        );
    }
    Ok(())
}

/// Whether two pixels are the same but rounding.
pub(super) fn same(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter()
        .zip(b)
        .all(|(a, b)| a.abs_diff(b) <= CHANNEL_TOLERANCE)
}

/// Whether `channel` of a pixel on live glass leads the other two by what
/// a window behind it shows (`GLASS_SHOWS`).
pub(super) fn leads(pixel: [u8; 3], channel: usize) -> bool {
    (0..3)
        .filter(|&c| c != channel)
        .all(|c| pixel[channel] >= pixel[c].saturating_add(GLASS_SHOWS))
}

/// Whether two pixels are as close as neighbours on soft glass.
pub(super) fn soft(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= GLASS_SMOOTH)
}

/// The mean of all channels of row `y` from `xs.start` to `xs.end`.
pub(super) fn row_mean(screen: &Image, y: usize, xs: std::ops::Range<usize>) -> f64 {
    let n = xs.len().max(1) as f64;
    let sum: u64 = xs.flat_map(|x| screen.pixel(x, y)).map(u64::from).sum();
    sum as f64 / (3.0 * n)
}

/// Whether `screen` has the menu bar: the soft background across the top
/// but the pointer, the app's name and the time, the time's ink at the
/// right end, and no background of its own – no step at its lower edge.
/// Which time it shows depends on the minute, which name the shell's
/// journal says; Beize UI's text itself is checked exactly with the
/// windows.
pub(super) fn menu_bar(screen: &Image) -> Result<()> {
    ensure!(
        screen.width > CLOCK_WIDTH + APP_NAME_WIDTH && screen.height > MENU_BAR,
        "a screen of {}x{} has no room for the menu bar",
        screen.width,
        screen.height
    );
    let clock = screen.width - CLOCK_WIDTH;
    let mut other = 0;
    let mut ink = 0;
    for y in 0..MENU_BAR {
        for x in APP_NAME_WIDTH..screen.width {
            let shown = screen.pixel(x, y);
            if x >= clock {
                let beside = screen.pixel(clock - 1, y);
                ink += usize::from(
                    shown
                        .iter()
                        .zip(beside)
                        .any(|(&a, b)| a.abs_diff(b) > INK_FROM),
                );
            } else if !soft(shown, screen.pixel(x - 1, y)) {
                other += 1;
            }
        }
    }
    ensure!(
        other == 0,
        "{other} pixels of the menu bar's strip are not the soft background"
    );
    ensure!(
        ink >= CLOCK_INK,
        "the menu bar shows no time: {ink} pixels of ink at its right end"
    );
    // No background: what lies behind runs on across its lower edge.
    let row = |y| row_mean(screen, y, APP_NAME_WIDTH..clock);
    let edge = (row(MENU_BAR - 1) - row(MENU_BAR)).abs();
    ensure!(
        edge < BAR_EDGE,
        "the menu bar has a background: the screen steps by {edge:.1} at its lower edge"
    );
    Ok(())
}

/// The dock in `screen`, centred along the bottom: in its middle row (the
/// only rows without tiles lie in its round corners) its sides are the
/// first pair of pixels, mirrored about the middle, darker than what lies
/// outside and inside them – the glass's dark edge, as Apple's; at its
/// round corner the background shows. Its width says how many apps it has;
/// that number.
pub(super) fn dock(screen: &Image) -> Result<usize> {
    ensure!(
        screen.height > DOCK_ZONE + MENU_BAR,
        "a screen {} high has no room for the dock",
        screen.height
    );
    let top = screen.height - DOCK_ZONE;
    let row = top + DOCK_HEIGHT / 2;
    let centre = screen.width / 2;
    let mean = |x: usize| {
        f64::from(
            screen
                .pixel(x, row)
                .iter()
                .map(|&c| u32::from(c))
                .sum::<u32>(),
        ) / 3.0
    };
    // The edge's pixel at `x` dark, the outside lying `outward` of it.
    let edge = |x: usize, outward: bool| {
        let (out, inside) = if outward {
            (x + RIM_OUT, x - RIM_IN)
        } else {
            (x - RIM_OUT, x + RIM_IN)
        };
        mean(x) + RIM_BELOW <= mean(out).min(mean(inside))
    };
    // Rightwards from the screen's left edge to the first such pair.
    let left = (RIM_OUT..centre - RIM_IN)
        .find(|&x| edge(x, false) && edge(2 * centre - 1 - x, true))
        .ok_or_else(|| anyhow::anyhow!("no dock along the bottom of the screen"))?;
    let width = 2 * (centre - left);
    let step = (DOCK_TILE + DOCK_GAP) as f64;
    let tiles = (((width + DOCK_GAP) as f64 - (2 * DOCK_PADDING) as f64) / step).round();
    let tiles = tiles.max(0.0) as usize;
    let expected = 2 * DOCK_PADDING + tiles * DOCK_TILE + tiles.saturating_sub(1) * DOCK_GAP;
    ensure!(
        tiles >= 1 && width.abs_diff(expected) <= 1,
        "the dock is {width} wide, which is no number of apps"
    );
    // A round corner shows what is below, as the pixel beside the dock.
    ensure!(
        soft(screen.pixel(left, top), screen.pixel(left - 1, top)),
        "the dock's corner is square"
    );
    Ok(tiles)
}

/// The middle of the `i`th of `apps` tiles of the dock.
pub(super) fn dock_tile(screen: (usize, usize), apps: usize, i: usize) -> Result<(i32, i32)> {
    let width = 2 * DOCK_PADDING + apps * DOCK_TILE + apps.saturating_sub(1) * DOCK_GAP;
    let x = (screen.0 - width) / 2 + DOCK_PADDING + i * (DOCK_TILE + DOCK_GAP) + DOCK_TILE / 2;
    let y = screen.1 - DOCK_ZONE + DOCK_PADDING + DOCK_TILE / 2;
    Ok((i32::try_from(x)?, i32::try_from(y)?))
}

/// Waits until `window` on the screen no longer shows `shown` – the window
/// left; if it does not, the last screen is in the logs.
pub(super) fn wait_for_gone(
    machine: &Machine,
    (at, size): Window,
    shown: &Image,
    name: &str,
) -> Result<()> {
    let looked = poll_screen(machine, name, |screen| {
        ensure!(
            screen.crop(at, size)?.pixels != shown.pixels,
            "the window still shows"
        );
        Ok(())
    })?;
    let Err((screen, e)) = looked else {
        return Ok(());
    };
    let png = machine.logs.join(format!("{name}-screen.png"));
    screen.crop(at, size)?.write_png(&png)?;
    Err(e.context(png.display().to_string()))
}

/// Waits until the dock shows the test client's icon in the middle of its
/// tile at `at`; if it does not, the last screen is in the logs.
pub(super) fn wait_for_dock_icon(machine: &Machine, at: (i32, i32)) -> Result<()> {
    let (x, y) = (usize::try_from(at.0)?, usize::try_from(at.1)?);
    wait_until_screen(machine, "dock-icon", |screen| {
        let shown = screen.pixel(x, y);
        ensure!(
            shown
                .iter()
                .zip(CLIENT_ICON_COLOR)
                .all(|(&a, b)| a.abs_diff(b) <= CHANNEL_TOLERANCE),
            "the dock shows {shown:?} at {x},{y}, not the test client's icon {CLIENT_ICON_COLOR:?}"
        );
        Ok(())
    })
}

/// An RGB image.
pub(super) struct Image {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) pixels: Vec<u8>,
}

impl Image {
    /// A binary PPM (P6) with 8 bits a channel, as QEMU writes it.
    pub(super) fn read_ppm(data: &[u8]) -> Result<Self> {
        let mut fields = Vec::new();
        let mut at = 0;
        while fields.len() < 4 {
            while data.get(at).is_some_and(u8::is_ascii_whitespace) {
                at += 1;
            }
            if data.get(at) == Some(&b'#') {
                while data.get(at).is_some_and(|&b| b != b'\n') {
                    at += 1;
                }
                continue;
            }
            let start = at;
            while data.get(at).is_some_and(|b| !b.is_ascii_whitespace()) {
                at += 1;
            }
            ensure!(at > start, "a PPM header cut short");
            fields.push(std::str::from_utf8(&data[start..at])?);
        }
        ensure!(fields[0] == "P6", "not a binary PPM: {}", fields[0]);
        ensure!(fields[3] == "255", "not 8 bits a channel: {}", fields[3]);
        let (width, height): (usize, usize) = (fields[1].parse()?, fields[2].parse()?);
        // One whitespace byte ends the header.
        let pixels = data.get(at + 1..).unwrap_or_default().to_vec();
        ensure!(
            pixels.len() == width * height * 3,
            "a {width}x{height} PPM with {} bytes of pixels",
            pixels.len()
        );
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub(super) fn write_ppm(&self, file: &Path) -> Result<()> {
        let mut data = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        data.extend_from_slice(&self.pixels);
        Ok(fs::write(file, data)?)
    }

    /// A PNG with 8-bit RGB, as the references in the repository are.
    pub(super) fn read_png(data: &[u8]) -> Result<Self> {
        let mut reader = png::Decoder::new(std::io::Cursor::new(data)).read_info()?;
        let mut pixels = vec![0; reader.output_buffer_size().context("a PNG too large")?];
        let info = reader.next_frame(&mut pixels)?;
        ensure!(
            info.color_type == png::ColorType::Rgb && info.bit_depth == png::BitDepth::Eight,
            "not an 8-bit RGB PNG: {:?}, {:?}",
            info.color_type,
            info.bit_depth
        );
        pixels.truncate(info.buffer_size());
        Ok(Self {
            width: info.width as usize,
            height: info.height as usize,
            pixels,
        })
    }

    pub(super) fn write_png(&self, file: &Path) -> Result<()> {
        let out = std::io::BufWriter::new(fs::File::create(file)?);
        let mut encoder = png::Encoder::new(out, self.width as u32, self.height as u32);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&self.pixels)?;
        Ok(())
    }

    pub(super) fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let at = (y * self.width + x) * 3;
        [self.pixels[at], self.pixels[at + 1], self.pixels[at + 2]]
    }

    /// The part of the image `at` in `size` (a window on the screen).
    pub(super) fn crop(&self, at: (usize, usize), size: (usize, usize)) -> Result<Self> {
        ensure!(
            at.0 + size.0 <= self.width && at.1 + size.1 <= self.height,
            "a window of {}x{} at {},{} is not within the screen of {}x{}",
            size.0,
            size.1,
            at.0,
            at.1,
            self.width,
            self.height
        );
        let mut pixels = Vec::with_capacity(size.0 * size.1 * 3);
        for y in at.1..at.1 + size.1 {
            let row = (y * self.width + at.0) * 3;
            pixels.extend_from_slice(&self.pixels[row..row + size.0 * 3]);
        }
        Ok(Self {
            width: size.0,
            height: size.1,
            pixels,
        })
    }
}

/// The client's pattern at (x, y) of a window `width`×`height`, as the
/// client draws it (`x < w / 2` is left).
pub(super) fn expected(x: usize, y: usize, width: usize, height: usize) -> [u8; 3] {
    QUADRANTS[usize::from(x >= width / 2) + 2 * usize::from(y >= height / 2)]
}

/// Whether `window` shows exactly what is `wanted` (named `what` in
/// errors), but rounding and its round corners (`CORNER`); if not,
/// `difference` shows the pixels that differ in magenta over a dimmed
/// window.
pub(super) fn compare(
    window: &Image,
    wanted: impl Fn(usize, usize) -> [u8; 3],
    what: &str,
    difference: &Path,
) -> Result<()> {
    let mut marked = Image {
        width: window.width,
        height: window.height,
        pixels: window.pixels.iter().map(|c| c / 3).collect(),
    };
    let mut differing = 0;
    let (w, h) = (window.width, window.height);
    let corner =
        |x: usize, y: usize| (x < CORNER || x + CORNER >= w) && (y < CORNER || y + CORNER >= h);
    for y in 0..h {
        for x in 0..w {
            if corner(x, y) {
                continue;
            }
            let at = (y * window.width + x) * 3;
            let shown = &window.pixels[at..at + 3];
            if shown
                .iter()
                .zip(wanted(x, y))
                .any(|(s, w)| s.abs_diff(w) > CHANNEL_TOLERANCE)
            {
                differing += 1;
                marked.pixels[at..at + 3].copy_from_slice(&[255, 0, 255]);
            }
        }
    }
    if differing > 0 {
        marked.write_ppm(difference)?;
        bail!(
            "{differing} pixels of the window differ from {what}; they are magenta in {}",
            difference.display()
        );
    }
    Ok(())
}
