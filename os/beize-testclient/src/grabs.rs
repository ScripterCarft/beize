//! `--grabs`: what a client may do with menus that grab, and how much it may
//! send. One line each:
//! - `grabs without input:` a menu asked for with the keyboard but no input
//!   behind it (the serial of the keyboard's enter) must be refused;
//! - `grabs with a key:` one answering a key press (the test presses it) opens;
//! - `grabs minimized:` its window minimized, it must close, or the keys would
//!   go to a menu of a window no longer there;
//! - `grabs flood:` many long titles, sent as fast as the socket takes them;
//!   the shell must survive that (the test looks).

use std::fs::File;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;

use rustix::event::{PollFd, PollFlags, poll};
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_compositor::WlCompositor,
    wl_keyboard::{self, WlKeyboard},
    wl_registry::WlRegistry,
    wl_seat::{self, WlSeat},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::{self, XdgPopup},
    xdg_positioner::XdgPositioner,
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

/// How many titles, and how long each: what fills a socket many times over,
/// each just under the protocol's largest message.
const FLOOD: (usize, usize) = (3000, 3900);

/// The window, the menu's size.
const SIZE: i32 = 200;

/// Whose xdg_surface.
enum Which {
    Window,
    Menu,
}

#[derive(Default)]
struct App {
    window_configured: bool,
    menu_configured: bool,
    menu_done: bool,
    /// The serial of the keyboard's enter, of a key press.
    entered: Option<u32>,
    pressed: Option<u32>,
    enters: u32,
    leaves: u32,
}

fn bind<I>(globals: &GlobalList, qh: &QueueHandle<App>, version: u32) -> Result<I, String>
where
    I: wayland_client::Proxy + 'static,
    App: Dispatch<I, ()>,
{
    globals
        .bind(qh, 1..=version, ())
        .map_err(|e| format!("no {}: {e}", I::interface().name))
}

fn wait(
    queue: &mut EventQueue<App>,
    app: &mut App,
    until: impl Fn(&App) -> bool,
) -> Result<(), String> {
    while !until(app) {
        queue
            .blocking_dispatch(app)
            .map_err(|e| format!("dispatching: {e}"))?;
    }
    Ok(())
}

/// A white buffer.
fn buffer(shm: &WlShm, qh: &QueueHandle<App>) -> Result<WlBuffer, String> {
    let fd = rustix::fs::memfd_create("beize-testclient", rustix::fs::MemfdFlags::CLOEXEC)
        .map_err(|e| format!("memfd: {e}"))?;
    let file = File::from(fd);
    let pixels = vec![0xffu8; (SIZE * SIZE * 4) as usize];
    file.write_all_at(&pixels, 0)
        .map_err(|e| format!("writing the buffer: {e}"))?;
    let pool: WlShmPool = shm.create_pool(file.as_fd(), SIZE * SIZE * 4, qh, ());
    let buffer = pool.create_buffer(0, SIZE, SIZE, SIZE * 4, wl_shm::Format::Xrgb8888, qh, ());
    pool.destroy();
    Ok(buffer)
}

/// A menu of `parent`, grabbing with `serial`.
struct Menu {
    surface: WlSurface,
    xdg_surface: XdgSurface,
    popup: XdgPopup,
}

impl Menu {
    fn open(
        compositor: &WlCompositor,
        wm_base: &XdgWmBase,
        parent: &XdgSurface,
        seat: &WlSeat,
        serial: u32,
        qh: &QueueHandle<App>,
    ) -> Menu {
        let positioner = wm_base.create_positioner(qh, ());
        positioner.set_size(SIZE, SIZE);
        positioner.set_anchor_rect(0, 0, 10, 10);
        let surface = compositor.create_surface(qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, qh, Which::Menu);
        let popup = xdg_surface.get_popup(Some(parent), &positioner, qh, ());
        positioner.destroy();
        popup.grab(seat, serial);
        surface.commit();
        Menu {
            surface,
            xdg_surface,
            popup,
        }
    }

    fn close(self) {
        self.popup.destroy();
        self.xdg_surface.destroy();
        self.surface.destroy();
    }
}

/// Sends what is queued, waiting while the socket is full.
fn send_all(connection: &Connection) -> Result<(), String> {
    loop {
        match connection.flush() {
            Ok(()) => return Ok(()),
            Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let backend = connection.backend();
                let fd = backend.poll_fd();
                let mut fds = [PollFd::new(&fd, PollFlags::OUT)];
                poll(&mut fds, None).map_err(|e| format!("poll: {e}"))?;
            }
            Err(e) => return Err(format!("sending: {e}")),
        }
    }
}

pub fn run() -> Result<(), String> {
    let connection = Connection::connect_to_env().map_err(|e| format!("connecting: {e}"))?;
    let (globals, mut queue) =
        registry_queue_init::<App>(&connection).map_err(|e| format!("registry: {e}"))?;
    let qh = queue.handle();
    let compositor: WlCompositor = bind(&globals, &qh, 4)?;
    let shm: WlShm = bind(&globals, &qh, 1)?;
    let wm_base: XdgWmBase = bind(&globals, &qh, 1)?;
    let seat: WlSeat = bind(&globals, &qh, 5)?;
    let mut app = App::default();

    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, Which::Window);
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_app_id("beize-testclient".into());
    surface.commit();
    wait(&mut queue, &mut app, |a| a.window_configured)?;
    let window_buffer = buffer(&shm, &qh)?;
    surface.attach(Some(&window_buffer), 0, 0);
    surface.commit();
    wait(&mut queue, &mut app, |a| a.entered.is_some())?;

    let entered = app.entered.unwrap_or(0);
    let menu = Menu::open(&compositor, &wm_base, &xdg_surface, &seat, entered, &qh);
    queue
        .roundtrip(&mut app)
        .map_err(|e| format!("roundtrip: {e}"))?;
    let answer = if app.menu_done { "refused" } else { "granted" };
    println!("grabs without input: {answer}");
    menu.close();
    app.menu_done = false;
    app.menu_configured = false;

    wait(&mut queue, &mut app, |a| a.pressed.is_some())?;
    let pressed = app.pressed.unwrap_or(0);
    let menu = Menu::open(&compositor, &wm_base, &xdg_surface, &seat, pressed, &qh);
    wait(&mut queue, &mut app, |a| a.menu_configured || a.menu_done)?;
    let menu_buffer = buffer(&shm, &qh)?;
    menu.surface.attach(Some(&menu_buffer), 0, 0);
    menu.surface.commit();
    queue
        .roundtrip(&mut app)
        .map_err(|e| format!("roundtrip: {e}"))?;
    let answer = if app.menu_done { "refused" } else { "open" };
    println!("grabs with a key: {answer}");

    toplevel.set_minimized();
    queue
        .roundtrip(&mut app)
        .map_err(|e| format!("roundtrip: {e}"))?;
    let answer = if app.menu_done { "closed" } else { "kept" };
    println!("grabs minimized: {answer}");
    menu.close();

    // A second window, mapped, then a null buffer: it must go, with the keyboard.
    let second = compositor.create_surface(&qh, ());
    let second_xdg = wm_base.get_xdg_surface(&second, &qh, Which::Window);
    let second_toplevel = second_xdg.get_toplevel(&qh, ());
    second_toplevel.set_app_id("beize-testclient".into());
    app.window_configured = false;
    second.commit();
    wait(&mut queue, &mut app, |a| a.window_configured)?;
    let enters = app.enters;
    let second_buffer = buffer(&shm, &qh)?;
    second.attach(Some(&second_buffer), 0, 0);
    second.commit();
    wait(&mut queue, &mut app, |a| a.enters > enters)?;
    let leaves = app.leaves;
    second.attach(None, 0, 0);
    second.commit();
    queue
        .roundtrip(&mut app)
        .map_err(|e| format!("roundtrip: {e}"))?;
    queue
        .roundtrip(&mut app)
        .map_err(|e| format!("roundtrip: {e}"))?;
    let answer = if app.leaves > leaves {
        "keyboard gone"
    } else {
        "kept"
    };
    println!("grabs unmapped: {answer}");

    let (titles, length) = FLOOD;
    for i in 0..titles {
        let mut title = "x".repeat(length);
        title.push_str(&i.to_string());
        toplevel.set_title(title);
        send_all(&connection)?;
    }
    queue
        .roundtrip(&mut app)
        .map_err(|e| format!("roundtrip: {e}"))?;
    println!("grabs flood: sent");
    Ok(())
}

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(
        _: &mut App,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for App {
    fn event(
        _: &mut App,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, Which> for App {
    fn event(
        app: &mut App,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        which: &Which,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            match which {
                Which::Window => app.window_configured = true,
                Which::Menu => app.menu_configured = true,
            }
        }
    }
}

impl Dispatch<XdgToplevel, ()> for App {
    fn event(
        _: &mut App,
        _: &XdgToplevel,
        _: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch<XdgPopup, ()> for App {
    fn event(
        app: &mut App,
        _: &XdgPopup,
        event: xdg_popup::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if matches!(event, xdg_popup::Event::PopupDone) {
            app.menu_done = true;
        }
    }
}

impl Dispatch<WlSeat, ()> for App {
    fn event(
        _: &mut App,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
            && capabilities.contains(wl_seat::Capability::Keyboard)
        {
            seat.get_keyboard(qh, ());
        }
    }
}

impl Dispatch<WlKeyboard, ()> for App {
    fn event(
        app: &mut App,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        match event {
            wl_keyboard::Event::Enter { serial, .. } => {
                app.entered.get_or_insert(serial);
                app.enters += 1;
            }
            wl_keyboard::Event::Leave { .. } => app.leaves += 1,
            wl_keyboard::Event::Key {
                serial,
                state: WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } => {
                app.pressed.get_or_insert(serial);
            }
            _ => {}
        }
    }
}

delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore WlShm);
delegate_noop!(App: ignore WlShmPool);
delegate_noop!(App: ignore WlBuffer);
delegate_noop!(App: ignore XdgPositioner);
