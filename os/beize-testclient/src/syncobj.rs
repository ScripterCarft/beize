//! `--syncobj`: explicit sync (linux-drm-syncobj-v1) from a client's side,
//! as Mesa's Vulkan WSI uses it on a real GPU – with dmabufs of the
//! compositor's own GPU (dumb buffers, the VM's virtio-gpu; their content
//! is black) and DRM timeline syncobjs it signals itself, from the CPU.
//! One window shows them; a subsurface of it (shm) shows a frame of its
//! own.
//!
//! Each check is about the protocol's order and prints one line:
//! - `syncobj acquire:` a commit whose acquire point is not signalled must
//!   not show – the subsurface's commit after it shows first –, and shows
//!   once the point is signalled, at a vblank after that;
//! - `syncobj release:` buffers committed on "frame done" of the one before
//!   (which comes when that frame is drawn, before its vblank): no
//!   buffer's release point may signal before the vblank of the frame that
//!   showed it – the compositor's GPU may read it until then. The client
//!   watches every release point while it runs and notes when it sees the
//!   signal, never before it was given: a release seen before the vblank
//!   is one. (The kernel's own time does not tell: a point signalled from
//!   the CPU gets the stub fence, stamped when the kernel started.) A
//!   compositor that released at the replacing commit, ~a refresh before
//!   the vblank, would be seen early unless this client looked that much
//!   later; how many buffers were replaced while their frame was on its
//!   way says whether the check had something to catch;
//! - `syncobj protocol:` a buffer committed without points raises
//!   no_acquire_point.

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use rustix::event::{EventfdFlags, PollFd, PollFlags, Timespec, eventfd, poll};
use rustix::ioctl::{Updater, ioctl, opcode};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_callback::{self, WlCallback},
    wl_compositor::WlCompositor,
    wl_registry::WlRegistry,
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_subcompositor::WlSubcompositor,
    wl_subsurface::WlSubsurface,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
    zwp_linux_dmabuf_feedback_v1::{self, ZwpLinuxDmabufFeedbackV1},
    zwp_linux_dmabuf_v1::{self, ZwpLinuxDmabufV1},
};
use wayland_protocols::wp::linux_drm_syncobj::v1::client::{
    wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1,
    wp_linux_drm_syncobj_surface_v1::{self, WpLinuxDrmSyncobjSurfaceV1},
    wp_linux_drm_syncobj_timeline_v1::WpLinuxDrmSyncobjTimelineV1,
};
use wayland_protocols::wp::presentation_time::client::{
    wp_presentation::{self, WpPresentation},
    wp_presentation_feedback::{self, WpPresentationFeedback},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

use crate::{Result, missing, monotonic};

/// The window's buffers: dumb buffers of this size, XRGB8888, linear.
const SIZE: (u32, u32) = (320, 240);
/// The subsurface's, in shm.
const SUB_SIZE: i32 = 64;
/// Buffers the window takes turns with, each with its own release
/// timeline (the protocol advises one per buffer).
const BUFFERS: usize = 3;
/// Rounds of the release check.
const ROUNDS: usize = 30;
/// How long any one step may take before the check gives up: a missing
/// answer, never a measure of time.
const STEP: Duration = Duration::from_secs(10);

const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
const LINEAR: u64 = 0;

// The DRM ioctls (linux/drm.h, linux/drm_mode.h).
#[repr(C)]
struct CreateDumb {
    height: u32,
    width: u32,
    bpp: u32,
    flags: u32,
    handle: u32,
    pitch: u32,
    size: u64,
}
#[repr(C)]
struct PrimeHandle {
    handle: u32,
    flags: u32,
    fd: i32,
}
#[repr(C)]
struct SyncobjCreate {
    handle: u32,
    flags: u32,
}
#[repr(C)]
struct SyncobjHandle {
    handle: u32,
    flags: u32,
    fd: i32,
    pad: u32,
    point: u64,
}
#[repr(C)]
struct TimelineArray {
    handles: u64,
    points: u64,
    count_handles: u32,
    flags: u32,
}
#[repr(C)]
struct SyncobjEventfd {
    handle: u32,
    flags: u32,
    point: u64,
    fd: i32,
    pad: u32,
}
const CREATE_DUMB: rustix::ioctl::Opcode = opcode::read_write::<CreateDumb>(b'd', 0xb2);
const PRIME_HANDLE_TO_FD: rustix::ioctl::Opcode = opcode::read_write::<PrimeHandle>(b'd', 0x2d);
const SYNCOBJ_CREATE: rustix::ioctl::Opcode = opcode::read_write::<SyncobjCreate>(b'd', 0xbf);
const SYNCOBJ_HANDLE_TO_FD: rustix::ioctl::Opcode = opcode::read_write::<SyncobjHandle>(b'd', 0xc1);
const SYNCOBJ_TIMELINE_SIGNAL: rustix::ioctl::Opcode =
    opcode::read_write::<TimelineArray>(b'd', 0xcd);
const SYNCOBJ_EVENTFD: rustix::ioctl::Opcode = opcode::read_write::<SyncobjEventfd>(b'd', 0xcf);

/// An ioctl that fills `arg` in.
fn drm<const OPCODE: rustix::ioctl::Opcode, T>(fd: BorrowedFd<'_>, arg: &mut T) -> Result<()> {
    // SAFETY: every argument here has the kernel's layout (the structs
    // above) and lives during the call; the kernel writes only into it.
    unsafe { ioctl(fd, Updater::<OPCODE, T>::new(arg)) }.map_err(|e| format!("DRM ioctl: {e}"))
}

/// A timeline syncobj of the client's device.
struct Timeline {
    handle: u32,
    wayland: WpLinuxDrmSyncobjTimelineV1,
}

impl Timeline {
    fn new(
        gpu: BorrowedFd<'_>,
        manager: &WpLinuxDrmSyncobjManagerV1,
        qh: &QueueHandle<Test>,
    ) -> Result<Self> {
        let mut create = SyncobjCreate {
            handle: 0,
            flags: 0,
        };
        drm::<SYNCOBJ_CREATE, _>(gpu, &mut create)?;
        let mut export = SyncobjHandle {
            handle: create.handle,
            flags: 0,
            fd: -1,
            pad: 0,
            point: 0,
        };
        drm::<SYNCOBJ_HANDLE_TO_FD, _>(gpu, &mut export)?;
        // SAFETY: a new file descriptor the kernel returned.
        let fd = unsafe { OwnedFd::from_raw_fd(export.fd) };
        let wayland = manager.import_timeline(fd.as_fd(), qh, ());
        Ok(Timeline {
            handle: create.handle,
            wayland,
        })
    }

    /// Signals `point`, as a GPU would once the buffer is written.
    fn signal(&self, gpu: BorrowedFd<'_>, point: u64) -> Result<()> {
        let handles = [self.handle];
        let points = [point];
        let mut arg = TimelineArray {
            handles: handles.as_ptr() as u64,
            points: points.as_ptr() as u64,
            count_handles: 1,
            flags: 0,
        };
        drm::<SYNCOBJ_TIMELINE_SIGNAL, _>(gpu, &mut arg)
    }

    /// An eventfd that becomes readable once `point` is signalled.
    fn eventfd(&self, gpu: BorrowedFd<'_>, point: u64) -> Result<OwnedFd> {
        let fd = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
            .map_err(|e| format!("eventfd: {e}"))?;
        let mut arg = SyncobjEventfd {
            handle: self.handle,
            flags: 0,
            point,
            fd: fd.as_raw_fd(),
            pad: 0,
        };
        drm::<SYNCOBJ_EVENTFD, _>(gpu, &mut arg)?;
        Ok(fd)
    }
}

/// A dumb buffer of the compositor's GPU, as a wl_buffer, with its release
/// timeline, the point last asked for and the release watched for it.
struct Dumb {
    buffer: WlBuffer,
    release: Timeline,
    point: u64,
    last: Option<usize>,
}

/// What a commit's presentation feedback said.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Answer {
    Waiting,
    Presented(Duration),
    Discarded,
}

struct Test {
    formats: Vec<(u32, u64)>,
    main_device: Option<u64>,
    feedback_done: bool,
    configured: bool,
    /// The answers to the feedbacks asked for, by number.
    answers: Vec<Answer>,
    /// "Frame done" of the window's last commit came.
    done: bool,
    /// The release points watched (their number, an eventfd), and when
    /// each was seen signalled, by number.
    watching: Vec<(usize, OwnedFd)>,
    released: Vec<Option<Duration>>,
}

/// Runs the checks; their lines are printed as each ends.
pub fn run() -> Result<()> {
    let connection = Connection::connect_to_env().map_err(|e| format!("connecting: {e}"))?;
    let (globals, mut queue) =
        registry_queue_init::<Test>(&connection).map_err(|e| format!("registry: {e}"))?;
    let qh = queue.handle();
    let compositor: WlCompositor = globals
        .bind(&qh, 4..=6, ())
        .map_err(missing("wl_compositor"))?;
    let subcompositor: WlSubcompositor = globals
        .bind(&qh, 1..=1, ())
        .map_err(missing("wl_subcompositor"))?;
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).map_err(missing("wl_shm"))?;
    let wm_base: XdgWmBase = globals
        .bind(&qh, 1..=6, ())
        .map_err(missing("xdg_wm_base"))?;
    let presentation: WpPresentation = globals
        .bind(&qh, 1..=2, ())
        .map_err(missing("wp_presentation"))?;
    let dmabuf: ZwpLinuxDmabufV1 = globals
        .bind(&qh, 4..=5, ())
        .map_err(missing("zwp_linux_dmabuf_v1"))?;
    let manager: WpLinuxDrmSyncobjManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(missing("wp_linux_drm_syncobj_manager_v1"))?;
    let mut test = Test {
        formats: Vec::new(),
        main_device: None,
        feedback_done: false,
        configured: false,
        answers: Vec::new(),
        done: false,
        watching: Vec::new(),
        released: Vec::new(),
    };

    // The compositor's GPU and what it takes (linux-dmabuf feedback).
    let _feedback = dmabuf.get_default_feedback(&qh, ());
    until(
        &mut queue,
        &mut test,
        |t| t.feedback_done,
        "dmabuf feedback",
    )?;
    let device = test
        .main_device
        .ok_or("no main device in the dmabuf feedback")?;
    if !test.formats.contains(&(XRGB8888, LINEAR)) {
        return Err("the compositor takes no linear XRGB8888 dmabuf".into());
    }
    let path = format!(
        "/dev/char/{}:{}",
        rustix::fs::major(device),
        rustix::fs::minor(device)
    );
    let gpu = rustix::fs::open(
        &path,
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|e| format!("opening {path}: {e}"))?;
    let gpu = gpu.as_fd();

    let surface = compositor.create_surface(&qh, ());
    let sync = manager.get_surface(&surface, &qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("beize-testclient".into());
    toplevel.set_app_id("beize-testclient-syncobj".into());
    surface.commit();
    until(&mut queue, &mut test, |t| t.configured, "configure")?;

    let acquire = Timeline::new(gpu, &manager, &qh)?;
    let mut dumbs = Vec::new();
    for _ in 0..BUFFERS {
        dumbs.push(dumb(gpu, &dmabuf, &manager, &qh)?);
    }
    let mut window = Window {
        surface: &surface,
        sync: &sync,
        presentation: &presentation,
        acquire: &acquire,
        acquired: 0,
        gpu,
    };
    // The window maps with the first buffer, its point signalled at once.
    let first = window.commit(&mut dumbs[0], true, &mut test, &qh)?;
    until(
        &mut queue,
        &mut test,
        |t| t.answers[first] != Answer::Waiting,
        "the first frame",
    )?;

    // The subsurface, desynchronised: it shows without the window.
    let sub_surface = compositor.create_surface(&qh, ());
    let subsurface = subcompositor.get_subsurface(&sub_surface, &surface, &qh, ());
    subsurface.set_desync();
    let sub_buffer = shm_buffer(&shm, &qh)?;
    surface.commit();

    // Acquire: held back until signalled.
    let held = window.commit(&mut dumbs[1], false, &mut test, &qh)?;
    sub_surface.attach(Some(&sub_buffer), 0, 0);
    sub_surface.damage_buffer(0, 0, SUB_SIZE, SUB_SIZE);
    let sub = feedback(&presentation, &sub_surface, &mut test, &qh);
    sub_surface.commit();
    until(
        &mut queue,
        &mut test,
        |t| t.answers[sub] != Answer::Waiting,
        "the subsurface's frame",
    )?;
    let early = test.answers[held];
    let signalled = monotonic();
    acquire.signal(gpu, window.acquired)?;
    until(
        &mut queue,
        &mut test,
        |t| t.answers[held] != Answer::Waiting,
        "the held frame",
    )?;
    match (early, test.answers[held]) {
        (Answer::Waiting, Answer::Presented(at)) if at > signalled => {
            println!("syncobj acquire: held until signalled, shown after");
        }
        (early, late) => println!(
            "syncobj acquire: wrong – before the signal {early:?}, after it {late:?} (signalled at {signalled:?})"
        ),
    }

    // Release: each buffer committed on "frame done" of the one before,
    // once its own last release point signalled, as Mesa waits.
    let mut rounds: Vec<(usize, usize, bool, Duration)> = Vec::new();
    let mut current = 1;
    for _ in 0..=ROUNDS {
        let next = (current + 1) % BUFFERS;
        if let Some(last) = dumbs[next].last {
            until(
                &mut queue,
                &mut test,
                |t| t.released[last].is_some(),
                "a release point",
            )?;
        }
        test.done = false;
        let callback = surface.frame(&qh, ());
        let committed = monotonic();
        let answer = window.commit(&mut dumbs[next], true, &mut test, &qh)?;
        until(&mut queue, &mut test, |t| t.done, "frame done")?;
        drop(callback);
        // Replaced while its frame is on its way: the next round commits
        // now, before this answer came. The last commit only replaces.
        let in_flight = test.answers[answer] == Answer::Waiting;
        if let Some(release) = dumbs[next].last
            && rounds.len() < ROUNDS
        {
            rounds.push((answer, release, in_flight, committed));
        }
        current = next;
    }
    until(
        &mut queue,
        &mut test,
        |t| {
            rounds
                .iter()
                .all(|&(a, r, _, _)| t.answers[a] != Answer::Waiting && t.released[r].is_some())
        },
        "the rounds' answers and releases",
    )?;
    let mut early = 0;
    let mut checked = 0;
    let mut in_flight = 0;
    let mut first_early = None;
    for (round, &(answer, release, flying, _)) in rounds.iter().enumerate() {
        let (Answer::Presented(at), Some(released)) =
            (test.answers[answer], test.released[release])
        else {
            continue;
        };
        checked += 1;
        in_flight += usize::from(flying);
        if released < at {
            early += 1;
            first_early.get_or_insert((round, at, released));
        }
    }
    if early == 0 {
        println!(
            "syncobj release: {checked} of {checked} after their vblank, {in_flight} replaced on their way"
        );
    } else {
        println!(
            "syncobj release: {early} of {checked} before their vblank, {in_flight} replaced on their way"
        );
        // When, to tell a release at the replacing commit from one at the
        // vblank (CLOCK_MONOTONIC, ms).
        if let Some((round, at, released)) = first_early {
            let ms = |d: Duration| d.as_secs_f64() * 1000.0;
            println!(
                "syncobj release detail: round {round} committed {:.3}, presented {:.3}, released {:.3}, next committed {:?}",
                ms(rounds[round].3),
                ms(at),
                ms(released),
                rounds.get(round + 1).map(|r| ms(r.3)),
            );
        }
    }

    // Protocol: a buffer without points.
    sub_surface.destroy();
    subsurface.destroy();
    let bare = compositor.create_surface(&qh, ());
    let _bare_sync = manager.get_surface(&bare, &qh, ());
    let shm_only = shm_buffer(&shm, &qh)?;
    bare.attach(Some(&shm_only), 0, 0);
    bare.commit();
    let raised = until(&mut queue, &mut test, |_| false, "a protocol error");
    match connection.protocol_error() {
        Some(e)
            if e.object_interface == "wp_linux_drm_syncobj_surface_v1"
                && e.code == wp_linux_drm_syncobj_surface_v1::Error::NoAcquirePoint as u32 =>
        {
            println!("syncobj protocol: no_acquire_point raised");
        }
        other => println!("syncobj protocol: wrong – {other:?} ({raised:?})"),
    }
    let _ = (toplevel, xdg_surface);
    Ok(())
}

/// Handles events until `done` holds, at most `STEP`.
fn until(
    queue: &mut EventQueue<Test>,
    test: &mut Test,
    done: impl Fn(&Test) -> bool,
    what: &str,
) -> Result<()> {
    let deadline = Instant::now() + STEP;
    loop {
        if done(test) {
            return Ok(());
        }
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return Err(format!("no {what} within {} s", STEP.as_secs()));
        };
        dispatch(queue, test, left)?;
    }
}

/// One round of events and release points, waiting at most `timeout`.
fn dispatch(queue: &mut EventQueue<Test>, test: &mut Test, timeout: Duration) -> Result<()> {
    queue
        .dispatch_pending(test)
        .map_err(|e| format!("dispatching: {e}"))?;
    queue.flush().map_err(|e| format!("sending: {e}"))?;
    let Some(guard) = queue.prepare_read() else {
        return Ok(());
    };
    let connection = guard.connection_fd();
    let mut polled: Vec<PollFd<'_>> = std::iter::once(PollFd::new(&connection, PollFlags::IN))
        .chain(
            test.watching
                .iter()
                .map(|(_, fd)| PollFd::new(fd, PollFlags::IN)),
        )
        .collect();
    let timeout = Timespec::try_from(timeout).map_err(|e| e.to_string())?;
    poll(&mut polled, Some(&timeout)).map_err(|e| format!("poll: {e}"))?;
    let seen = monotonic();
    let readable = polled[0].revents().contains(PollFlags::IN);
    let signalled: Vec<usize> = polled[1..]
        .iter()
        .enumerate()
        .filter(|(_, p)| p.revents().contains(PollFlags::IN))
        .map(|(i, _)| i)
        .collect();
    drop(polled);
    for &i in signalled.iter().rev() {
        let (number, _) = test.watching.remove(i);
        test.released[number] = Some(seen);
    }
    if readable {
        match guard.read() {
            Ok(_) => {}
            Err(wayland_client::backend::WaylandError::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(format!("reading: {e}")),
        }
    } else {
        drop(guard);
    }
    queue
        .dispatch_pending(test)
        .map_err(|e| format!("dispatching: {e}"))?;
    Ok(())
}

/// The window with its sync points.
struct Window<'a> {
    surface: &'a WlSurface,
    sync: &'a WpLinuxDrmSyncobjSurfaceV1,
    presentation: &'a WpPresentation,
    acquire: &'a Timeline,
    acquired: u64,
    gpu: BorrowedFd<'a>,
}

impl Window<'_> {
    /// Commits `dumb` with the next acquire point – signalled first if
    /// `ready` – and its next release point, which is watched from now on;
    /// the number of its feedback.
    fn commit(
        &mut self,
        dumb: &mut Dumb,
        ready: bool,
        test: &mut Test,
        qh: &QueueHandle<Test>,
    ) -> Result<usize> {
        self.acquired += 1;
        if ready {
            self.acquire.signal(self.gpu, self.acquired)?;
        }
        dumb.point += 1;
        let watched = dumb.release.eventfd(self.gpu, dumb.point)?;
        let number = test.released.len();
        test.released.push(None);
        test.watching.push((number, watched));
        dumb.last = Some(number);
        self.surface.attach(Some(&dumb.buffer), 0, 0);
        self.surface
            .damage_buffer(0, 0, SIZE.0 as i32, SIZE.1 as i32);
        let acquired = self.acquired;
        self.sync.set_acquire_point(
            &self.acquire.wayland,
            (acquired >> 32) as u32,
            acquired as u32,
        );
        self.sync.set_release_point(
            &dumb.release.wayland,
            (dumb.point >> 32) as u32,
            dumb.point as u32,
        );
        let answer = feedback(self.presentation, self.surface, test, qh);
        self.surface.commit();
        Ok(answer)
    }
}

/// A dumb buffer of `SIZE` on the compositor's GPU, as a dmabuf wl_buffer.
fn dumb(
    gpu: BorrowedFd<'_>,
    dmabuf: &ZwpLinuxDmabufV1,
    manager: &WpLinuxDrmSyncobjManagerV1,
    qh: &QueueHandle<Test>,
) -> Result<Dumb> {
    let mut create = CreateDumb {
        height: SIZE.1,
        width: SIZE.0,
        bpp: 32,
        flags: 0,
        handle: 0,
        pitch: 0,
        size: 0,
    };
    drm::<CREATE_DUMB, _>(gpu, &mut create)?;
    let mut prime = PrimeHandle {
        handle: create.handle,
        flags: (rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::RDWR).bits(),
        fd: -1,
    };
    drm::<PRIME_HANDLE_TO_FD, _>(gpu, &mut prime)?;
    // SAFETY: a new file descriptor the kernel returned.
    let fd = unsafe { OwnedFd::from_raw_fd(prime.fd) };
    let params = dmabuf.create_params(qh, ());
    params.add(
        fd.as_fd(),
        0,
        0,
        create.pitch,
        (LINEAR >> 32) as u32,
        LINEAR as u32,
    );
    let buffer = params.create_immed(
        SIZE.0 as i32,
        SIZE.1 as i32,
        XRGB8888,
        zwp_linux_buffer_params_v1::Flags::empty(),
        qh,
        (),
    );
    params.destroy();
    Ok(Dumb {
        buffer,
        release: Timeline::new(gpu, manager, qh)?,
        point: 0,
        last: None,
    })
}

/// An shm buffer for the subsurface.
fn shm_buffer(shm: &WlShm, qh: &QueueHandle<Test>) -> Result<WlBuffer> {
    let bytes = SUB_SIZE * SUB_SIZE * 4;
    let fd = rustix::fs::memfd_create("beize-testclient", rustix::fs::MemfdFlags::CLOEXEC)
        .map_err(|e| format!("memfd: {e}"))?;
    rustix::fs::ftruncate(&fd, bytes as u64).map_err(|e| format!("sizing: {e}"))?;
    let pool: WlShmPool = shm.create_pool(fd.as_fd(), bytes, qh, ());
    let buffer = pool.create_buffer(
        0,
        SUB_SIZE,
        SUB_SIZE,
        SUB_SIZE * 4,
        wl_shm::Format::Xrgb8888,
        qh,
        (),
    );
    pool.destroy();
    Ok(buffer)
}

/// Asks for `surface`'s next commit to be told when it showed; its number.
fn feedback(
    presentation: &WpPresentation,
    surface: &WlSurface,
    test: &mut Test,
    qh: &QueueHandle<Test>,
) -> usize {
    let number = test.answers.len();
    test.answers.push(Answer::Waiting);
    presentation.feedback(surface, qh, number);
    number
}

impl Dispatch<WlRegistry, GlobalListContents> for Test {
    fn event(
        _: &mut Test,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Test>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for Test {
    fn event(
        _: &mut Test,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Test>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Test {
    fn event(
        test: &mut Test,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Test>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            test.configured = true;
        }
    }
}

impl Dispatch<WlCallback, ()> for Test {
    fn event(
        test: &mut Test,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Test>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            test.done = true;
        }
    }
}

impl Dispatch<WpPresentationFeedback, usize> for Test {
    fn event(
        test: &mut Test,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        number: &usize,
        _: &Connection,
        _: &QueueHandle<Test>,
    ) {
        let answer = match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                ..
            } => Answer::Presented(Duration::new(
                (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo),
                tv_nsec,
            )),
            wp_presentation_feedback::Event::Discarded => Answer::Discarded,
            _ => return,
        };
        if let Some(slot) = test.answers.get_mut(*number) {
            *slot = answer;
        }
    }
}

impl Dispatch<ZwpLinuxDmabufFeedbackV1, ()> for Test {
    fn event(
        test: &mut Test,
        _: &ZwpLinuxDmabufFeedbackV1,
        event: zwp_linux_dmabuf_feedback_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Test>,
    ) {
        match event {
            zwp_linux_dmabuf_feedback_v1::Event::MainDevice { device } => {
                test.main_device = device
                    .get(..8)
                    .and_then(|b| b.try_into().ok())
                    .map(u64::from_ne_bytes);
            }
            zwp_linux_dmabuf_feedback_v1::Event::FormatTable { fd, size } => {
                test.formats = format_table(&fd, size);
            }
            zwp_linux_dmabuf_feedback_v1::Event::Done => test.feedback_done = true,
            _ => {}
        }
    }
}

/// The (format, modifier) pairs of a dmabuf feedback's table: 16 bytes
/// each, read from its file.
fn format_table(fd: &OwnedFd, size: u32) -> Vec<(u32, u64)> {
    let mut table = vec![0u8; size as usize];
    if rustix::io::pread(fd, &mut table, 0).is_err() {
        return Vec::new();
    }
    let (entries, _) = table.as_chunks::<16>();
    entries
        .iter()
        .map(
            |&[f0, f1, f2, f3, _, _, _, _, m0, m1, m2, m3, m4, m5, m6, m7]| {
                (
                    u32::from_ne_bytes([f0, f1, f2, f3]),
                    u64::from_ne_bytes([m0, m1, m2, m3, m4, m5, m6, m7]),
                )
            },
        )
        .collect()
}

impl Dispatch<ZwpLinuxBufferParamsV1, ()> for Test {
    fn event(
        _: &mut Test,
        _: &ZwpLinuxBufferParamsV1,
        event: zwp_linux_buffer_params_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Test>,
    ) {
        if let zwp_linux_buffer_params_v1::Event::Failed = event {
            eprintln!("beize-testclient: a dmabuf was refused");
        }
    }
}

impl Dispatch<ZwpLinuxDmabufV1, ()> for Test {
    fn event(
        _: &mut Test,
        _: &ZwpLinuxDmabufV1,
        _: zwp_linux_dmabuf_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Test>,
    ) {
    }
}

impl Dispatch<WpPresentation, ()> for Test {
    fn event(
        _: &mut Test,
        _: &WpPresentation,
        _: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Test>,
    ) {
    }
}

impl Dispatch<XdgToplevel, ()> for Test {
    fn event(
        _: &mut Test,
        _: &XdgToplevel,
        _: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Test>,
    ) {
    }
}

impl Dispatch<WlBuffer, ()> for Test {
    fn event(
        _: &mut Test,
        _: &WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Test>,
    ) {
    }
}

delegate_noop!(Test: ignore WlCompositor);
delegate_noop!(Test: ignore WlSubcompositor);
delegate_noop!(Test: ignore WlSubsurface);
delegate_noop!(Test: ignore WlSurface);
delegate_noop!(Test: ignore WlShm);
delegate_noop!(Test: ignore WlShmPool);
delegate_noop!(Test: ignore WpLinuxDrmSyncobjManagerV1);
delegate_noop!(Test: ignore WpLinuxDrmSyncobjSurfaceV1);
delegate_noop!(Test: ignore WpLinuxDrmSyncobjTimelineV1);
