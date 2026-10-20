// SPDX-License-Identifier: Apache-2.0
//! Software compositor and Golden Gate-inspired desktop shell.
//!
//! The compositor paints an original, clean-room desktop directly into the GOP
//! framebuffer. It uses layered translucency, specular highlights, tighter
//! window corners, edge-to-edge sidebars, a floating dock, Spaces, and a
//! Spotlight-style search. Those are design principles, not Apple code or
//! assets: every glyph, icon, palette, and animation is defined in this tree.
//!
//! The pure window-manager model has no authority. This adapter still runs in
//! ring 0 and is part of the trusted computing base; its service-only access is
//! a coding rule, not containment of a compromised compositor. Ring-3 migration
//! is required to enforce the specification's desktop security boundary.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::graphics;
use crate::icons;
use crate::input::{self, Key};
use crate::serial_line;
use crate::{fs, packaging, portal, recovery, scheduler, supervisor, syscall};
use aether_packaging::manifest::Version;
use aether_wm::browser::{Browser, BrowserLayout, LOCATIONS};
use aether_wm::layout::{self, DOCK_SLOTS, DockSlot};
use aether_wm::{
    Command, GlassLevel, Key as WmKey, MenuKind, Modifiers, Rect, Settings, WORKSPACES, Window,
    WindowKind, Wm,
};
use core::cell::UnsafeCell;

struct Global<T>(UnsafeCell<T>);

// SAFETY: The compositor is only mutated by the idle task on the single CPU.
// Hardware IRQs enqueue input but never touch desktop or window-manager state.
unsafe impl<T> Sync for Global<T> {}

struct DesktopState {
    wm: Option<Wm>,
    last_ticks: u64,
    last_second: u64,
    last_buttons: u8,
    last_paint: u64,
    painted: Option<Wm>,
    wallpaper_high_contrast: Option<bool>,
    browser: Browser,
    files_dirty: bool,
    live_dirty: bool,
    scene_paints: u64,
    cursor_paints: u64,
    last_paint_ticks: u64,
    max_paint_ticks: u64,
    rename: RenameState,
}

#[derive(Clone, Copy)]
struct RenameState {
    active: bool,
    buffer: [u8; 16],
    len: usize,
}

impl RenameState {
    const fn new() -> Self {
        Self {
            active: false,
            buffer: [0; 16],
            len: 0,
        }
    }
}

impl DesktopState {
    const fn new() -> Self {
        Self {
            wm: None,
            last_ticks: 0,
            last_second: 0,
            last_buttons: 0,
            last_paint: 0,
            painted: None,
            wallpaper_high_contrast: None,
            browser: Browser::new(),
            files_dirty: true,
            live_dirty: true,
            scene_paints: 0,
            cursor_paints: 0,
            last_paint_ticks: 0,
            max_paint_ticks: 0,
            rename: RenameState::new(),
        }
    }
}

static DESKTOP: Global<DesktopState> = Global(UnsafeCell::new(DesktopState::new()));

/// Initializes the desktop and paints the first frame.
pub fn init() {
    if !graphics::is_ready() {
        serial_line!("AETHER_GRAPHICS_SKIP reason=no-framebuffer");
        return;
    }
    serial_line!(
        "AETHER_GRAPHICS_OK mode={}x{} format=linear",
        graphics::width(),
        graphics::height()
    );
    let screen = Rect::new(0, 0, graphics::width() as i32, graphics::height() as i32);
    let mut wm = Wm::new(screen);
    wm.seed();
    let now = scheduler::ticks();
    let mouse = input::mouse();
    let mut browser = Browser::new();
    browser.refresh(&mut fs::BrowserFiles);
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked and boot is single-threaded.
    unsafe {
        *DESKTOP.0.get() = DesktopState {
            wm: Some(wm),
            last_ticks: now,
            last_second: now / 100,
            last_buttons: mouse.buttons,
            last_paint: now,
            browser,
            ..DesktopState::new()
        };
    }
    drop(_guard);
    redraw();
    serial_line!(
        "AETHER_DESKTOP_OK windows={} spaces={} checksum={:#010x}",
        wm.window_count(),
        WORKSPACES,
        graphics::checksum()
    );
}

/// Returns a copy of the current window-manager state.
fn current() -> Option<Wm> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the snapshot; `Wm` is `Copy`.
    unsafe { (*DESKTOP.0.get()).wm }
}

fn store(wm: Wm, ticks: u64, second: u64, buttons: u8) {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the state update.
    unsafe {
        (*DESKTOP.0.get()).wm = Some(wm);
        (*DESKTOP.0.get()).last_ticks = ticks;
        (*DESKTOP.0.get()).last_second = second;
        (*DESKTOP.0.get()).last_buttons = buttons;
    }
}

fn with_browser<R>(f: impl FnOnce(&mut Browser) -> R) -> R {
    let _guard = InterruptGuard::enter();
    // SAFETY: Short, interrupt-serialized access from the idle/boot context.
    unsafe { f(&mut (*DESKTOP.0.get()).browser) }
}

fn mark_files_dirty() {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupt-serialized update; IRQs never enter the desktop.
    unsafe {
        (*DESKTOP.0.get()).files_dirty = true;
    }
}

/// Marks the live diagnostic/Linux-surface regions for the next frame.
pub fn mark_live_dirty() {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupt-serialized update; IRQs never enter the desktop.
    unsafe {
        (*DESKTOP.0.get()).live_dirty = true;
    }
}

fn rename_state() -> RenameState {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the snapshot.
    unsafe { (*DESKTOP.0.get()).rename }
}

fn begin_rename() -> bool {
    let Some(name) = with_browser(|browser| {
        browser.selected_name().map(|name| {
            let mut buffer = [0u8; 16];
            let length = name.len().min(buffer.len());
            buffer[..length].copy_from_slice(&name.as_bytes()[..length]);
            (buffer, length)
        })
    }) else {
        return false;
    };
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupt-serialized update.
    unsafe {
        let state = &mut (*DESKTOP.0.get()).rename;
        state.buffer = name.0;
        state.len = name.1;
        state.active = true;
    }
    true
}

fn cancel_rename() {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupt-serialized update.
    unsafe {
        (*DESKTOP.0.get()).rename.active = false;
    }
}

fn rename_push(byte: u8) {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupt-serialized update.
    unsafe {
        let state = &mut (*DESKTOP.0.get()).rename;
        if state.active && state.len < state.buffer.len() {
            state.buffer[state.len] = byte;
            state.len += 1;
        }
    }
}

fn rename_backspace() {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupt-serialized update.
    unsafe {
        let state = &mut (*DESKTOP.0.get()).rename;
        state.len = state.len.saturating_sub(1);
    }
}

fn rename_name() -> Option<([u8; 16], usize)> {
    let state = rename_state();
    if !state.active || state.len == 0 {
        return None;
    }
    let mut name = [0u8; 16];
    name[..state.len].copy_from_slice(&state.buffer[..state.len]);
    Some((name, state.len))
}

/// Repaints the whole desktop.
pub fn redraw() {
    if !graphics::is_ready() {
        return;
    }
    let Some(wm) = current() else {
        return;
    };
    paint_frame(&wm, wm.screen());
}

fn paint_frame(wm: &Wm, mut damage: Rect) {
    let started = scheduler::ticks();
    let palette = wm.palette();
    let glass = effective_glass(wm.settings());
    let screen = wm.screen();
    graphics::clear_clip();
    graphics::clear_damage();
    let old_cursor = graphics::restore_cursor_background();
    // SAFETY: The idle task is the only painter; no mutable reference escapes.
    let theme = unsafe { (*DESKTOP.0.get()).wallpaper_high_contrast };
    if theme != Some(wm.settings().high_contrast)
        || !graphics::restore_wallpaper(damage.x, damage.y, damage.width, damage.height)
    {
        damage = screen;
        paint_wallpaper(screen, palette, wm.settings().high_contrast);
        graphics::cache_wallpaper();
    }
    graphics::set_damage(damage.x, damage.y, damage.width, damage.height);
    paint_windows(wm, palette);
    if wm.overview_open() || wm.overview_progress() > 0 {
        paint_overview(wm, palette);
    }
    paint_dock(wm, palette, glass);
    paint_menu_bar(wm, palette, glass);
    if wm.menu_state().is_some() {
        paint_menu(wm, palette, glass);
    }
    if wm.spotlight_open() {
        paint_spotlight(wm, palette, glass);
    }
    graphics::clear_damage();
    graphics::save_cursor_background(wm.cursor().x, wm.cursor().y);
    paint_cursor(wm.cursor(), palette);
    if damage == screen {
        graphics::present();
    } else {
        graphics::present_region(damage.x, damage.y, damage.width, damage.height);
        if let Some((x, y, width, height)) = old_cursor {
            graphics::present_region(x, y, width, height);
        }
        graphics::present_region(
            wm.cursor().x,
            wm.cursor().y,
            graphics::CURSOR_WIDTH,
            graphics::CURSOR_HEIGHT,
        );
    }
    let finished = scheduler::ticks();
    let _guard = InterruptGuard::enter();
    // SAFETY: No rendering or service borrow is live during this state update.
    unsafe {
        let state = &mut *DESKTOP.0.get();
        state.painted = Some(*wm);
        state.wallpaper_high_contrast = Some(wm.settings().high_contrast);
        state.files_dirty = false;
        state.live_dirty = false;
        // Pace frame starts, not frame completions: painting already consumes
        // the frame budget and must not be followed by an extra 30 ms delay.
        state.last_paint = started;
        state.scene_paints = state.scene_paints.saturating_add(1);
        state.last_paint_ticks = finished.saturating_sub(started);
        state.max_paint_ticks = state.max_paint_ticks.max(state.last_paint_ticks);
    }
}

fn move_cursor_only(wm: &Wm) {
    let Some((x, y, width, height)) = graphics::restore_cursor_background() else {
        redraw();
        return;
    };
    graphics::save_cursor_background(wm.cursor().x, wm.cursor().y);
    paint_cursor(wm.cursor(), wm.palette());
    graphics::present_region(x, y, width, height);
    graphics::present_region(
        wm.cursor().x,
        wm.cursor().y,
        graphics::CURSOR_WIDTH,
        graphics::CURSOR_HEIGHT,
    );
    let _guard = InterruptGuard::enter();
    // SAFETY: Short update from the only desktop owner.
    unsafe {
        (*DESKTOP.0.get()).cursor_paints = (*DESKTOP.0.get()).cursor_paints.saturating_add(1);
        // Keep the last painted scene state, not unpresented animation state.
        (*DESKTOP.0.get()).painted = Some(*wm);
    }
}

/// Advances animations and routes pointer state into the window manager.
///
/// The idle loop calls this after every interrupt. Work is proportional to
/// actual change: a static desktop with no animation is not repainted.
pub fn tick() {
    let (wm, painted, last_ticks, last_second, mut last_buttons, last_paint) = {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked for the snapshot.
        let state = unsafe { &*DESKTOP.0.get() };
        (
            state.wm,
            state.painted,
            state.last_ticks,
            state.last_second,
            state.last_buttons,
            state.last_paint,
        )
    };
    let Some(mut wm) = wm else {
        return;
    };
    let now = scheduler::ticks();
    let elapsed = now.saturating_sub(last_ticks).min(u64::from(u16::MAX)) as u16;
    wm.tick(elapsed);
    let (events, mouse) = input::drain_mouse_events();
    for event in events.into_iter().flatten().chain(core::iter::once(mouse)) {
        route_pointer(&mut wm, &mut last_buttons, event, now);
    }
    // Feed the Linux endpoint only while its window owns focus. Coordinates
    // are translated to the window's client area here, not by the guest.
    let linux = wm
        .focused_window()
        .filter(|window| window.kind == WindowKind::Linux)
        .map(|window| window.id);
    let content = linux
        .and_then(|id| wm.window_frame(id))
        .map(layout::content_rect);
    let cursor = wm.cursor();
    crate::display::sync_input(linux, content, cursor.x, cursor.y, mouse.buttons);

    let second = now / 100;
    let clock_changed = second != last_second;
    if clock_changed {
        // SAFETY: Only the idle task writes desktop state.
        unsafe {
            (*DESKTOP.0.get()).live_dirty = true;
        }
    }
    store(wm, now, second, last_buttons);
    let mut damage = painted.map_or(Some(wm.screen()), |before| wm.damage_since(&before));
    let mut include = |rect: Rect| {
        damage = Some(damage.map_or(rect, |old| old.union(rect)));
    };
    // SAFETY: Only the idle task writes desktop state; IRQs only enqueue input.
    let (files_dirty, live_dirty) = unsafe {
        (
            (*DESKTOP.0.get()).files_dirty,
            (*DESKTOP.0.get()).live_dirty,
        )
    };
    if files_dirty
        && let Some(id) = wm.window_id(WindowKind::Files)
        && let Some(frame) = wm.window_frame(id)
    {
        include(frame);
    }
    if live_dirty {
        include(layout::menu_bar(wm.screen()));
        for kind in [
            WindowKind::Security,
            WindowKind::Portal,
            WindowKind::Log,
            WindowKind::Linux,
        ] {
            if let Some(id) = wm.window_id(kind)
                && let Some(frame) = wm.window_frame(id)
            {
                include(frame);
            }
        }
    }
    // Batch all queued keys and pointer events into at most one scene frame per
    // three ticks. A fast click's press/release is still processed in order.
    if let Some(damage) = damage.and_then(|rect| rect.intersection(wm.screen())) {
        if now.saturating_sub(last_paint) >= 3 {
            paint_frame(&wm, damage);
        }
    } else if painted.is_some_and(|before| before.cursor() != wm.cursor()) {
        move_cursor_only(&wm);
    }
}

fn route_pointer(wm: &mut Wm, last_buttons: &mut u8, mouse: input::MouseSnapshot, now: u64) {
    let point = aether_wm::Point::new(mouse.x, mouse.y);
    let pressed = mouse.buttons & 1 != 0;
    let was_pressed = *last_buttons & 1 != 0;
    if pressed && !was_pressed {
        let hit = wm.hit_test(point);
        wm.pointer_down(point, now);
        if let aether_wm::Hit::Window(id) = hit
            && wm
                .window(id)
                .is_some_and(|window| window.kind == WindowKind::Files)
            && let Some(frame) = wm.window_frame(id)
        {
            with_browser(|browser| {
                browser.pointer(
                    BrowserLayout::new(layout::content_rect(frame)),
                    point,
                    now,
                    &mut fs::BrowserFiles,
                )
            });
            mark_files_dirty();
        } else if let aether_wm::Hit::Window(id) = hit
            && wm
                .window(id)
                .is_some_and(|window| window.kind == WindowKind::Portal)
            && let Some(frame) = wm.window_frame(id)
            && let Some((index, approve, deny)) =
                portal_pending_buttons(layout::content_rect(frame))
        {
            let decision = if approve.contains(point) {
                Some(true)
            } else if deny.contains(point) {
                Some(false)
            } else {
                None
            };
            if let Some(approved) = decision {
                let result = if approved {
                    portal::approve(index)
                } else {
                    portal::deny(index)
                };
                match result {
                    Ok(()) => {
                        serial_line!("desktop: portal consent index={index} approved={approved}");
                        mark_live_dirty();
                    }
                    Err(_) => serial_line!("desktop: stale portal consent index={index}"),
                }
            }
        }
    } else {
        wm.pointer_moved(point);
        if !pressed && was_pressed {
            wm.pointer_up();
        }
    }
    *last_buttons = mouse.buttons;
}

/// Handles a decoded physical key.
pub fn on_key(key: Key, modifiers: Modifiers) {
    let Some(mut wm) = current() else {
        return;
    };
    // While the Linux surface owns focus, keys go to its endpoint instead of
    // the native window manager. Overlays and menus keep native focus.
    let linux_focused = wm
        .focused_window()
        .is_some_and(|window| window.kind == WindowKind::Linux)
        && !wm.overview_open()
        && !wm.spotlight_open()
        && wm.menu_state().is_none();
    if linux_focused {
        crate::display::key(key, modifiers);
        return;
    }
    let translated = match key {
        Key::Char(byte) => WmKey::Char(byte),
        Key::Enter => WmKey::Enter,
        Key::Backspace => WmKey::Backspace,
        Key::Delete => WmKey::Delete,
        Key::Tab => WmKey::Tab,
        Key::Escape => WmKey::Escape,
        Key::Up => WmKey::Up,
        Key::Down => WmKey::Down,
        Key::Left => WmKey::Left,
        Key::Right => WmKey::Right,
        Key::Function(number) => WmKey::Function(number),
    };
    let files_active = wm
        .focused_window()
        .is_some_and(|window| window.kind == WindowKind::Files)
        && !wm.overview_open()
        && !wm.spotlight_open()
        && wm.menu_state().is_none()
        && !modifiers.command()
        && !modifiers.shift;
    let files_focused = wm
        .focused_window()
        .is_some_and(|window| window.kind == WindowKind::Files)
        && !wm.overview_open()
        && !wm.spotlight_open()
        && wm.menu_state().is_none();
    if files_focused && rename_state().active {
        match key {
            Key::Char(byte)
                if !modifiers.command()
                    && (byte.is_ascii_alphanumeric() || b"-. _".contains(&byte)) =>
            {
                if byte != b' ' {
                    rename_push(byte);
                }
                mark_files_dirty();
                return;
            }
            Key::Backspace => {
                rename_backspace();
                mark_files_dirty();
                return;
            }
            Key::Enter => {
                let renamed = rename_name().is_some_and(|(buffer, length)| {
                    let name = core::str::from_utf8(&buffer[..length]).unwrap_or("");
                    with_browser(|browser| browser.rename_selected(&mut fs::BrowserFiles, name))
                });
                cancel_rename();
                mark_files_dirty();
                serial_line!("desktop: files rename ok={renamed}");
                return;
            }
            Key::Escape => {
                cancel_rename();
                mark_files_dirty();
                return;
            }
            _ => return,
        }
    }
    if files_active && key == Key::Function(2) {
        if begin_rename() {
            mark_files_dirty();
            serial_line!("desktop: files rename prompt");
        }
        return;
    }
    if files_active && with_browser(|browser| browser.key(translated, &mut fs::BrowserFiles)) {
        mark_files_dirty();
        return;
    }
    let outcome = wm.key(translated, modifiers);
    if outcome.changed {
        let _guard = InterruptGuard::enter();
        // SAFETY: Preserve timing/button state so keyboard input cannot swallow
        // pointer edges or reset elapsed animation time. The next tick paints.
        unsafe {
            (*DESKTOP.0.get()).wm = Some(wm);
        }
    }
    if let Some(label) = outcome.announce {
        if translated == WmKey::Enter {
            serial_line!("desktop: activated window={label}");
        } else {
            serial_line!("desktop: action={label}");
        }
    }
}

/// Opens or resizes the single Linux surface window.
///
/// The frame is sized so its client area matches the requested surface within
/// the compositor's work area. The surface is scaled by the painter when the
/// frame is clamped smaller. Returns `None` when no desktop exists.
pub fn open_linux_window(width: u32, height: u32, title: &str) -> Option<u8> {
    let _ = title;
    let mut wm = current()?;
    let id = wm.open(WindowKind::Linux)?;
    let area = layout::work_area(wm.screen());
    let frame_width =
        (i32::try_from(width).ok()?.saturating_add(2)).clamp(layout::MIN_WINDOW_WIDTH, area.width);
    let frame_height = (i32::try_from(height)
        .ok()?
        .saturating_add(layout::TITLE_BAR_HEIGHT + 1))
    .clamp(layout::MIN_WINDOW_HEIGHT, area.height);
    let x = area.x + (area.width - frame_width) / 2;
    let y = area.y + (area.height - frame_height) / 2;
    wm.set_window_frame(id, Rect::new(x, y, frame_width, frame_height));
    let now = scheduler::ticks();
    store(wm, now, now / 100, input::mouse().buttons);
    mark_live_dirty();
    Some(id)
}

/// Retires the Linux window when the endpoint closes or the task exits.
pub fn close_linux_window(id: u8) {
    let Some(mut wm) = current() else {
        return;
    };
    if wm
        .window(id)
        .is_none_or(|window| window.kind != WindowKind::Linux)
    {
        return;
    }
    wm.close_window(id);
    let now = scheduler::ticks();
    store(wm, now, now / 100, input::mouse().buttons);
    mark_live_dirty();
}

/// Restores the welcome window as the focused, frontmost window.
///
/// The boot input test drives real focus and workspaces; this leaves the
/// running desktop in the same calm state a person sees on a fresh login.
pub fn restore_default_view() {
    let Some(mut wm) = current() else {
        return;
    };
    if let Some(id) = wm.window_id(WindowKind::Welcome) {
        with_browser(|browser| {
            *browser = Browser::new();
            browser.refresh(&mut fs::BrowserFiles);
        });
        wm.focus_window(id);
        wm.finish_animations();
        let now = scheduler::ticks();
        store(wm, now, now / 100, input::mouse().buttons);
        redraw();
    }
}

/// Runs a deterministic desktop self-test on a copy of the live state.
///
/// The live desktop is not disturbed. This proves the state machine can open,
/// minimize, restore, close, move between Spaces, show the overview, and filter
/// Spotlight results without touching the display or any authority.
pub fn self_test() {
    let Some(mut wm) = current() else {
        serial_line!("AETHER_WM_FAILED reason=not-initialized");
        return;
    };
    let initial = wm.window_count();
    let mut failures = 0usize;

    let mut browser = Browser::new();
    browser.refresh(&mut fs::BrowserFiles);
    let folders = browser.entries().len();
    let opened = browser.navigate("/home", &mut fs::BrowserFiles);
    browser.open_selected(&mut fs::BrowserFiles);
    let preview = browser.preview_open() && browser.preview_bytes() == b"welcome to aether\n";
    browser.parent(&mut fs::BrowserFiles);
    browser.parent(&mut fs::BrowserFiles);
    let parent = browser.path() == "/";
    let denied = !browser.navigate("/apps/other/private", &mut fs::BrowserFiles);
    if folders > 0 && opened && preview && parent && denied {
        serial_line!(
            "AETHER_FILES_OK folders={} navigation=1 preview=1 private_denied=1",
            folders
        );
    } else {
        serial_line!("AETHER_FILES_FAILED reason=navigation-or-scope");
        failures += 1;
    }

    // Interactive consent is separate from the boot-time grant self-test:
    // queue a request, prove it grants nothing, approve it, then revoke and
    // compact the record. The path is reserved for this test.
    const CONSENT_PATH: &str = "/portal/consent-self-test";
    let before_grants = portal::grant_count();
    let mut consent_ok = false;
    if let Ok(index) = portal::request(
        fs::DEMO_ROOT,
        CONSENT_PATH,
        aether_abi::user::FileRights::READ,
    ) && portal::check(
        fs::DEMO_ROOT,
        CONSENT_PATH,
        aether_abi::user::FileRights::READ,
    )
    .is_err()
        && portal::pending(index).is_some()
        && portal::approve(index).is_ok()
        && portal::check(
            fs::DEMO_ROOT,
            CONSENT_PATH,
            aether_abi::user::FileRights::READ,
        )
        .is_ok()
        && portal::grant_count() == before_grants + 1
    {
        consent_ok = true;
        if let Some(index) = (0..8)
            .find(|slot| portal::grant(*slot).is_some_and(|grant| grant.path() == CONSENT_PATH))
        {
            let _ = portal::revoke_index(index);
        }
        portal::forget_revoked();
    }
    if consent_ok && portal::grant_count() == before_grants {
        serial_line!(
            "AETHER_PORTAL_CONSENT_OK checks=5 requests={}",
            portal::request_count()
        );
    } else {
        serial_line!("AETHER_PORTAL_FAILED reason=consent");
        failures += 1;
    }

    // File mutations run against a reserved path in the ordinary Home
    // service scope. The test copies, renames, and deletes its own file, so a
    // failure cannot touch user data.
    const MUTATE_PATH: &str = "/home/mutate.txt";
    let setup = fs::write_path(MUTATE_PATH, b"mutation-test\n");
    let mut mutate_ok = setup.is_ok();
    if let Err(error) = setup {
        serial_line!("desktop: mutation setup failed path={MUTATE_PATH} error={error:?}");
    }
    if mutate_ok {
        let mut files = Browser::new();
        let navigated = files.navigate("/home", &mut fs::BrowserFiles);
        let selected = files.select_name("mutate.txt");
        let duplicated = selected && files.duplicate_selected(&mut fs::BrowserFiles);
        let copied = duplicated && files.select_name("copy1");
        let renamed = copied && files.rename_selected(&mut fs::BrowserFiles, "renamed.txt");
        let reselected = renamed && files.select_name("renamed.txt");
        let deleted_copy = reselected && files.delete_selected(&mut fs::BrowserFiles);
        let original = deleted_copy && files.select_name("mutate.txt");
        let deleted_original = original && files.delete_selected(&mut fs::BrowserFiles);
        mutate_ok = navigated
            && selected
            && duplicated
            && copied
            && renamed
            && reselected
            && deleted_copy
            && original
            && deleted_original;
        if !mutate_ok {
            serial_line!(
                "desktop: mutation steps nav={navigated} select={selected} copy={duplicated} \
                 copy_select={copied} rename={renamed} select2={reselected} delete_copy={deleted_copy} \
                 select3={original} delete={deleted_original} error={:?}",
                files.error()
            );
        }
        let _ = fs::remove_path(MUTATE_PATH);
    }
    if mutate_ok {
        serial_line!("AETHER_FILES_MUTATE_OK copy=1 rename=1 delete=1");
    } else {
        serial_line!("AETHER_FILES_FAILED reason=mutation");
        failures += 1;
    }

    let portal_id = wm.open(WindowKind::Portal);
    if portal_id.is_none() {
        failures += 1;
    }
    if let Some(id) = portal_id {
        wm.minimize_window(id);
        wm.tick(30);
        wm.restore_window(id);
        wm.tick(30);
        wm.close_window(id);
        wm.tick(30);
        if wm.window_count() != initial {
            failures += 1;
        }
    }

    wm.switch_workspace(WORKSPACES - 1);
    wm.tick(30);
    wm.switch_workspace(0);
    wm.tick(30);
    if wm.workspace() != 0 || wm.transition().active {
        failures += 1;
    }

    wm.toggle_overview();
    wm.tick(30);
    wm.toggle_overview();
    wm.tick(30);
    if wm.overview_open() {
        failures += 1;
    }

    wm.open_spotlight();
    for byte in b"settings" {
        wm.key(WmKey::Char(*byte), Modifiers::default());
    }
    let search_results = wm.search_result_count();
    if search_results == 0 || wm.search_result(0).is_none() {
        failures += 1;
    }
    wm.close_spotlight();

    let before_glass = wm.settings().glass;
    wm.toggle_setting(2);
    if wm.settings().glass == before_glass {
        failures += 1;
    }

    if failures == 0 {
        serial_line!(
            "AETHER_WM_OK windows={} spaces={} dock={} menus={} search={} glass={}",
            initial,
            WORKSPACES,
            DOCK_SLOTS.len(),
            6,
            search_results,
            wm.settings().glass.label()
        );
    } else {
        serial_line!("AETHER_WM_FAILED reason=state failures={failures}");
    }
}

/// Returns the focused window title for diagnostics.
#[must_use]
pub fn focus() -> &'static str {
    current().map_or("AETHER", |wm| wm.focus_title())
}

/// Returns the active workspace, one-based for display.
#[must_use]
pub fn workspace() -> u8 {
    current().map_or(1, |wm| wm.workspace() + 1)
}

/// Returns the number of open windows.
#[must_use]
pub fn window_count() -> usize {
    current().map_or(0, |wm| wm.window_count())
}

/// Prints live window state for diagnostics.
pub fn print_state() {
    let Some(wm) = current() else {
        serial_line!("desktop: state unavailable");
        return;
    };
    serial_line!(
        "desktop: state space={} focus={:?} mode={} order={:?} transition_active={}",
        wm.workspace(),
        wm.focus_id(),
        mode(),
        wm.order(),
        wm.transition().active
    );
    for id in wm.order().iter() {
        if let Some(window) = wm.window(*id) {
            serial_line!(
                "desktop: w{} k={:?} ws{} {:?} v{} m{} {},{},{},{}",
                window.id,
                window.kind,
                window.workspace,
                window.state,
                window.is_visible(),
                window.minimize.value(),
                window.frame.x,
                window.frame.y,
                window.frame.width,
                window.frame.height
            );
        }
    }
    let _guard = InterruptGuard::enter();
    // SAFETY: Read-only snapshot while interrupts are masked.
    let state = unsafe { &*DESKTOP.0.get() };
    serial_line!(
        "desktop: render scene={} cursor={} last_ms={} max_ms={}",
        state.scene_paints,
        state.cursor_paints,
        state.last_paint_ticks * 10,
        state.max_paint_ticks * 10
    );
    serial_line!(
        "desktop: files path={} entries={} selected={} preview={} error={:?}",
        state.browser.path(),
        state.browser.entries().len(),
        state.browser.selected(),
        state.browser.preview_open(),
        state.browser.error()
    );
}

/// Returns a short mode label for the shell.
#[must_use]
pub fn mode() -> &'static str {
    current().map_or("OFFLINE", |wm| {
        if wm.spotlight_open() {
            "SEARCH"
        } else if wm.overview_open() {
            "SPACES"
        } else if wm.menu_state().is_some() {
            "MENU"
        } else {
            "WINDOW"
        }
    })
}

fn effective_glass(settings: Settings) -> GlassLevel {
    if settings.high_contrast {
        GlassLevel::Tinted
    } else {
        settings.glass
    }
}

fn paint_wallpaper(screen: Rect, palette: aether_wm::Palette, high_contrast: bool) {
    graphics::vertical_gradient(
        screen.x,
        screen.y,
        screen.width,
        screen.height,
        palette.wallpaper_top,
        palette.wallpaper_bottom,
    );
    if high_contrast {
        return;
    }
    // An original "golden hour" composition: warm horizon light diffused
    // through cool fog. Nothing is sampled from an external wallpaper.
    graphics::blend_glow(
        screen.x + screen.width * 3 / 4,
        screen.y + screen.height / 3,
        screen.width / 4,
        palette.wallpaper_glow,
        88,
    );
    graphics::blend_glow(
        screen.x + screen.width / 5,
        screen.y + screen.height * 2 / 3,
        screen.width / 5,
        palette.wallpaper_glow_alt,
        74,
    );
    graphics::blend_glow(
        screen.x + screen.width / 2,
        screen.y + screen.height + screen.height / 12,
        screen.width / 6,
        palette.wallpaper_glow,
        52,
    );
    graphics::blend_rect(
        screen.x,
        screen.y + screen.height * 58 / 100,
        screen.width,
        screen.height * 10 / 100,
        palette.wallpaper_glow,
        10,
    );
    for band in 0..3 {
        let y = screen.y + screen.height * (52 + band * 5) / 100;
        graphics::blend_rect(screen.x, y, screen.width, 2, palette.wallpaper_glow, 10);
    }
    graphics::blend_rect(screen.x, screen.y, screen.width, 2, palette.shadow, 40);
}

fn paint_windows(wm: &Wm, palette: aether_wm::Palette) {
    let screen = wm.screen();
    for id in wm.order() {
        let Some(window) = wm.window(*id) else {
            continue;
        };
        let Some(frame) = wm.window_frame(*id) else {
            continue;
        };
        paint_window(wm, window, frame, wm.focus_id() == Some(*id), palette);
    }
    if wm.transition().active {
        let label = workspace_label(wm.workspace());
        draw_pill(
            screen.x + screen.width / 2 - 50,
            screen.y + screen.height - 110,
            100,
            24,
            palette,
            label,
            palette.accent,
        );
    }
}

fn paint_window(wm: &Wm, window: &Window, frame: Rect, focused: bool, palette: aether_wm::Palette) {
    if frame.width < 24 || frame.height < 24 {
        return;
    }
    let settings = wm.settings();
    let glass = effective_glass(settings);
    let alpha = if settings.high_contrast {
        255
    } else {
        glass.window_alpha()
    };
    let radius = layout::WINDOW_RADIUS;

    // Layered shadow: darker nearest the frame, softer further away.
    for (offset, spread, strength) in [(0, 10, 78u8), (4, 20, 34)] {
        graphics::blend_round_rect(
            frame.x - spread / 2,
            frame.y + offset,
            frame.width + spread,
            frame.height + spread / 2,
            radius + spread / 2,
            palette.shadow,
            strength,
        );
    }

    graphics::blend_round_rect(
        frame.x,
        frame.y,
        frame.width,
        frame.height,
        radius,
        if focused {
            palette.window
        } else {
            palette.window_inactive
        },
        alpha,
    );

    let title_color = if focused {
        palette.title
    } else {
        palette.title_inactive
    };
    let chrome_alpha = if settings.high_contrast {
        255
    } else {
        glass.chrome_alpha()
    };
    graphics::blend_round_rect(
        frame.x,
        frame.y,
        frame.width,
        layout::TITLE_BAR_HEIGHT,
        radius,
        title_color,
        chrome_alpha,
    );
    graphics::blend_rect(
        frame.x,
        frame.y + radius,
        frame.width,
        layout::TITLE_BAR_HEIGHT - radius,
        title_color,
        chrome_alpha,
    );
    // Specular top edge and a darkened lower toolbar edge give the window its
    // Liquid Glass depth without a blur pass.
    graphics::blend_rect(
        frame.x + radius,
        frame.y + 1,
        (frame.width - radius * 2).max(0),
        1,
        0xffffff,
        if settings.high_contrast { 40 } else { 72 },
    );
    graphics::blend_rect(
        frame.x + radius,
        frame.y + layout::TITLE_BAR_HEIGHT - 1,
        (frame.width - radius * 2).max(0),
        1,
        palette.shadow,
        55,
    );

    let content = layout::content_rect(frame);
    graphics::set_round_clip(frame.x, frame.y, frame.width, frame.height, radius);
    graphics::fill_rect(
        content.x,
        content.y,
        content.width,
        content.height,
        palette.window,
    );
    graphics::set_clip(content.x, content.y, content.width, content.height);
    paint_content(wm, window, content, focused, palette);
    graphics::clear_clip();
    graphics::clear_round_clip();

    let lights = layout::traffic_lights(frame);
    paint_traffic_lights(wm, &lights, focused, palette);
    let title = if window.kind == WindowKind::Linux {
        let requested = crate::display::title();
        if requested.is_empty() {
            window.kind.title()
        } else {
            requested
        }
    } else {
        window.kind.title()
    };
    draw_text_centered(
        frame.x + 72,
        frame.right() - 12,
        frame.y + 13,
        title,
        if focused { palette.text } else { palette.muted },
        1,
    );
    graphics::outline_round_rect(
        frame.x,
        frame.y,
        frame.width,
        frame.height,
        radius,
        if focused {
            palette.accent
        } else {
            palette.border
        },
    );
}

fn paint_traffic_lights(
    wm: &Wm,
    lights: &layout::TrafficLights,
    _focused: bool,
    palette: aether_wm::Palette,
) {
    let cursor = wm.cursor();
    for (rect, color, glyph) in [
        (lights.close, palette.close, 0u8),
        (lights.minimize, palette.minimize, 1u8),
        (lights.zoom, palette.zoom, 2u8),
    ] {
        let center = rect.center();
        graphics::fill_circle(center.x, center.y, 6, palette.shadow);
        graphics::fill_circle(center.x, center.y, 5, color);
        graphics::blend_circle(center.x - 1, center.y - 2, 2, 0xffffff, 70);
        if rect.contains(cursor) {
            let ink = palette.shadow;
            match glyph {
                0 => {
                    graphics::draw_line(
                        center.x - 2,
                        center.y - 2,
                        center.x + 2,
                        center.y + 2,
                        ink,
                    );
                    graphics::draw_line(
                        center.x + 2,
                        center.y - 2,
                        center.x - 2,
                        center.y + 2,
                        ink,
                    );
                }
                1 => {
                    graphics::fill_rect(center.x - 2, center.y, 5, 1, ink);
                }
                _ => {
                    graphics::fill_rect(center.x - 2, center.y, 5, 1, ink);
                    graphics::fill_rect(center.x, center.y - 2, 1, 5, ink);
                }
            }
        }
    }
}

fn paint_content(
    wm: &Wm,
    window: &Window,
    content: Rect,
    focused: bool,
    palette: aether_wm::Palette,
) {
    match window.kind {
        WindowKind::Files => paint_files(wm, content, palette),
        WindowKind::Security => paint_security(wm, content, palette),
        WindowKind::Portal => paint_portal(wm, content, palette),
        WindowKind::Settings => paint_settings(wm, content, palette),
        WindowKind::Recovery => paint_recovery(wm, content, palette),
        WindowKind::Log => paint_log(wm, content, palette),
        WindowKind::Welcome => paint_welcome(wm, content, focused, palette),
        WindowKind::Linux => crate::display::paint_content(content),
    }
}

fn paint_files(wm: &Wm, content: Rect, palette: aether_wm::Palette) {
    let layout = BrowserLayout::new(content);
    let browser = with_browser(|browser| {
        browser.set_visible_rows(layout.visible_rows());
        *browser
    });
    let glass = effective_glass(wm.settings());
    let sidebar = layout.sidebar;
    graphics::blend_rect(
        sidebar.x,
        sidebar.y,
        sidebar.width,
        sidebar.height,
        palette.card,
        glass.chrome_alpha(),
    );
    graphics::fill_rect(
        sidebar.right() - 1,
        sidebar.y,
        1,
        sidebar.height,
        palette.divider,
    );

    icons::app_icon(
        WindowKind::Files,
        Rect::new(sidebar.x + 12, sidebar.y + 12, 22, 22),
        palette,
    );
    draw_text(sidebar.x + 40, sidebar.y + 17, "AETHER", palette.text, 1);

    for (index, (label, path)) in LOCATIONS.iter().enumerate() {
        let rect = layout.sidebar_row(index);
        if rect.bottom() > sidebar.bottom() - 24 {
            break;
        }
        if browser.path() == *path {
            graphics::fill_round_rect(
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                6,
                palette.accent_dim,
            );
        }
        graphics::fill_circle(rect.x + 11, rect.y + 11, 3, palette.accent);
        draw_text(rect.x + 22, rect.y + 8, label, palette.text, 1);
    }
    let foot_y = sidebar.bottom() - 22;
    draw_text(sidebar.x + 12, foot_y, "RAM DISK", palette.faint, 1);

    let pane = layout.pane;
    for (rect, label) in [(layout.parent, "<"), (layout.refresh, "R")] {
        graphics::fill_round_rect(
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            5,
            palette.card_hover,
        );
        draw_text_centered(rect.x, rect.right(), rect.y + 8, label, palette.accent, 1);
    }
    draw_text_fit(
        layout.parent.right() + 8,
        pane.y + 14,
        browser.path(),
        layout.refresh.x - layout.parent.right() - 16,
        palette.text,
        1,
    );
    graphics::fill_rect(pane.x + 8, pane.y + 34, pane.width - 16, 1, palette.divider);
    if browser.preview_open() {
        draw_text_fit(
            layout.body.x + 4,
            layout.body.y + 2,
            browser.preview_name(),
            layout.body.width - 8,
            palette.accent,
            1,
        );
        let max_columns = ((layout.body.width - 8) / 6).max(1) as usize;
        let mut column = 0;
        let mut y = layout.body.y + 18;
        for byte in browser.preview_bytes() {
            if *byte == b'\n' || column >= max_columns {
                column = 0;
                y += 12;
                if *byte == b'\n' {
                    continue;
                }
            }
            if y + 7 > layout.body.bottom() {
                break;
            }
            let character = if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '.'
            };
            graphics::draw_char(
                layout.body.x + 4 + column as i32 * 6,
                y,
                character,
                palette.text,
                1,
            );
            column += 1;
        }
    } else {
        for (index, entry) in browser.entries().iter().enumerate().skip(browser.first()) {
            let Some(rect) = layout.row(index - browser.first()) else {
                break;
            };
            if index == browser.selected() {
                graphics::fill_round_rect(
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                    6,
                    palette.accent_dim,
                );
            }
            if entry.directory {
                icons::app_icon(
                    WindowKind::Files,
                    Rect::new(rect.x + 6, rect.y + 3, 16, 16),
                    palette,
                );
            } else {
                graphics::fill_round_rect(rect.x + 8, rect.y + 4, 12, 14, 2, palette.muted);
            }
            draw_text_fit(
                rect.x + 28,
                rect.y + 8,
                entry.name(),
                rect.width - 42,
                palette.text,
                1,
            );
            if entry.directory {
                draw_text(rect.right() - 10, rect.y + 8, ">", palette.muted, 1);
            }
        }
        if browser.entries().is_empty() {
            draw_text(
                layout.body.x + 4,
                layout.body.y + 8,
                "EMPTY FOLDER",
                palette.faint,
                1,
            );
        }
    }
    let rename = rename_state();
    if rename.active {
        let mut line = [0u8; 48];
        let prefix = b"RENAME: ";
        line[..prefix.len()].copy_from_slice(prefix);
        let end = prefix.len() + rename.len.min(line.len() - prefix.len());
        line[prefix.len()..end].copy_from_slice(&rename.buffer[..end - prefix.len()]);
        let line = core::str::from_utf8(&line[..end]).unwrap_or("RENAME:");
        draw_text_fit(
            pane.x + 10,
            pane.bottom() - 16,
            line,
            pane.width - 20,
            palette.accent,
            1,
        );
        return;
    }
    let footer = if let Some(error) = browser.error() {
        error.message()
    } else if browser.preview_truncated() && browser.preview_open() {
        "PREVIEW TRUNCATED - BACKSPACE TO CLOSE"
    } else if browser.preview_open() {
        "READ ONLY - BACKSPACE TO CLOSE"
    } else {
        "ENTER / DOUBLE CLICK OPEN - F5 REFRESH"
    };
    draw_text_fit(
        pane.x + 10,
        pane.bottom() - 16,
        footer,
        pane.width - 20,
        if browser.error().is_some() {
            palette.warning
        } else {
            palette.faint
        },
        1,
    );
}

fn paint_security(wm: &Wm, content: Rect, palette: aether_wm::Palette) {
    let _ = wm;
    let faults = scheduler::user_faults();
    let quarantined = supervisor::quarantined_count();
    let healthy = faults == 0 && quarantined == 0;
    let banner_color = if healthy {
        palette.success
    } else {
        palette.warning
    };
    let banner = Rect::new(content.x + 14, content.y + 12, content.width - 28, 34);
    graphics::fill_round_rect(
        banner.x,
        banner.y,
        banner.width,
        banner.height,
        8,
        palette.card,
    );
    graphics::fill_circle(banner.x + 16, banner.y + 17, 5, banner_color);
    draw_text(
        banner.x + 30,
        banner.y + 9,
        if healthy {
            "NO TASK/SERVICE ALERTS"
        } else {
            "ATTENTION NEEDED"
        },
        banner_color,
        1,
    );
    draw_text(
        banner.x + 30,
        banner.y + 21,
        "PROTOTYPE - TRUSTED RING-0 DESKTOP",
        palette.muted,
        1,
    );

    let columns = 2usize;
    let gap = 8;
    let card_width = (content.width - 28 - gap) / columns as i32;
    let top = banner.bottom() + 10;
    let available = content.bottom() - 10 - top;
    let rows = 3;
    let card_height = ((available - gap * (rows - 1)) / rows).max(38);
    let mut cards: [(&str, [u8; 64]); 6] = [
        ("VERIFIED PACKAGE", [0; 64]),
        ("SERVICES", [0; 64]),
        ("PORTAL", [0; 64]),
        ("ISOLATION", [0; 64]),
        ("USER TASKS", [0; 64]),
        ("IPC", [0; 64]),
    ];
    if let Some(installed) = packaging::installed() {
        let mut text = [0u8; 64];
        let name = installed.id.as_bytes();
        let name_length = name.len().min(text.len());
        text[..name_length].copy_from_slice(&name[..name_length]);
        let mut length = name_length;
        text[length] = b' ';
        length += 1;
        length += write_version(&mut text[length..], installed.version);
        cards[0].1 = text;
        let _ = length;
    } else {
        cards[0].1 = fixed("NONE VERIFIED");
    }
    cards[1].1 = fixed_two(
        "RUNNING ",
        supervisor::running_count(),
        " QUARANTINED ",
        quarantined,
    );
    cards[2].1 = fixed_two(
        "GRANTS ",
        portal::grant_count(),
        " DENIED ",
        portal::denials(),
    );
    cards[3].1 = fixed_two(
        "ESCAPES ",
        fs::escape_denials(),
        " ACCESS ",
        fs::access_denials(),
    );
    cards[4].1 = fixed_two("EXITS ", scheduler::user_exits(), " FAULTS ", faults);
    cards[5].1 = fixed_one("DENIED ", syscall::ipc_denials());

    // On a small window the two-column cards cannot hold the long form. Use
    // compact, still truthful labels instead of truncating mid-word.
    if card_width < 170 {
        if let Some(installed) = packaging::installed() {
            let mut text = [0u8; 64];
            write_version(&mut text, installed.version);
            cards[0].1 = text;
        }
        cards[1].1 = fixed_two("RUN ", supervisor::running_count(), " QRT ", quarantined);
        cards[2].1 = fixed_two("G ", portal::grant_count(), " D ", portal::denials());
        cards[3].1 = fixed_two("ESC ", fs::escape_denials(), " ACC ", fs::access_denials());
        cards[4].1 = fixed_two("EXIT ", scheduler::user_exits(), " F ", faults);
    }

    for (index, (label, value)) in cards.iter().enumerate() {
        let column = index % columns;
        let row = index / columns;
        let x = content.x + 14 + column as i32 * (card_width + gap);
        let y = top + row as i32 * (card_height + gap);
        graphics::fill_round_rect(x, y, card_width, card_height, 8, palette.card);
        graphics::outline_round_rect(x, y, card_width, card_height, 8, palette.divider);
        draw_text(x + 10, y + 8, label, palette.muted, 1);
        let value = trim_nul(&value[..]);
        let scale = if value.len() <= 14 { 2 } else { 1 };
        draw_text_fit(
            x + 10,
            y + card_height - 10 - 7 * scale,
            value,
            card_width - 20,
            if label.contains("TASKS") && faults > 0 {
                palette.danger
            } else {
                palette.text
            },
            scale,
        );
    }
}

const PORTAL_BUTTON_WIDTH: i32 = 84;
const PORTAL_BUTTON_HEIGHT: i32 = 22;

fn portal_first_pending() -> Option<usize> {
    (0..4).find(|index| portal::pending(*index).is_some())
}

fn portal_pending_buttons(content: Rect) -> Option<(usize, Rect, Rect)> {
    let index = portal_first_pending()?;
    let y = content.y + 50 + 56 + 34;
    let approve = Rect::new(content.x + 16, y, PORTAL_BUTTON_WIDTH, PORTAL_BUTTON_HEIGHT);
    let deny = Rect::new(
        approve.right() + 8,
        y,
        PORTAL_BUTTON_WIDTH,
        PORTAL_BUTTON_HEIGHT,
    );
    Some((index, approve, deny))
}

fn paint_portal(wm: &Wm, content: Rect, palette: aether_wm::Palette) {
    let _ = wm;
    draw_text(
        content.x + 16,
        content.y + 12,
        "FILE PORTAL",
        palette.text,
        2,
    );
    draw_text(
        content.x + 16,
        content.y + 30,
        "THE DESKTOP CAN ASK; THE PORTAL DECIDES",
        palette.muted,
        1,
    );
    let top = content.y + 50;
    for (index, (label, value, color)) in [
        ("ACTIVE GRANTS", portal::grant_count(), palette.accent),
        ("GRANT USES", portal::grant_uses(), palette.text),
        ("DENIED REQUESTS", portal::denials(), palette.warning),
    ]
    .iter()
    .enumerate()
    {
        let width = (content.width - 32 - 16) / 3;
        let x = content.x + 16 + index as i32 * (width + 8);
        graphics::fill_round_rect(x, top, width, 44, 8, palette.card);
        graphics::outline_round_rect(x, top, width, 44, 8, palette.divider);
        draw_text(x + 8, top + 8, label, *color, 1);
        draw_number(x + 8, top + 22, *value, palette.text, 2);
    }

    let mut y = top + 56;
    graphics::fill_rect(
        content.x + 16,
        y - 8,
        content.width - 32,
        1,
        palette.divider,
    );
    draw_text(content.x + 16, y, "PENDING CONSENT", palette.accent, 1);
    y += 13;
    if let Some(_index) = portal_first_pending() {
        let mut text = [0u8; 320];
        let length = portal::pending_summary(&mut text);
        if let Some(line) = text[..length].split(|byte| *byte == b'\n').next() {
            draw_text_bytes(content.x + 18, y, line, palette.text, 1);
        }
        y += 13;
        for line in text[..length].split(|byte| *byte == b'\n').skip(1).take(2) {
            if line.is_empty() {
                continue;
            }
            draw_text_bytes(content.x + 18, y, line, palette.muted, 1);
            y += 12;
        }
        if let Some((_, approve, deny)) = portal_pending_buttons(content) {
            graphics::fill_round_rect(
                approve.x,
                approve.y,
                approve.width,
                approve.height,
                6,
                palette.accent,
            );
            draw_text_centered(
                approve.x,
                approve.right(),
                approve.y + 5,
                "APPROVE",
                palette.window,
                1,
            );
            graphics::fill_round_rect(deny.x, deny.y, deny.width, deny.height, 6, palette.card);
            graphics::outline_round_rect(
                deny.x,
                deny.y,
                deny.width,
                deny.height,
                6,
                palette.warning,
            );
            draw_text_centered(deny.x, deny.right(), deny.y + 5, "DENY", palette.warning, 1);
        }
        y = content.y + 50 + 56 + 34 + PORTAL_BUTTON_HEIGHT + 14;
    } else {
        draw_text(content.x + 18, y, "NO PENDING REQUESTS", palette.faint, 1);
        y += 18;
    }

    let mut text = [0u8; 640];
    let length = portal::summary(&mut text);
    graphics::fill_rect(
        content.x + 16,
        y - 8,
        content.width - 32,
        1,
        palette.divider,
    );
    for line in text[..length].split(|byte| *byte == b'\n') {
        if line.is_empty() {
            y += 4;
            continue;
        }
        if y + 14 > content.bottom() - 8 {
            break;
        }
        let is_label =
            line.ends_with(b":") || line.starts_with(b"ROOT:") || line.starts_with(b"PATH:");
        draw_text_bytes(
            content.x + 18,
            y,
            line,
            if is_label {
                palette.muted
            } else {
                palette.text
            },
            1,
        );
        y += 13;
    }
    draw_text(
        content.x + 16,
        content.bottom() - 18,
        "SINGLE-FILE GRANTS ARE REVOCABLE",
        palette.faint,
        1,
    );
}

fn paint_settings(wm: &Wm, content: Rect, palette: aether_wm::Palette) {
    draw_text(
        content.x + 16,
        content.y + 12,
        "APPEARANCE & ACCESSIBILITY",
        palette.text,
        1,
    );
    draw_text(
        content.x + 16,
        content.y + 28,
        "CHANGES APPLY IMMEDIATELY",
        palette.muted,
        1,
    );
    let settings = wm.settings();
    for row in 0..Settings::row_count() {
        let rect = Rect::new(
            content.x + 16,
            content.y + 54 + row as i32 * 32,
            content.width - 32,
            30,
        );
        if rect.bottom() > content.bottom() - 6 {
            break;
        }
        let hovered = rect.contains(wm.cursor());
        graphics::fill_round_rect(
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            7,
            if hovered {
                palette.card_hover
            } else {
                palette.card
            },
        );
        let (label, detail) = Settings::row(row);
        draw_text(rect.x + 10, rect.y + 6, label, palette.text, 1);
        draw_text(rect.x + 10, rect.y + 18, detail, palette.muted, 1);
        let control_x = rect.right() - 52;
        if row == 2 {
            draw_text_right(
                rect.right() - 12,
                rect.y + 11,
                settings.glass.label(),
                palette.accent,
                1,
            );
        } else {
            let on = settings.value(row);
            graphics::fill_round_rect(
                control_x,
                rect.y + 6,
                38,
                18,
                9,
                if on {
                    palette.accent_soft
                } else {
                    palette.control_off
                },
            );
            let knob_x = if on { control_x + 22 } else { control_x + 4 };
            graphics::fill_circle(knob_x + 6, rect.y + 15, 6, palette.text);
        }
    }
    let foot = content.bottom() - 16;
    draw_text(
        content.x + 16,
        foot,
        "PREFERENCES RESET ON REBOOT",
        palette.faint,
        1,
    );
}

fn paint_recovery(wm: &Wm, content: Rect, palette: aether_wm::Palette) {
    let _ = wm;
    draw_text(
        content.x + 16,
        content.y + 12,
        "IMAGE RECOVERY",
        palette.text,
        2,
    );
    draw_text(
        content.x + 16,
        content.y + 30,
        "USER DATA IS OUTSIDE THE IMAGE SET",
        palette.muted,
        1,
    );
    let mut text = [0u8; 512];
    let length = recovery::status_text(&mut text);
    let mut y = content.y + 52;
    for line in text[..length].split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if y + 22 > content.bottom() - 8 {
            break;
        }
        let color = if line.starts_with(b"ROLLBACK: ACTIVE") {
            palette.warning
        } else if line.starts_with(b"CURRENT:") || line.starts_with(b"SELECTED:") {
            palette.accent
        } else {
            palette.text
        };
        draw_text_bytes(content.x + 18, y, line, color, 1);
        graphics::fill_rect(
            content.x + 16,
            y + 12,
            content.width - 32,
            1,
            palette.divider,
        );
        y += 22;
    }
}

fn paint_log(wm: &Wm, content: Rect, palette: aether_wm::Palette) {
    let _ = wm;
    draw_text(
        content.x + 16,
        content.y + 12,
        "SYSTEM LOG",
        palette.text,
        2,
    );
    draw_text(
        content.x + 16,
        content.y + 30,
        "LIVE KERNEL COUNTERS",
        palette.muted,
        1,
    );
    graphics::fill_rect(
        content.x + 16,
        content.y + 44,
        content.width - 32,
        1,
        palette.divider,
    );
    let rows = [
        ("IPC DENIALS", syscall::ipc_denials()),
        ("USER EXITS", scheduler::user_exits()),
        ("USER FAULTS", scheduler::user_faults()),
        ("PORTAL GRANTS", portal::grant_count()),
        ("PORTAL DENIALS", portal::denials()),
        ("INPUT DROPPED", input::dropped() as usize),
        ("SERVICES RUNNING", supervisor::running_count()),
        ("FLAGS ESCAPES", fs::escape_denials()),
        ("FLAGS ACCESS", fs::access_denials()),
    ];
    let mut y = content.y + 54;
    for (label, value) in rows {
        if y + 16 > content.bottom() - 30 {
            break;
        }
        draw_text(content.x + 18, y, label, palette.muted, 1);
        draw_number(content.x + content.width - 50, y, value, palette.text, 1);
        y += 16;
    }
    draw_text(
        content.x + 16,
        content.bottom() - 22,
        "COMPOSITOR: SOFTWARE · FONT: ORIGINAL 5X7",
        palette.faint,
        1,
    );
}

fn paint_welcome(wm: &Wm, content: Rect, focused: bool, palette: aether_wm::Palette) {
    let _ = focused;
    let center = content.x + content.width / 2;
    icons::aether_mark(center, content.y + 28, 20, palette.warning);
    draw_text_centered(
        content.x + 8,
        content.right() - 8,
        content.y + 52,
        "WELCOME TO AETHER",
        palette.text,
        2,
    );
    draw_text_centered(
        content.x + 8,
        content.right() - 8,
        content.y + 70,
        "GOLDEN GATE-INSPIRED · CLEAN-ROOM RUST",
        palette.muted,
        1,
    );
    let shortcuts = [
        ("CTRL + LEFT / RIGHT", "SWITCH SPACES"),
        ("CTRL + UP", "SPACES OVERVIEW"),
        ("CTRL + SPACE", "SEARCH AETHER"),
        ("CTRL + TAB", "CYCLE WINDOWS"),
        ("DRAG CORNER", "RESIZE WINDOW"),
    ];
    let mut y = content.y + 88;
    for (keys, action) in shortcuts {
        if y + 14 > content.bottom() - 30 {
            break;
        }
        let key_width = keys.len() as i32 * 6 + 12;
        graphics::fill_round_rect(content.x + 16, y - 2, key_width, 14, 4, palette.card_hover);
        draw_text(content.x + 22, y + 1, keys, palette.accent, 1);
        draw_text(content.x + 26 + key_width, y + 1, action, palette.muted, 1);
        y += 14;
    }
    let _ = wm;
    draw_text_centered(
        content.x + 8,
        content.right() - 8,
        content.bottom() - 18,
        "ORIGINAL ARTWORK · NO APPLE CODE OR ASSETS",
        palette.faint,
        1,
    );
}

fn paint_menu_bar(wm: &Wm, palette: aether_wm::Palette, glass: GlassLevel) {
    let screen = wm.screen();
    let alpha = if wm.settings().high_contrast {
        255
    } else {
        glass.panel_alpha()
    };
    graphics::blend_rect(
        screen.x,
        screen.y,
        screen.width,
        layout::MENU_BAR_HEIGHT,
        palette.menu_bar,
        alpha,
    );
    graphics::blend_rect(
        screen.x,
        screen.y + layout::MENU_BAR_HEIGHT - 1,
        screen.width,
        1,
        palette.menu_border,
        220,
    );
    graphics::blend_rect(
        screen.x,
        screen.y + 1,
        screen.width,
        1,
        0xffffff,
        if wm.settings().high_contrast { 30 } else { 45 },
    );

    let logo = layout::menu_logo_rect(screen);
    icons::aether_mark(logo.center().x, logo.center().y, 7, palette.warning);
    let cursor = wm.cursor();
    let titles = layout::menu_title_rects(screen, wm.app_name());
    for (index, kind) in MenuKind::BAR.iter().enumerate() {
        let rect = titles[index];
        let open = wm.menu_state().is_some_and(|menu| menu.kind == *kind);
        if open || rect.contains(cursor) {
            graphics::fill_round_rect(
                rect.x + 2,
                screen.y + 4,
                rect.width - 4,
                layout::MENU_BAR_HEIGHT - 8,
                6,
                if open {
                    palette.accent_dim
                } else {
                    palette.card
                },
            );
        }
        let label = kind.label(wm.app_name());
        draw_text(
            rect.x + 8,
            screen.y + 10,
            label,
            if open { palette.accent } else { palette.muted },
            1,
        );
    }

    let status = layout::status_items(screen);
    icons::search(
        Rect::new(status.search.x + 2, status.search.y + 1, 14, 14),
        palette.muted,
    );
    icons::workspace_grid(status.spaces, palette.muted);
    let active = wm.workspace();
    let cell = 7;
    let x = status.spaces.x + (active as i32 % 2) * (cell + 2);
    let y = status.spaces.y + (active as i32 / 2) * (cell + 2);
    graphics::fill_rect(x, y, cell, cell, palette.accent);
    let unhealthy = scheduler::user_faults() > 0 || supervisor::quarantined_count() > 0;
    icons::shield_status(
        status.security,
        if unhealthy {
            palette.warning
        } else {
            palette.success
        },
    );
    let mut clock = [0u8; 12];
    let length = format_clock(&mut clock, scheduler::ticks() / 100);
    draw_text(status.clock.x, screen.y + 11, "UP ", palette.faint, 1);
    draw_text(
        status.clock.x + 18,
        screen.y + 11,
        core::str::from_utf8(&clock[..length]).unwrap_or("0:00"),
        palette.muted,
        1,
    );
}

fn paint_menu(wm: &Wm, palette: aether_wm::Palette, glass: GlassLevel) {
    let Some(menu) = wm.menu_state() else {
        return;
    };
    let Some(panel) = wm.menu_panel() else {
        return;
    };
    let alpha = if wm.settings().high_contrast {
        255
    } else {
        glass.menu_alpha()
    };
    graphics::blend_round_rect(
        panel.x + 3,
        panel.y + 6,
        panel.width,
        panel.height,
        10,
        palette.shadow,
        70,
    );
    graphics::blend_round_rect(
        panel.x,
        panel.y,
        panel.width,
        panel.height,
        10,
        palette.menu_bar,
        alpha,
    );
    graphics::blend_rect(panel.x + 10, panel.y + 1, panel.width - 20, 1, 0xffffff, 55);
    graphics::outline_round_rect(
        panel.x,
        panel.y,
        panel.width,
        panel.height,
        10,
        palette.menu_border,
    );
    let items = menu.kind.items();
    let cursor = wm.cursor();
    for (index, item) in items.iter().enumerate() {
        let rect = layout::menu_item_rect(panel, index);
        if item.is_separator() {
            graphics::fill_rect(
                rect.x + 8,
                rect.y + rect.height / 2,
                rect.width - 16,
                1,
                palette.divider,
            );
            continue;
        }
        let selected = (menu.keyboard && menu.selected == index) || rect.contains(cursor);
        if selected {
            graphics::fill_round_rect(
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                6,
                palette.accent_dim,
            );
        }
        let toggled = match item.command {
            Command::ToggleReduceMotion => wm.settings().reduce_motion,
            Command::ToggleHighContrast => wm.settings().high_contrast,
            Command::ToggleDockMagnification => wm.settings().magnify_dock,
            _ => false,
        };
        if toggled {
            icons::check(rect.x + 8, rect.y + 7, 8, palette.accent);
        }
        draw_text(
            rect.x + 24,
            rect.y + 7,
            item.label,
            if selected {
                palette.accent
            } else {
                palette.text
            },
            1,
        );
        if !item.shortcut.is_empty() {
            draw_text_right(
                rect.right() - 10,
                rect.y + 7,
                item.shortcut,
                palette.faint,
                1,
            );
        }
    }
}

fn paint_overview(wm: &Wm, palette: aether_wm::Palette) {
    let screen = wm.screen();
    let strength = (wm.overview_progress() as u32 * 170 / 256) as u8;
    graphics::blend_rect(
        screen.x,
        screen.y,
        screen.width,
        screen.height,
        palette.overlay,
        strength,
    );
    let title = "SPACES";
    draw_text_centered(
        screen.x,
        screen.right(),
        screen.y + layout::MENU_BAR_HEIGHT + 8,
        title,
        palette.text,
        2,
    );
    draw_text_centered(
        screen.x,
        screen.right(),
        screen.y + layout::MENU_BAR_HEIGHT + 26,
        "CHOOSE A DESKTOP · CTRL + ARROWS · ESC TO CLOSE",
        palette.muted,
        1,
    );
    let progress = i32::from(wm.overview_progress());
    let cards = layout::overview_cards(screen);
    for (index, card) in cards.iter().enumerate() {
        let inset = (256 - progress) * 18 / 256;
        let rect = card.inset(inset);
        let selected = index as u8 == wm.overview_selection();
        graphics::blend_round_rect(
            rect.x + 3,
            rect.y + 6,
            rect.width,
            rect.height,
            12,
            palette.shadow,
            80,
        );
        graphics::fill_round_rect(rect.x, rect.y, rect.width, rect.height, 12, palette.card);
        graphics::vertical_gradient(
            rect.x + 1,
            rect.y + 28,
            rect.width - 2,
            rect.height - 30,
            palette.wallpaper_top,
            palette.wallpaper_bottom,
        );
        graphics::outline_round_rect(
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            12,
            if selected {
                palette.accent
            } else {
                palette.divider
            },
        );
        if selected {
            graphics::outline_round_rect(
                rect.x + 1,
                rect.y + 1,
                rect.width - 2,
                rect.height - 2,
                11,
                palette.accent_soft,
            );
        }
        let label = workspace_label(index as u8);
        draw_text(rect.x + 12, rect.y + 12, label, palette.text, 1);
        let mut count = [0u8; 16];
        let count_length = format_u64(&mut count, wm.workspace_window_count(index as u8) as u64);
        let mut suffix = [0u8; 20];
        let suffix_text = append_bytes(
            &mut suffix,
            &count[..count_length],
            if wm.workspace_window_count(index as u8) == 1 {
                b" WINDOW"
            } else {
                b" WINDOWS"
            },
        );
        draw_text_right(
            rect.right() - 12,
            rect.y + 12,
            core::str::from_utf8(suffix_text).unwrap_or("0"),
            palette.muted,
            1,
        );
        let previews = layout::overview_preview_rects(rect);
        let mut drawn = 0usize;
        graphics::set_clip(rect.x, rect.y, rect.width, rect.height);
        for id in wm.order().iter().rev() {
            let Some(window) = wm.window(*id) else {
                continue;
            };
            if window.workspace != index as u8 || !window.is_visible() {
                continue;
            }
            if drawn >= previews.len() {
                break;
            }
            let preview = previews[drawn];
            graphics::fill_round_rect(
                preview.x,
                preview.y,
                preview.width,
                preview.height,
                5,
                palette.window,
            );
            graphics::fill_round_rect(
                preview.x,
                preview.y,
                preview.width,
                11,
                5,
                if drawn == 0 {
                    palette.accent_dim
                } else {
                    palette.title_inactive
                },
            );
            graphics::fill_rect(
                preview.x + 5,
                preview.y + 4,
                (preview.width - 10).max(4),
                2,
                palette.muted,
            );
            draw_text_fit(
                preview.x + 6,
                preview.y + 2,
                window.kind.title(),
                preview.width - 12,
                palette.faint,
                1,
            );
            for line in 0..2 {
                graphics::fill_rect(
                    preview.x + 5,
                    preview.y + 17 + line * 6,
                    (preview.width - 14).max(4),
                    2,
                    palette.divider,
                );
            }
            drawn += 1;
        }
        graphics::clear_clip();
        if drawn == 0 {
            draw_text(rect.x + 12, rect.y + 48, "EMPTY DESKTOP", palette.faint, 1);
        }
    }
    draw_text_centered(
        screen.x,
        screen.right(),
        screen.bottom() - layout::DOCK_HEIGHT - 40,
        "CLICK A DESKTOP OR PRESS ENTER",
        palette.faint,
        1,
    );
}

fn paint_dock(wm: &Wm, palette: aether_wm::Palette, glass: GlassLevel) {
    let screen = wm.screen();
    let panel = layout::dock_panel(screen);
    let alpha = if wm.settings().high_contrast {
        255
    } else {
        glass.panel_alpha()
    };
    graphics::blend_round_rect(
        panel.x + 4,
        panel.y + 7,
        panel.width,
        panel.height,
        18,
        palette.shadow,
        75,
    );
    graphics::blend_round_rect(
        panel.x,
        panel.y,
        panel.width,
        panel.height,
        18,
        palette.dock,
        alpha,
    );
    graphics::blend_rect(
        panel.x + 18,
        panel.y + 1,
        panel.width - 36,
        1,
        0xffffff,
        if wm.settings().high_contrast { 30 } else { 65 },
    );
    graphics::outline_round_rect(
        panel.x,
        panel.y,
        panel.width,
        panel.height,
        18,
        palette.dock_border,
    );

    let icons = layout::dock_icon_rects(screen, wm.dock_hover());
    let cursor = wm.cursor();
    for (index, slot) in DOCK_SLOTS.iter().enumerate() {
        let rect = icons[index];
        if index == DOCK_SLOTS.len() - 1 {
            graphics::fill_rect(
                rect.x - 10,
                rect.y + 8,
                1,
                rect.height - 16,
                palette.dock_border,
            );
        }
        match slot {
            DockSlot::App(kind) => {
                icons::app_icon(*kind, rect, palette);
                if wm.is_running(*kind) {
                    graphics::fill_circle(
                        rect.center().x,
                        panel.bottom() - 5,
                        2,
                        if wm.is_active(*kind) {
                            palette.accent
                        } else {
                            palette.muted
                        },
                    );
                }
                let hovered = wm.dock_hover()[index] > 80;
                if hovered {
                    let label = kind.app_name();
                    draw_tooltip(rect.center().x, rect.y - 22, label, palette);
                }
            }
            DockSlot::Overview => {
                let selected = wm.overview_open() || wm.overview_progress() > 0;
                graphics::fill_round_rect(
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                    rect.width / 5,
                    if selected {
                        palette.accent_dim
                    } else {
                        palette.card
                    },
                );
                icons::workspace_grid(
                    Rect::new(
                        rect.x + rect.width / 4,
                        rect.y + rect.height / 4,
                        rect.width / 2,
                        rect.height / 2,
                    ),
                    if selected {
                        palette.accent
                    } else {
                        palette.muted
                    },
                );
                if wm.dock_hover()[index] > 80 {
                    draw_tooltip(rect.center().x, rect.y - 22, "Spaces", palette);
                }
            }
        }
        if rect.contains(cursor) {
            graphics::blend_round_rect(
                rect.x - 1,
                rect.y - 1,
                rect.width + 2,
                rect.height + 2,
                rect.width / 5 + 1,
                palette.accent,
                35,
            );
        }
    }
}

fn paint_spotlight(wm: &Wm, palette: aether_wm::Palette, glass: GlassLevel) {
    let screen = wm.screen();
    graphics::blend_rect(
        screen.x,
        screen.y,
        screen.width,
        screen.height,
        palette.overlay,
        if wm.settings().high_contrast {
            210
        } else {
            150
        },
    );
    let panel = layout::spotlight_panel(screen);
    graphics::blend_round_rect(
        panel.x + 5,
        panel.y + 10,
        panel.width,
        panel.height,
        16,
        palette.shadow,
        90,
    );
    graphics::blend_round_rect(
        panel.x,
        panel.y,
        panel.width,
        panel.height,
        16,
        palette.menu_bar,
        if wm.settings().high_contrast {
            255
        } else {
            240
        },
    );
    graphics::outline_round_rect(
        panel.x,
        panel.y,
        panel.width,
        panel.height,
        16,
        palette.menu_border,
    );
    graphics::blend_rect(panel.x + 16, panel.y + 1, panel.width - 32, 1, 0xffffff, 60);
    let _ = glass;

    let field = layout::spotlight_field(panel);
    graphics::fill_round_rect(field.x, field.y, field.width, field.height, 9, palette.card);
    graphics::outline_round_rect(
        field.x,
        field.y,
        field.width,
        field.height,
        9,
        palette.accent_soft,
    );
    icons::search(Rect::new(field.x + 10, field.y + 9, 16, 16), palette.accent);
    let query = wm.spotlight_query();
    if query.is_empty() {
        draw_text(
            field.x + 34,
            field.y + 13,
            "SEARCH AETHER",
            palette.faint,
            1,
        );
    } else {
        draw_text(field.x + 34, field.y + 13, query, palette.text, 1);
        let caret_x = field.x + 34 + graphics::text_width(query, 1);
        graphics::fill_rect(caret_x + 1, field.y + 11, 1, 12, palette.accent);
    }
    draw_text_right(
        field.right() - 10,
        field.y + 13,
        "CTRL + SPACE",
        palette.faint,
        1,
    );

    let count = wm.search_result_count();
    if count == 0 {
        draw_text(panel.x + 20, panel.y + 72, "NO RESULTS", palette.muted, 2);
    }
    let cursor = wm.cursor();
    for index in 0..count {
        let Some(item) = wm.search_result(index) else {
            break;
        };
        let rect = layout::spotlight_result_rect(panel, index);
        let selected = index == wm.spotlight_selection() || rect.contains(cursor);
        if selected {
            graphics::fill_round_rect(
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                7,
                palette.accent_dim,
            );
        }
        let icon_rect = Rect::new(rect.x + 6, rect.y + 4, 18, 18);
        match item.command {
            Command::Open(kind) => icons::app_icon(kind, icon_rect, palette),
            Command::Workspace(_) => {
                graphics::fill_round_rect(
                    icon_rect.x,
                    icon_rect.y,
                    icon_rect.width,
                    icon_rect.height,
                    5,
                    palette.card,
                );
                icons::workspace_grid(icon_rect, palette.muted);
            }
            _ => {
                graphics::fill_round_rect(
                    icon_rect.x,
                    icon_rect.y,
                    icon_rect.width,
                    icon_rect.height,
                    5,
                    palette.card,
                );
                icons::aether_mark(
                    icon_rect.center().x,
                    icon_rect.center().y,
                    7,
                    palette.warning,
                );
            }
        }
        draw_text(
            rect.x + 32,
            rect.y + 9,
            item.label,
            if selected {
                palette.accent
            } else {
                palette.text
            },
            1,
        );
        draw_text_right(rect.right() - 10, rect.y + 9, item.kind, palette.faint, 1);
    }
    draw_text_centered(
        panel.x,
        panel.right(),
        panel.bottom() - 18,
        "ENTER OPEN · ESC CLOSE · TYPE TO FILTER",
        palette.faint,
        1,
    );
}

fn paint_cursor(point: aether_wm::Point, palette: aether_wm::Palette) {
    let x = point.x;
    let y = point.y;
    let outline = [
        (x, y),
        (x, y + 18),
        (x + 5, y + 14),
        (x + 8, y + 21),
        (x + 11, y + 20),
        (x + 8, y + 13),
        (x + 14, y + 13),
    ];
    let fill = [
        (x + 1, y + 2),
        (x + 1, y + 15),
        (x + 5, y + 12),
        (x + 7, y + 18),
        (x + 9, y + 17),
        (x + 6, y + 11),
        (x + 12, y + 11),
    ];
    graphics::fill_polygon(&outline, palette.shadow);
    graphics::fill_polygon(&fill, palette.text);
}

fn draw_tooltip(center_x: i32, y: i32, label: &str, palette: aether_wm::Palette) {
    let width = graphics::text_width(label, 1) + 16;
    let rect = Rect::new(center_x - width / 2, y, width, 18);
    graphics::blend_round_rect(
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        6,
        palette.menu_bar,
        235,
    );
    graphics::outline_round_rect(
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        6,
        palette.menu_border,
    );
    draw_text(rect.x + 8, rect.y + 5, label, palette.text, 1);
}

fn draw_pill(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    palette: aether_wm::Palette,
    label: &str,
    color: u32,
) {
    graphics::blend_round_rect(x, y, width, height, height / 2, palette.menu_bar, 230);
    graphics::outline_round_rect(x, y, width, height, height / 2, palette.menu_border);
    draw_text_centered(x, x + width, y + (height - 7) / 2, label, color, 1);
}

fn draw_text(x: i32, y: i32, text: &str, rgb: u32, scale: i32) {
    graphics::draw_text(x, y, text, rgb, scale);
}

fn draw_text_fit(x: i32, y: i32, text: &str, max_width: i32, rgb: u32, scale: i32) {
    let scale = scale.max(1);
    let characters = (max_width / (6 * scale)).max(0) as usize;
    graphics::draw_text(x, y, truncate_chars(text, characters), rgb, scale);
}

fn truncate_chars(text: &str, characters: usize) -> &str {
    match text.char_indices().nth(characters) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

fn draw_text_right(right: i32, y: i32, text: &str, rgb: u32, scale: i32) {
    let width = graphics::text_width(text, scale);
    graphics::draw_text(right - width, y, text, rgb, scale);
}

fn draw_text_centered(left: i32, right: i32, y: i32, text: &str, rgb: u32, scale: i32) {
    let width = graphics::text_width(text, scale);
    let available = (right - left).max(0);
    if width <= available {
        graphics::draw_text(left + (available - width) / 2, y, text, rgb, scale);
        return;
    }
    let characters = (available / (6 * scale.max(1))).max(0) as usize;
    let truncated = truncate_chars(text, characters);
    let width = graphics::text_width(truncated, scale);
    graphics::draw_text(left + (available - width) / 2, y, truncated, rgb, scale);
}

fn draw_text_bytes(x: i32, y: i32, bytes: &[u8], rgb: u32, scale: i32) {
    if let Ok(text) = core::str::from_utf8(bytes) {
        graphics::draw_text(x, y, text, rgb, scale);
    }
}

fn draw_number(x: i32, y: i32, value: usize, rgb: u32, scale: i32) {
    let mut buffer = [0u8; 20];
    let length = format_u64(&mut buffer, value as u64);
    draw_text_bytes(x, y, &buffer[..length], rgb, scale);
}

fn workspace_label(workspace: u8) -> &'static str {
    match workspace {
        0 => "SPACE 1",
        1 => "SPACE 2",
        2 => "SPACE 3",
        _ => "SPACE 4",
    }
}

fn format_clock(buffer: &mut [u8], total_seconds: u64) -> usize {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds / 60) % 60;
    let seconds = total_seconds % 60;
    let mut written = 0usize;
    if hours > 0 {
        written += format_u64(&mut buffer[written..], hours);
        buffer[written] = b':';
        written += 1;
    }
    if written == 0 {
        written += format_u64(&mut buffer[written..], minutes);
    } else if minutes < 10 {
        buffer[written] = b'0';
        written += 1;
        written += format_u64(&mut buffer[written..], minutes);
    } else {
        written += format_u64(&mut buffer[written..], minutes);
    }
    buffer[written] = b':';
    written += 1;
    if seconds < 10 {
        buffer[written] = b'0';
        written += 1;
    }
    written += format_u64(&mut buffer[written..], seconds);
    written
}

fn format_u64(buffer: &mut [u8], mut value: u64) -> usize {
    if value == 0 {
        buffer[0] = b'0';
        return 1;
    }
    let mut digits = [0u8; 20];
    let mut index = digits.len();
    while value > 0 {
        index -= 1;
        digits[index] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    let length = digits.len() - index;
    buffer[..length].copy_from_slice(&digits[index..]);
    length
}

fn append_bytes<'a>(buffer: &'a mut [u8], first: &[u8], second: &[u8]) -> &'a [u8] {
    let length = first.len().min(buffer.len());
    buffer[..length].copy_from_slice(&first[..length]);
    let mut written = length;
    let remaining = buffer.len() - written;
    let second_length = second.len().min(remaining);
    buffer[written..written + second_length].copy_from_slice(&second[..second_length]);
    written += second_length;
    &buffer[..written]
}

fn fixed(text: &str) -> [u8; 64] {
    let mut buffer = [0u8; 64];
    let length = text.len().min(buffer.len());
    buffer[..length].copy_from_slice(&text.as_bytes()[..length]);
    buffer
}

fn fixed_one(prefix: &str, value: usize) -> [u8; 64] {
    let mut buffer = [0u8; 64];
    let written = prefix.len().min(buffer.len());
    buffer[..written].copy_from_slice(&prefix.as_bytes()[..written]);
    format_u64(&mut buffer[written..], value as u64);
    buffer
}

fn fixed_two(first: &str, first_value: usize, second: &str, second_value: usize) -> [u8; 64] {
    let mut buffer = [0u8; 64];
    let mut written = first.len().min(buffer.len());
    buffer[..written].copy_from_slice(&first.as_bytes()[..written]);
    written += format_u64(&mut buffer[written..], first_value as u64);
    let remaining = buffer.len() - written;
    let label_length = second.len().min(remaining);
    buffer[written..written + label_length].copy_from_slice(&second.as_bytes()[..label_length]);
    written += label_length;
    format_u64(&mut buffer[written..], second_value as u64);
    buffer
}

fn trim_nul(buffer: &[u8]) -> &str {
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    core::str::from_utf8(&buffer[..end]).unwrap_or("?")
}

fn write_version(buffer: &mut [u8], version: Version) -> usize {
    let mut written = 0usize;
    written += format_u64(&mut buffer[written..], u64::from(version.major));
    buffer[written] = b'.';
    written += 1;
    written += format_u64(&mut buffer[written..], u64::from(version.minor));
    buffer[written] = b'.';
    written += 1;
    written += format_u64(&mut buffer[written..], u64::from(version.patch));
    written
}
