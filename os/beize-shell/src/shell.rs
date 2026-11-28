//! The shell's state and one turn of its event loop (`Shell::turn`): what
//! the compositor, the minute's timer, networkd and polkit said, handled
//! source by source in a fixed order, then what changed drawn. Each panel
//! and popup is drawn once the compositor has configured it and it is
//! ready for a frame.

use super::*;

/// What one dispatch brought: each window's events and the watches ready.
struct Turn {
    ready: Vec<usize>,
    desktop: bool,
    bar: beize_ui::Events,
    dock: beize_ui::Events,
    open: Option<beize_ui::Events>,
    submenu: Option<beize_ui::Events>,
    asking: Option<beize_ui::Events>,
    switching: Option<beize_ui::Events>,
    auth: Option<beize_ui::Events>,
}

/// A menu to open once the one open is closed: which, its anchor, its
/// alignment.
type Opening = Option<(Open, Rect, Align)>;

/// An app's name and icon from its desktop file, read once each.
fn names(known: &mut HashMap<String, apps::App>, app_id: &str) -> apps::App {
    known
        .entry(app_id.to_owned())
        .or_insert_with(|| apps::app(app_id, dock::ICON_PIXELS))
        .clone()
}

/// Whether `events` hold a press of the left or right button.
fn pressed(events: &beize_ui::Events) -> bool {
    events.input.iter().any(|i| {
        matches!(
            i,
            Input::Pointer(Pointer::Button {
                button: Button::Left | Button::Right,
                pressed: true,
            })
        )
    })
}

pub(crate) struct Shell {
    app: App,
    bar: WindowId,
    dock_panel: WindowId,
    known: HashMap<String, apps::App>,
    minutes: clock::Minutes,
    // Without it the network shows as it was at the start.
    monitor: Option<network::Monitor>,
    network: network::Status,
    look: BarLook,
    appearance: Appearance,
    backdrop: Appearance,

    // Each panel is drawn once the compositor has configured it, and the
    // dock again only in the size it asked for last. Once both are drawn,
    // the compositor shows the desktop (ADR 0028 §6).
    bar_shown: bool,
    dock_shown: bool,
    bar_drawn: bool,
    dock_drawn: bool,
    ready: bool,
    // What changed since a panel's last frame, drawn once it is ready.
    redraw_bar: bool,
    redraw_dock: bool,

    // The bar: the active window (its menus show, its app's windows in its
    // name's), the time it shows – the clock set within the minute (NTS at
    // boot) changes nothing to draw –, its buttons as last drawn and where
    // the pointer is on it, and where it last was, off the bar too: the
    // compositor repeats the place when a menu changes, and only moving
    // goes from menu to menu.
    active_name: Option<String>,
    active: Option<beize_ui::DesktopWindow>,
    shown_time: Option<String>,
    buttons: Buttons,
    on_bar: Option<(f64, f64)>,
    bar_pointer: (f64, f64),

    dock: Dock,
    dock_width: u32,

    // The open menu: which, its popup, and whether it waits to be drawn;
    // the menu of a dock tile, an app or Window while it is open, with its
    // entries' commands; the menu of a command of the open menu (Move &
    // Resize), its popup and whether it waits to be drawn.
    open: Option<(Open, WindowId)>,
    redraw_open: bool,
    beize_menu: Menu,
    control_center: ControlCenter,
    dock_menu: Option<(Menu, Vec<Option<dock::Command>>)>,
    on_menu: Option<(f64, f64)>,
    submenu: Option<(WindowId, Menu, Vec<Option<dock::Command>>)>,
    redraw_submenu: bool,

    // The dialog open, what it confirms if anything, and whether it waits
    // to be drawn.
    asking: Option<(WindowId, Dialog, Option<Command>)>,
    redraw_asking: bool,
    // Alt+Tab's window while it shows, and whether it waits to be drawn.
    switching: Option<(WindowId, Switcher)>,
    redraw_switching: bool,
    // polkit's agent and its dialog (ADR 0031).
    auth: Option<auth::Auth>,
}

impl Shell {
    pub(crate) fn new() -> Result<Shell, String> {
        let mut app = App::connect().map_err(|e| e.to_string())?;
        let bar = app
            .panel(&Panel {
                namespace: "menu-bar",
                edge: Edge::Top,
                width: 0,
                height: HEIGHT,
                margin: 0,
                translucent: true,
            })
            .map_err(|e| e.to_string())?;
        let mut known = HashMap::new();
        let dock = Dock::new(&mut |id: &str| names(&mut known, id));
        let dock_panel = app
            .panel(&Panel {
                namespace: "dock",
                edge: Edge::Bottom,
                width: dock.width(),
                height: dock::HEIGHT,
                margin: dock::MARGIN,
                translucent: true,
            })
            .map_err(|e| e.to_string())?;
        app.watch_desktop().map_err(|e| e.to_string())?;
        info!("menu bar and dock on {}", app.gpu_name());
        let minutes = clock::Minutes::new().map_err(|e| format!("the minute's timer: {e}"))?;
        let monitor = network::Monitor::new()
            .inspect_err(|e| warn!("watching the network: {e}"))
            .ok();
        let network = network::Status::now();
        info!("network: {network:?}");
        let look = BarLook::from_environment();
        info!("menu bar {look:?}");
        Ok(Shell {
            appearance: app.appearance(),
            backdrop: app.menu_bar_backdrop(),
            app,
            bar,
            dock_panel,
            known,
            minutes,
            monitor,
            network,
            look,
            bar_shown: false,
            dock_shown: false,
            bar_drawn: false,
            dock_drawn: false,
            ready: false,
            redraw_bar: false,
            redraw_dock: false,
            active_name: None,
            active: None,
            shown_time: None,
            buttons: Buttons::default(),
            on_bar: None,
            bar_pointer: (-1.0, -1.0),
            dock_width: dock.width(),
            dock,
            open: None,
            redraw_open: false,
            beize_menu: Menu::new(beize_menu_entries()),
            control_center: ControlCenter::new(),
            dock_menu: None,
            on_menu: None,
            submenu: None,
            redraw_submenu: false,
            asking: None,
            redraw_asking: false,
            switching: None,
            redraw_switching: false,
            auth: auth::Auth::new(),
        })
    }

    /// One turn: waits for the compositor or a watch, handles what came,
    /// draws what changed.
    pub(crate) fn turn(&mut self) -> Result<(), String> {
        let mut watches = vec![Watch::Read(self.minutes.fd())];
        watches.extend(self.monitor.as_ref().map(|m| Watch::Read(m.fd())));
        let (auth_fds, auth_watched) = match &self.auth {
            Some(auth) => {
                let (fds, watched) = auth.watches(watches.len());
                (fds, Some(watched))
            }
            None => (Vec::new(), None),
        };
        watches.extend(auth_fds.into_iter().map(Watch::Read));
        let mut dispatched = self
            .app
            .dispatch(None, &watches)
            .map_err(|e| e.to_string())?;
        drop(watches);
        // Apps the dock starts, once their activation tokens are made.
        for (app_id, token) in self.app.take_activation_tokens() {
            beize_systemd::apps::start(&app_id, Some(token));
        }
        let mut turn = Turn {
            bar: dispatched.take(self.bar),
            dock: dispatched.take(self.dock_panel),
            open: self.open.map(|(_, id)| dispatched.take(id)),
            submenu: self.submenu.as_ref().map(|(id, ..)| dispatched.take(*id)),
            asking: self.asking.as_ref().map(|(id, ..)| dispatched.take(*id)),
            switching: self.switching.as_ref().map(|(id, _)| dispatched.take(*id)),
            auth: self
                .auth
                .as_ref()
                .and_then(auth::Auth::window)
                .map(|id| dispatched.take(id)),
            ready: std::mem::take(&mut dispatched.ready),
            desktop: dispatched.desktop,
        };
        if turn.bar.closed || turn.dock.closed {
            // Its screen went: a failure, so the service manager starts the
            // shell again.
            return Err("the compositor closed a panel".into());
        }
        self.timers(&turn)?;
        self.look_changes();
        let active_changed = self.desktop_changed(&turn);
        self.switcher(&turn);
        let mut opening = self.open_menu(&turn, active_changed)?;
        self.dialog(&turn)?;
        // A press elsewhere in the shell closes the open menu, as macOS'
        // (the compositor's grab closes it only for other apps' surfaces).
        let was = self.open.map(|(o, _)| o);
        if pressed(&turn.bar)
            || pressed(&turn.dock)
            || turn.asking.as_ref().is_some_and(pressed)
            || turn.auth.as_ref().is_some_and(pressed)
        {
            self.close_menu();
        }
        if let (Some(agent), Some(watched)) = (&mut self.auth, &auth_watched) {
            let known = &mut self.known;
            if !agent.handle(
                &mut self.app,
                &turn.ready,
                watched,
                turn.auth.take(),
                &mut |id: &str| names(known, id),
            ) {
                self.auth = None;
            }
        }
        self.bar_input(&turn, was, &mut opening);
        // Super+R: the Beize menu, or closed if it is open.
        if self.app.take_beize_menu_key() {
            if matches!(self.open, Some((Open::BeizeMenu, _))) {
                self.close_menu();
            } else {
                opening = self
                    .buttons
                    .along_bar()
                    .into_iter()
                    .find(|(to, ..)| *to == Open::BeizeMenu);
            }
        }
        if let Some(opening) = opening {
            self.open_next(opening);
        }
        self.dock_input(&turn);
        self.draw(&turn)
    }

    /// Closes the open menu with its submenu; the bar draws its button
    /// without the pill.
    fn close_menu(&mut self) {
        let Some((which, id)) = self.open.take() else {
            return;
        };
        if let Some((sub_id, ..)) = self.submenu.take() {
            self.app.close(sub_id);
        }
        info!("{which:?} closed");
        self.app.close(id);
        self.on_menu = None;
        self.beize_menu = Menu::new(beize_menu_entries());
        self.dock_menu = None;
        self.redraw_bar = true;
    }

    /// The minute's tick and the network's changes.
    fn timers(&mut self, turn: &Turn) -> Result<(), String> {
        let tick = turn.ready.contains(&0);
        if tick {
            self.minutes
                .take()
                .map_err(|e| format!("the minute's timer: {e}"))?;
        }
        if let Some(monitor) = &self.monitor
            && turn.ready.contains(&1)
        {
            monitor.take();
            let now = network::Status::now();
            if now != self.network {
                info!("network: {now:?}");
                self.network = now;
                self.redraw_bar = true;
                self.redraw_open |= self.open.is_some_and(|(o, _)| o == Open::ControlCenter);
            }
        }
        self.redraw_bar |= turn.bar.configured || (tick && clock::now() != self.shown_time);
        self.redraw_dock |= turn.dock.configured;
        Ok(())
    }

    /// The appearance and the background behind the bar, as the compositor
    /// says them.
    fn look_changes(&mut self) {
        if self.app.appearance() != self.appearance {
            self.appearance = self.app.appearance();
            info!("appearance {:?}", self.appearance);
            self.redraw_bar = true;
            self.redraw_dock = true;
            self.redraw_open = self.open.is_some();
            if let Some(auth) = &mut self.auth {
                auth.appearance_changed();
            }
        }
        if self.app.menu_bar_backdrop() != self.backdrop {
            self.backdrop = self.app.menu_bar_backdrop();
            info!("menu bar on a {:?} background", self.backdrop);
            self.redraw_bar = true;
        }
    }

    /// The desktop's windows: the dock's tiles and the active app in the
    /// bar. Whether another window became active.
    fn desktop_changed(&mut self, turn: &Turn) -> bool {
        if !turn.desktop {
            return false;
        }
        let windows = self.app.desktop_windows();
        let before = self.dock.summary();
        let known = &mut self.known;
        self.dock.update(&windows, &mut |id: &str| names(known, id));
        if self.dock.summary() != before {
            info!("dock: {}", self.dock.summary());
            self.redraw_dock = true;
        }
        let now = windows.iter().find(|w| w.active).cloned();
        let name = now.as_ref().map(|w| names(&mut self.known, &w.app_id).name);
        if name != self.active_name {
            info!("menu bar: {}", name.as_deref().unwrap_or("(no app)"));
            self.active_name = name;
            self.redraw_bar = true;
        }
        let titles = |w: &Option<beize_ui::DesktopWindow>| {
            w.as_ref()
                .map(|w| w.menus.iter().map(|m| m.title.clone()).collect::<Vec<_>>())
        };
        self.redraw_bar |= titles(&now) != titles(&self.active);
        let changed = now.as_ref().map(|w| w.id) != self.active.as_ref().map(|w| w.id);
        self.active = now;
        changed
    }

    /// Alt+Tab: its window follows what the compositor says; the pointer
    /// selects and chooses in it.
    fn switcher(&mut self, turn: &Turn) {
        if turn.desktop {
            match (self.app.desktop_switcher(), &mut self.switching) {
                (None, Some((id, _))) => {
                    self.app.close(*id);
                    self.switching = None;
                }
                (Some(news), None) => {
                    let mut switcher = Switcher::new(news.room);
                    switcher.update(news.entries, news.selected);
                    let overlay = Overlay {
                        namespace: "switcher",
                        size: switcher.size(),
                        keyboard: false,
                    };
                    match self.app.overlay(&overlay) {
                        Ok(id) => {
                            self.switching = Some((id, switcher));
                            self.redraw_switching = false;
                        }
                        Err(e) => warn!("the switcher: {e}"),
                    }
                }
                (Some(news), Some((id, switcher))) => {
                    let before = switcher.size();
                    self.redraw_switching |= switcher.update(news.entries, news.selected);
                    if switcher.size() != before {
                        // Drawn once the compositor configures the new size.
                        self.app.set_panel_size(*id, switcher.size());
                        self.redraw_switching = false;
                    }
                }
                (None, None) => {}
            }
        }
        if let (Some(events), Some((_, switcher))) = (&turn.switching, &mut self.switching) {
            self.redraw_switching |= events.configured;
            for input in &events.input {
                match input {
                    // Moved, not only there: a pointer lying where the window
                    // opens selects nothing.
                    Input::Pointer(Pointer::Motion(x, y)) => {
                        if let Some(window) = switcher.pointer(Some((*x, *y))) {
                            self.app.switcher_hover(window);
                        }
                    }
                    Input::Pointer(Pointer::Enter(x, y)) => {
                        switcher.pointer(Some((*x, *y)));
                    }
                    Input::Pointer(Pointer::Leave) => {
                        switcher.pointer(None);
                    }
                    Input::Pointer(Pointer::Button {
                        button: Button::Left,
                        pressed: false,
                    }) => {
                        if let Some(window) = switcher.under() {
                            self.app.switcher_choose(window);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// The open menu and its submenu: what happened to them, and the
    /// command chosen once it closes. The menu beside it, if the arrows
    /// asked for it.
    fn open_menu(&mut self, turn: &Turn, active_changed: bool) -> Result<Opening, String> {
        let (Some((which, id)), Some(events)) = (self.open, &turn.open) else {
            return Ok(None);
        };
        self.redraw_open |= events.configured;
        let mut close = events.closed;
        let mut chosen = None;
        let mut tile_command = None;
        let mut neighbour = None;
        let width = self.app.size(id).0;
        // The app's and Window's menus are of the window active when they
        // opened, and their place in the bar goes with it.
        close |=
            active_changed && matches!(which, Open::AppName | Open::AppMenu(_) | Open::WindowMenu);
        // Its submenu first: a command there ends both.
        if let (Some(events), Some((sub, menu, commands))) = (&turn.submenu, &mut self.submenu) {
            self.redraw_submenu |= events.configured;
            close |= events.closed;
            let sub_width = self.app.size(*sub).0;
            for input in &events.input {
                match menu.input(input, sub_width) {
                    Outcome::None => {}
                    Outcome::Changed => self.redraw_submenu = true,
                    Outcome::Dismissed => close = true,
                    // Left: back to the menu; Right: the next along the bar.
                    Outcome::Neighbour(by) => {
                        if by > 0 {
                            neighbour = Some(by);
                        }
                        if let Some((parent, _)) = &mut self.dock_menu {
                            parent.hold(None);
                        }
                    }
                    Outcome::Chosen(i) => {
                        close = true;
                        tile_command = commands.get(i).cloned().flatten();
                    }
                }
            }
        }
        for input in &events.input {
            if let Input::Pointer(Pointer::Enter(x, y) | Pointer::Motion(x, y)) = input {
                self.on_menu = Some((*x, *y));
            }
            match which {
                Open::BeizeMenu => match self.beize_menu.input(input, width) {
                    Outcome::None => {}
                    Outcome::Neighbour(by) => neighbour = Some(by),
                    Outcome::Changed => self.redraw_open = true,
                    Outcome::Dismissed => close = true,
                    Outcome::Chosen(i) => {
                        close = true;
                        chosen = command_at(i);
                    }
                },
                Open::DockMenu | Open::AppName | Open::AppMenu(_) | Open::WindowMenu => {
                    if let Some((menu, commands)) = &mut self.dock_menu {
                        match menu.input(input, width) {
                            Outcome::None => {}
                            Outcome::Neighbour(by) => {
                                if which.along_bar() {
                                    neighbour = Some(by);
                                }
                            }
                            Outcome::Changed => self.redraw_open = true,
                            Outcome::Dismissed => close = true,
                            Outcome::Chosen(i) => {
                                close = true;
                                tile_command = commands.get(i).cloned().flatten();
                            }
                        }
                    }
                }
                Open::ControlCenter => {
                    if let Input::Pointer(Pointer::Button {
                        button: Button::Left,
                        pressed: true,
                    }) = input
                        && let Some(at) = self.on_menu
                        && let Some(control_center::Action::Appearance(chosen)) =
                            self.control_center.click(at, self.appearance)
                    {
                        info!("control center: appearance {chosen:?}");
                        self.app.choose_appearance(chosen);
                    }
                    // Pressed, as the Beize menu's (`Menu::input`): the
                    // release of an Escape that closed that menu comes
                    // here when this opened while it was held.
                    if let Input::Key(key) = input
                        && key.sym == beize_ui::Keysym::Escape
                        && key.state != KeyState::Released
                    {
                        close = true;
                    }
                }
            }
        }
        // The command with a menu: it opens beside its row, or closes when
        // the pointer goes to another row.
        if let Some((menu, _)) = &mut self.dock_menu {
            match (menu.opening(), &self.submenu) {
                (Some(row), None) if !close => {
                    let window = self.active.as_ref().map(|w| w.id);
                    let anchor = menu.row(row, width);
                    if let (Some(window), Some(anchor)) = (window, anchor) {
                        let (entries, commands) = move_and_resize_menu(window);
                        let sub = Menu::new(entries);
                        let popup = Popup {
                            anchor,
                            size: sub.size(self.app.text()),
                            align: Align::Beside,
                            above: false,
                        };
                        match self.app.popup(id, &popup) {
                            Ok(sub_id) => {
                                info!("submenu open");
                                menu.hold(Some(row));
                                self.submenu = Some((sub_id, sub, commands));
                                self.redraw_submenu = false;
                            }
                            Err(e) => {
                                menu.hold(None);
                                warn!("opening a submenu: {e}");
                            }
                        }
                    }
                }
                (None, Some((sub_id, ..))) => {
                    info!("submenu closed");
                    self.app.close(*sub_id);
                    self.submenu = None;
                    menu.hold(None);
                }
                _ => {}
            }
        }
        // The arrows: the menu beside it in the bar.
        let mut opening = None;
        if let Some(by) = neighbour {
            let order = self.buttons.along_bar();
            if let Some(i) = order.iter().position(|(o, ..)| *o == which) {
                let n = order.len() as isize;
                opening = Some(order[(i as isize + by).rem_euclid(n) as usize]);
            }
        }
        if close {
            self.close_menu();
            self.act(tile_command, chosen)?;
        }
        Ok(opening)
    }

    /// What a menu that closed chose: a dock or app command, or one of the
    /// Beize menu's (asked first, but Sleep).
    fn act(
        &mut self,
        tile_command: Option<dock::Command>,
        chosen: Option<Command>,
    ) -> Result<(), String> {
        let app = &mut self.app;
        let mut about = None;
        match tile_command {
            Some(dock::Command::Activate(window)) => app.activate_desktop_window(window),
            Some(dock::Command::Show(windows)) => {
                for window in windows {
                    app.activate_desktop_window(window);
                }
            }
            Some(dock::Command::Minimize(windows)) => {
                for window in windows {
                    app.minimize_desktop_window(window);
                }
            }
            Some(dock::Command::Zoom(window)) => app.zoom_desktop_window(window),
            Some(dock::Command::MoveTo(window, place)) => {
                app.move_desktop_window(window, place);
            }
            Some(dock::Command::About(name)) => about = Some(name),
            Some(dock::Command::App(window, command)) => {
                app.activate_menu(window, command);
            }
            Some(dock::Command::Start(app_id)) => start(app, self.dock_panel, &app_id),
            Some(dock::Command::Quit(windows)) => {
                for window in windows {
                    app.close_desktop_window(window);
                }
            }
            None => {}
        }
        // Gone from the screen before logind acts – it may take a while, and
        // sleep would keep the menu to the wake.
        app.flush().map_err(|e| e.to_string())?;
        match chosen {
            Some(Command::Power(Power::Sleep)) => choose(chosen),
            Some(command) => {
                let dialog = confirmation(command);
                info!("asking: {command:?}");
                if show_dialog(app, &mut self.asking, dialog, Some(command)) {
                    self.redraw_asking = false;
                }
            }
            None => {}
        }
        if let Some(name) = about {
            let dialog = Dialog::info(name, system_name());
            info!("about");
            if show_dialog(app, &mut self.asking, dialog, None) {
                self.redraw_asking = false;
            }
        }
        Ok(())
    }

    /// The confirmation or About: its answer.
    fn dialog(&mut self, turn: &Turn) -> Result<(), String> {
        let (Some(events), Some((id, dialog, command))) = (&turn.asking, &mut self.asking) else {
            return Ok(());
        };
        self.redraw_asking |= events.configured;
        let mut answer = events.closed.then_some(false);
        let size = self.app.size(*id);
        for input in &events.input {
            match dialog.input(input, size) {
                // No field in these.
                dialog::Outcome::None | dialog::Outcome::Paste => {}
                dialog::Outcome::Changed => self.redraw_asking = true,
                dialog::Outcome::Confirmed => answer = Some(true),
                dialog::Outcome::Cancelled => answer = Some(false),
            }
        }
        if let Some(answer) = answer {
            let (id, command) = (*id, *command);
            match command {
                Some(command) => {
                    info!("asked: {command:?}, {}", if answer { "yes" } else { "no" });
                }
                None => info!("about closed"),
            }
            self.app.close(id);
            self.asking = None;
            self.app.flush().map_err(|e| e.to_string())?;
            if answer {
                choose(command);
            }
        }
        Ok(())
    }

    /// The bar's buttons: a press opens their menu, or closes it; with a
    /// menu open along the bar, moving onto another's button goes there.
    /// `was` is the menu open before this turn's presses closed it.
    fn bar_input(&mut self, turn: &Turn, was: Option<Open>, opening: &mut Opening) {
        for input in &turn.bar.input {
            let Input::Pointer(pointer) = input else {
                continue;
            };
            match *pointer {
                Pointer::Enter(x, y) => {
                    self.on_bar = Some((x, y));
                    self.bar_pointer = (x, y);
                }
                Pointer::Motion(x, y) => {
                    self.on_bar = Some((x, y));
                    let moved = self.bar_pointer != (x, y);
                    self.bar_pointer = (x, y);
                    // A menu open along the bar: moved onto another's button
                    // goes there (macOS).
                    if let (Some((now, _)), Some((to, anchor, align))) =
                        (self.open, self.buttons.at((x, y)))
                        && moved
                        && now.along_bar()
                        && to.along_bar()
                        && to != now
                    {
                        *opening = Some((to, anchor, align));
                    }
                }
                Pointer::Leave => self.on_bar = None,
                Pointer::Button {
                    button: Button::Left,
                    pressed: true,
                } => {
                    let Some(at) = self.on_bar else {
                        continue;
                    };
                    let Some((pressed, anchor, align)) = self.buttons.at(at) else {
                        continue;
                    };
                    // Its own button again: closed above, it stays closed.
                    if was == Some(pressed) {
                        continue;
                    }
                    *opening = Some((pressed, anchor, align));
                }
                _ => {}
            }
        }
    }

    /// Opens `to` below its button, after the one open is gone: the
    /// compositor refuses a new grab over a popup still there.
    fn open_next(&mut self, (to, anchor, align): (Open, Rect, Align)) {
        self.close_menu();
        let known = &mut self.known;
        let commands = match to {
            Open::AppName => self.active.as_ref().map(|window| {
                app_name_menu(window, &self.app.desktop_windows(), &mut |id: &str| {
                    names(known, id)
                })
            }),
            Open::AppMenu(i) => self
                .active
                .as_ref()
                .and_then(|window| Some((window.id, window.menus.get(i)?)))
                .map(|(window, menu)| app_menu_commands(window, menu)),
            Open::WindowMenu => self
                .active
                .as_ref()
                .map(|window| window_menu(window, &self.dock)),
            _ => None,
        };
        let size = match (to, &commands) {
            (Open::ControlCenter, _) => Some(self.control_center.size()),
            (Open::BeizeMenu, _) => Some(self.beize_menu.size(self.app.text())),
            (_, Some((entries, _))) => Some(Menu::new(entries.clone()).size(self.app.text())),
            // The bar still drawn for a window no longer active: nothing to
            // show, so no popup that would hold the keyboard.
            _ => None,
        };
        let Some(size) = size else {
            return;
        };
        let popup = Popup {
            anchor,
            size,
            align,
            above: false,
        };
        match self.app.popup(self.bar, &popup) {
            Ok(id) => {
                info!("{to:?} open");
                if to == Open::ControlCenter {
                    self.app.set_input(id, Some(&self.control_center.input()));
                    // Out of its button (ADR 0032 §5).
                    self.app.mark_grows(id);
                }
                self.dock_menu = commands.map(|(entries, list)| (Menu::new(entries), list));
                self.open = Some((to, id));
                self.redraw_open = false;
                self.redraw_bar = true;
            }
            Err(e) => warn!("opening {to:?}: {e}"),
        }
    }

    /// The dock: hover, a click that starts or brings forward, a tile's
    /// menu above it.
    fn dock_input(&mut self, turn: &Turn) {
        for input in &turn.dock.input {
            let Input::Pointer(pointer) = input else {
                continue;
            };
            match *pointer {
                Pointer::Enter(x, y) | Pointer::Motion(x, y) => self.dock.hover(Some((x, y))),
                Pointer::Leave => self.dock.hover(None),
                Pointer::Button {
                    button: Button::Left,
                    pressed: true,
                } => self.redraw_dock |= self.dock.press(),
                Pointer::Button {
                    button: Button::Right,
                    pressed: true,
                } => {
                    let Some(tile) = self.dock.menu() else {
                        continue;
                    };
                    let menu = Menu::new(tile.entries);
                    // Above the dock, on its top edge – as the bar's menus
                    // hang from the bar –, centred on the tile.
                    let popup = Popup {
                        anchor: Rect {
                            y: 0.0,
                            h: 1.0,
                            ..tile.tile
                        },
                        size: menu.size(self.app.text()),
                        align: Align::Centre,
                        above: true,
                    };
                    match self.app.popup(self.dock_panel, &popup) {
                        Ok(id) => {
                            info!("{:?} open", Open::DockMenu);
                            self.dock_menu = Some((menu, tile.commands));
                            self.open = Some((Open::DockMenu, id));
                            self.redraw_open = false;
                        }
                        Err(e) => warn!("opening a tile's menu: {e}"),
                    }
                }
                Pointer::Button {
                    button: Button::Left,
                    pressed: false,
                } => {
                    self.redraw_dock = true;
                    match self.dock.release() {
                        Some(Action::Start(app_id)) => {
                            start(&mut self.app, self.dock_panel, &app_id);
                        }
                        Some(Action::Activate(windows)) => {
                            for window in windows {
                                self.app.activate_desktop_window(window);
                            }
                        }
                        None => {}
                    }
                }
                _ => {}
            }
        }
        if self.dock.width() != self.dock_width {
            // Drawn once the compositor configures the new width.
            self.dock_width = self.dock.width();
            self.app
                .set_panel_size(self.dock_panel, (self.dock_width, dock::HEIGHT));
            self.redraw_dock = false;
        }
    }

    /// Draws what changed, each window once it is ready; tells the
    /// compositor once the bar and the dock are drawn.
    fn draw(&mut self, turn: &Turn) -> Result<(), String> {
        self.bar_shown |= turn.bar.configured;
        if self.bar_shown && self.redraw_bar && self.app.ready(self.bar) {
            self.draw_bar()?;
        }
        if let Some((which, id)) = self.open
            && self.redraw_open
            && self.app.ready(id)
        {
            self.redraw_open = false;
            let size = self.app.size(id);
            let appearance = self.appearance;
            let drawn = match which {
                Open::BeizeMenu => Some((
                    Menu::glass(size),
                    self.beize_menu.draw(self.app.text(), size, appearance),
                )),
                // Opened only with its entries (`open_next`).
                Open::DockMenu | Open::AppName | Open::AppMenu(_) | Open::WindowMenu => {
                    self.dock_menu.as_ref().map(|(menu, _)| {
                        (
                            Menu::glass(size),
                            menu.draw(self.app.text(), size, appearance),
                        )
                    })
                }
                Open::ControlCenter => Some((
                    self.control_center.glass(),
                    self.control_center
                        .draw(self.app.text(), self.network, appearance),
                )),
            };
            if let Some((glass, scene)) = drawn {
                self.app.set_glass(id, Some(glass));
                self.app.draw(id, &scene).map_err(|e| e.to_string())?;
            }
        }
        if let Some((id, menu, _)) = &self.submenu
            && self.redraw_submenu
            && self.app.ready(*id)
        {
            self.redraw_submenu = false;
            let id = *id;
            let size = self.app.size(id);
            self.app.set_glass(id, Some(Menu::glass(size)));
            let scene = menu.draw(self.app.text(), size, self.appearance);
            self.app.draw(id, &scene).map_err(|e| e.to_string())?;
        }
        if let Some((id, dialog, _)) = &self.asking
            && self.redraw_asking
            && self.app.ready(*id)
        {
            self.redraw_asking = false;
            let id = *id;
            let size = self.app.size(id);
            self.app.set_glass(id, Some(Dialog::glass(size)));
            let scene = dialog.draw(self.app.text(), size, self.appearance);
            self.app.draw(id, &scene).map_err(|e| e.to_string())?;
        }
        if let Some(auth) = &mut self.auth {
            auth.draw(&mut self.app, self.appearance)?;
        }
        self.draw_switcher()?;
        self.dock_shown |= turn.dock.configured;
        if self.dock_shown && self.redraw_dock && self.app.ready(self.dock_panel) {
            self.redraw_dock = false;
            let size = self.app.size(self.dock_panel);
            self.app.set_glass(self.dock_panel, Some(Dock::glass(size)));
            self.app
                .show_dock_icons(self.dock_panel, &self.dock.icons());
            let scene = self.dock.draw(self.app.text(), size, self.appearance);
            self.app
                .draw(self.dock_panel, &scene)
                .map_err(|e| e.to_string())?;
            self.dock_drawn = true;
        }
        if self.bar_drawn && self.dock_drawn && !self.ready {
            self.app.desktop_ready();
            self.ready = true;
            info!("ready: menu bar and dock drawn");
            // Once, for the journal, after the desktop shows: what logind
            // lets the Beize menu do.
            on_the_side(|| info!("Beize menu may: {}", may()));
        }
        Ok(())
    }

    fn draw_bar(&mut self) -> Result<(), String> {
        self.redraw_bar = false;
        let size = self.app.size(self.bar);
        // On glass the ink follows the appearance, as the dock's.
        let behind = match self.look {
            BarLook::Transparent => self.backdrop,
            BarLook::Glass => {
                self.app.set_glass(self.bar, Some(bar_glass(size)));
                self.appearance
            }
        };
        self.shown_time = clock::now();
        let menus: Vec<String> = self
            .active
            .as_ref()
            .map(|w| w.menus.iter().map(|m| m.title.clone()).collect())
            .unwrap_or_default();
        let (scene, drawn) = menu_bar(
            self.app.text(),
            size,
            &BarContent {
                app: self.active_name.as_deref(),
                menus: &menus,
                time: self.shown_time.clone(),
                network: self.network,
                open: self.open.map(|(o, _)| o),
            },
            behind,
        );
        if drawn.centres() != self.buttons.centres() {
            // Where to click, for tests (the clock's width moves the control
            // center's).
            let [beize, centre] = drawn.centres();
            info!("menu bar buttons: Beize menu at {beize:?}, control center at {centre:?}");
        }
        if drawn.along != self.buttons.along {
            // Where to click, for tests.
            let places: Vec<String> = drawn
                .along
                .iter()
                .map(|(_, r, _)| {
                    format!("{},{}", (r.x + r.w / 2.0) as i32, (r.y + r.h / 2.0) as i32)
                })
                .collect();
            info!("menu bar menus: {}", places.join(" "));
        }
        self.buttons = drawn;
        self.app.draw(self.bar, &scene).map_err(|e| e.to_string())?;
        self.bar_drawn = true;
        Ok(())
    }

    fn draw_switcher(&mut self) -> Result<(), String> {
        let Some((id, switcher)) = &mut self.switching else {
            return Ok(());
        };
        if !self.redraw_switching || !self.app.ready(*id) {
            return Ok(());
        }
        let id = *id;
        let size = self.app.size(id);
        self.app.set_glass(id, Some(Switcher::glass(size)));
        self.app.show_thumbnails(id, &switcher.pictures());
        let windows = self.app.desktop_windows();
        let known = &mut self.known;
        let scene = switcher.draw(
            self.app.text(),
            size,
            self.appearance,
            |wanted| windows.iter().find(|w| w.id == wanted).cloned(),
            &mut |id: &str| names(known, id),
        );
        // Once at rest, not each frame of the ring's move.
        if !switcher.moving() {
            info!(
                "switcher: {}x{} with {} windows, the first picture at {:?}",
                size.0,
                size.1,
                switcher.pictures().len(),
                switcher
                    .pictures()
                    .first()
                    .map(|(_, r)| (r.x, r.y, r.w, r.h))
            );
        }
        self.app.draw(id, &scene).map_err(|e| e.to_string())?;
        // Its ring moves: again once it is ready.
        self.redraw_switching = switcher.moving();
        Ok(())
    }
}
