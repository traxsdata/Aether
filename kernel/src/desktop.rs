// SPDX-License-Identifier: Apache-2.0
//! Software compositor and Golden Gate-inspired desktop shell.
//!
//! The compositor paints an original, clean-room desktop directly into the GOP
//! framebuffer. It uses layered translucency, specular highlights, tighter
//! window corners, edge-to-edge sidebars, a floating dock, Spaces, and a
//! Spotlight-style search. Those are design principles, not Apple code or
//! assets: every glyph, icon, palette, and animation is defined in this tree.
//!
//! The desktop owns no authority. Files are read through the filesystem
//! service, grants through the portal, and service state through the
//! supervisor. A compromised compositor could lie about pixels, but it cannot
//! mint a file capability or bypass a root check.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::graphics;
use crate::icons;
use crate::input::{self, Key};
use crate::serial_line;
use crate::{fs, packaging, portal, recovery, scheduler, supervisor, syscall};
use aether_packaging::manifest::Version;
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
}

impl DesktopState {
    const fn new() -> Self {
        Self {
            wm: None,
            last_ticks: 0,
            last_second: 0,
            last_buttons: 0,
            last_paint: 0,
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
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked and boot is single-threaded.
    unsafe {
        *DESKTOP.0.get() = DesktopState {
            wm: Some(wm),
            last_ticks: now,
            last_second: now / 100,
            last_buttons: mouse.buttons,
            last_paint: now,
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

fn store(wm: Wm, ticks: u64, second: u64, buttons: u8, paint: u64) {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the state update.
    unsafe {
        (*DESKTOP.0.get()).wm = Some(wm);
        (*DESKTOP.0.get()).last_ticks = ticks;
        (*DESKTOP.0.get()).last_second = second;
        (*DESKTOP.0.get()).last_buttons = buttons;
        (*DESKTOP.0.get()).last_paint = paint;
    }
}

/// Repaints the whole desktop.
pub fn redraw() {
    if !graphics::is_ready() {
        return;
    }
    let Some(wm) = current() else {
        return;
    };
    let palette = wm.palette();
    let glass = effective_glass(wm.settings());
    let screen = wm.screen();

    paint_wallpaper(screen, palette, wm.settings().high_contrast);
    paint_windows(&wm, palette);
    if wm.overview_open() || wm.overview_progress() > 0 {
        paint_overview(&wm, palette);
    }
    paint_dock(&wm, palette, glass);
    paint_menu_bar(&wm, palette, glass);
    if wm.menu_state().is_some() {
        paint_menu(&wm, palette, glass);
    }
    if wm.spotlight_open() {
        paint_spotlight(&wm, palette, glass);
    }
    paint_cursor(wm.cursor(), palette);
    graphics::present();
}

/// Advances animations and routes pointer state into the window manager.
///
/// The idle loop calls this after every interrupt. Work is proportional to
/// actual change: a static desktop with no animation is not repainted.
pub fn tick() {
    let (wm, last_ticks, last_second, last_buttons, last_paint) = {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked for the snapshot.
        let state = unsafe { &*DESKTOP.0.get() };
        (
            state.wm,
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
    let elapsed = now.saturating_sub(last_ticks).min(8) as u16;
    let mut changed = wm.tick(elapsed);
    let mut interactive = false;

    let mouse = input::mouse();
    let point = aether_wm::Point::new(mouse.x, mouse.y);
    let pressed = mouse.buttons & 1 != 0;
    let was_pressed = last_buttons & 1 != 0;
    if pressed != was_pressed {
        if pressed {
            wm.pointer_down(point, now);
        } else {
            wm.pointer_moved(point);
            wm.pointer_up();
        }
        changed = true;
        interactive = true;
    } else if point.x != wm.cursor().x || point.y != wm.cursor().y {
        let moved = wm.pointer_moved(point);
        changed |= moved;
        interactive |= moved;
    }

    let second = now / 100;
    let clock_changed = second != last_second;
    if clock_changed {
        changed = true;
    }
    // Animate at roughly 33 fps on a 100 Hz tick. Pointer, keyboard, and clock
    // events still present immediately; state advances every tick regardless.
    let animation_due = !wm.is_animating() || now.saturating_sub(last_paint) >= 3;
    if changed && (interactive || clock_changed || animation_due) {
        store(wm, now, second, mouse.buttons, now);
        redraw();
    } else {
        store(wm, now, last_second, mouse.buttons, last_paint);
    }
}

/// Handles a decoded physical key.
pub fn on_key(key: Key, modifiers: Modifiers) {
    let Some(mut wm) = current() else {
        return;
    };
    let translated = match key {
        Key::Char(byte) => WmKey::Char(byte),
        Key::Enter => WmKey::Enter,
        Key::Backspace => WmKey::Backspace,
        Key::Tab => WmKey::Tab,
        Key::Escape => WmKey::Escape,
        Key::Up => WmKey::Up,
        Key::Down => WmKey::Down,
        Key::Left => WmKey::Left,
        Key::Right => WmKey::Right,
        Key::Function(number) => WmKey::Function(number),
    };
    let outcome = wm.key(translated, modifiers);
    if outcome.changed {
        let now = scheduler::ticks();
        store(wm, now, now / 100, input::mouse().buttons, now);
        redraw();
    }
    if let Some(label) = outcome.announce {
        if translated == WmKey::Enter {
            serial_line!("desktop: activated window={label}");
        } else {
            serial_line!("desktop: action={label}");
        }
    }
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
        wm.focus_window(id);
        let now = scheduler::ticks();
        store(wm, now, now / 100, input::mouse().buttons, now);
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
        "desktop: state space={} focus={:?} mode={} order={:?}",
        wm.workspace(),
        wm.focus_id(),
        mode(),
        wm.order()
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
    if wm.transition().active {
        let transition = wm.transition();
        let progress = i32::from(transition.anim.eased());
        let direction = i32::from(transition.direction);
        let away = -direction * progress * screen.width / 256;
        let enter = direction * (256 - progress) * screen.width / 256;
        paint_workspace(wm, palette, transition.from, away);
        paint_workspace(wm, palette, transition.to, enter);
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
    } else {
        paint_workspace(wm, palette, wm.workspace(), 0);
    }
}

fn paint_workspace(wm: &Wm, palette: aether_wm::Palette, workspace: u8, offset_x: i32) {
    let icons = layout::dock_icon_rects(wm.screen(), wm.dock_hover());
    for id in wm.order().iter() {
        let Some(window) = wm.window(*id) else {
            continue;
        };
        if window.workspace != workspace || !window.is_visible() {
            continue;
        }
        let minimize_target = dock_target(window.kind, &icons).unwrap_or(window.frame);
        let frame = window
            .animated_frame(minimize_target)
            .translate(offset_x, 0);
        let focused = wm.focus_id() == Some(window.id);
        paint_window(wm, window, frame, focused, palette);
    }
}

fn dock_target(kind: WindowKind, icons: &[Rect; layout::DOCK_ITEMS]) -> Option<Rect> {
    DOCK_SLOTS
        .iter()
        .position(|slot| match slot {
            DockSlot::App(slot_kind) => *slot_kind == kind,
            DockSlot::Overview => false,
        })
        .map(|index| icons[index])
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
    graphics::blend_rect(
        content.x,
        content.y,
        content.width,
        content.height,
        palette.window,
        if settings.high_contrast { 255 } else { 248 },
    );
    graphics::set_clip(content.x, content.y, content.width, content.height);
    paint_content(wm, window, content, focused, palette);
    graphics::clear_clip();

    let lights = layout::traffic_lights(frame);
    paint_traffic_lights(wm, &lights, focused, palette);
    draw_text_centered(
        frame.x + 72,
        frame.right() - 12,
        frame.y + 13,
        window.kind.title(),
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
    }
}

fn paint_files(wm: &Wm, content: Rect, palette: aether_wm::Palette) {
    let glass = effective_glass(wm.settings());
    let sidebar_width = (content.width / 3).clamp(92, 138);
    let sidebar = Rect::new(content.x, content.y, sidebar_width, content.height);
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

    let items = [
        ("Home", palette.accent),
        ("Applications", palette.success),
        ("Portal", palette.warning),
        ("System", palette.muted),
    ];
    for (index, (label, color)) in items.iter().enumerate() {
        let y = sidebar.y + 50 + index as i32 * 26;
        if y + 20 > sidebar.bottom() {
            break;
        }
        if index == 0 {
            graphics::fill_round_rect(
                sidebar.x + 6,
                y - 4,
                sidebar.width - 12,
                22,
                6,
                palette.accent_dim,
            );
        }
        graphics::fill_circle(sidebar.x + 17, y + 6, 3, *color);
        draw_text(sidebar.x + 28, y + 3, label, palette.text, 1);
    }
    let foot_y = sidebar.bottom() - 22;
    draw_text(sidebar.x + 12, foot_y, "RAM DISK", palette.faint, 1);

    let pane = Rect::new(
        sidebar.right(),
        content.y,
        content.width - sidebar.width,
        content.height,
    );
    draw_text(pane.x + 14, pane.y + 11, "AETHERFS", palette.text, 1);
    draw_text_right(pane.right() - 14, pane.y + 11, "VOLATILE", palette.faint, 1);
    graphics::fill_rect(
        pane.x + 14,
        pane.y + 25,
        pane.width - 28,
        1,
        palette.divider,
    );

    let mut entries = [0u8; 640];
    let length = fs::list_path("/", &mut entries).unwrap_or(0);
    let mut y = pane.y + 34;
    let row_height = 24;
    for line in entries[..length].split(|byte| *byte == b'\n') {
        if line.is_empty() || y + row_height > pane.bottom() - 18 {
            break;
        }
        let mut parts = line.split(|byte| *byte == b'\t');
        let name = parts.next().unwrap_or(b"?");
        let kind = parts.next().unwrap_or(b"f");
        let size = parts.next().unwrap_or(b"0");
        let is_dir = kind == b"d";
        if y == pane.y + 34 {
            graphics::fill_round_rect(
                pane.x + 8,
                y - 2,
                pane.width - 16,
                row_height - 2,
                6,
                palette.accent_dim,
            );
        }
        if is_dir {
            icons::app_icon(
                WindowKind::Files,
                Rect::new(pane.x + 16, y + 2, 16, 16),
                palette,
            );
        } else {
            graphics::fill_round_rect(pane.x + 16, y + 4, 12, 14, 2, palette.muted);
        }
        let name_text = core::str::from_utf8(name).unwrap_or("?");
        draw_text(pane.x + 40, y + 6, name_text, palette.text, 1);
        if !is_dir {
            let size_text = core::str::from_utf8(size).unwrap_or("0");
            let mut buffer = [0u8; 16];
            let label = append_bytes(&mut buffer, size_text.as_bytes(), b" B");
            draw_text_right(
                pane.right() - 16,
                y + 6,
                core::str::from_utf8(label).unwrap_or("0 B"),
                palette.faint,
                1,
            );
        }
        y += row_height;
    }
    if y == pane.y + 34 {
        draw_text(pane.x + 16, y + 8, "NO ENTRIES", palette.faint, 1);
    }
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
            "SYSTEM PROTECTED"
        } else {
            "ATTENTION NEEDED"
        },
        banner_color,
        1,
    );
    draw_text(
        banner.x + 30,
        banner.y + 21,
        "POLICY ENFORCED BELOW THE DESKTOP",
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

    let mut text = [0u8; 640];
    let length = portal::summary(&mut text);
    let mut y = top + 56;
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
        (glass.panel_alpha() + 20).min(250)
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
