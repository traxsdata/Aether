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

use crate::arch::x86_64::timer::{self, InterruptGuard};
use crate::graphics;
use crate::icons;
use crate::input::{self, Key};
use crate::net::consent;
use crate::serial_line;
use crate::{fs, net, packaging, portal, recovery, scheduler, supervisor, syscall};
use aether_packaging::manifest::Version;
use aether_wm::browser::{Browser, BrowserLayout, LOCATIONS};
use aether_wm::layout::{self, DOCK_SLOTS, DockSlot};
use aether_wm::{
    Command, Damage, GlassLevel, Key as WmKey, MenuKind, Modifiers, Rect, Settings, WORKSPACES,
    Window, WindowKind, Wm,
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
    /// A Linux surface changed; only the content rectangle needs repainting.
    linux_dirty: bool,
    /// A Linux title or window geometry changed; repaint the whole frame.
    linux_frame_dirty: bool,
    /// A menu-bar clock or diagnostic view needs repainting.
    clock_dirty: bool,
    /// Portal consent or other non-frame live content changed.
    live_dirty: bool,
    scene_paints: u64,
    cursor_paints: u64,
    last_paint_ticks: u64,
    max_paint_ticks: u64,
    /// TSC-timed scene paint durations; the tick counters above are 10 ms
    /// granular, which hides most partial repaints.
    last_paint_us: u64,
    max_paint_us: u64,
    total_paint_us: u64,
    fast_paints: u64,
    slow_paints: u64,
    rename: RenameState,
    /// Last network consent generation the desktop painted.
    network_generation: u64,
    /// Tick the current network consent prompt appeared, while one is up.
    network_prompt: Option<u64>,
    /// Keyboard selection in the Applications launcher grid.
    applications_selected: usize,
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
            linux_dirty: true,
            linux_frame_dirty: true,
            clock_dirty: false,
            live_dirty: true,
            scene_paints: 0,
            cursor_paints: 0,
            last_paint_ticks: 0,
            max_paint_ticks: 0,
            last_paint_us: 0,
            max_paint_us: 0,
            total_paint_us: 0,
            fast_paints: 0,
            slow_paints: 0,
            rename: RenameState::new(),
            network_generation: 0,
            network_prompt: None,
            applications_selected: 0,
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
    // Restore persisted desktop and accessibility settings before the first
    // frame, so a high-contrast user never sees the default theme flash.
    // A missing or bad record falls back to the defaults already loaded.
    crate::settings::load(&mut wm);
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
    // The settings dirty flag is the only place a user change is detected.
    // Persist once here, at the single choke point every mutation reaches,
    // so a real change writes the record exactly once instead of per frame.
    let mut wm = wm;
    if wm.settings_dirty() {
        crate::settings::persist(&wm);
        wm.clear_settings_dirty();
    }
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

/// Marks the live diagnostic regions for the next frame.
pub fn mark_live_dirty() {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupt-serialized update; IRQs never enter the desktop.
    unsafe {
        (*DESKTOP.0.get()).live_dirty = true;
    }
}

fn applications_selected() -> usize {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupt-serialized read of a plain counter.
    unsafe { (*DESKTOP.0.get()).applications_selected }
}

fn set_applications_selected(value: usize) {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupt-serialized write of a plain counter.
    unsafe {
        (*DESKTOP.0.get()).applications_selected = value;
    }
}

/// Marks only the Linux window for the next frame.
///
/// Linux presents are the highest-rate damage source. Keeping them separate
/// from the clock and diagnostics avoids unioning the menu bar and every
/// status window into each frame's damage rectangle.
pub fn mark_linux_dirty() {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupt-serialized update; IRQs never enter the desktop.
    unsafe {
        (*DESKTOP.0.get()).linux_dirty = true;
    }
}

/// Marks the Linux title bar and frame for repainting.
pub fn mark_linux_frame_dirty() {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupt-serialized update; IRQs never enter the desktop.
    unsafe {
        (*DESKTOP.0.get()).linux_frame_dirty = true;
    }
}

/// Returns compositor pass counts and the last/maximum paint duration in
/// 10 ms ticks (the timer runs at 100 Hz).
#[must_use]
#[cfg(feature = "gate4-gui-test")]
pub fn paint_stats() -> (u64, u64, u64, u64, u64, u64) {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupt-serialized snapshot; IRQs never enter the desktop.
    unsafe {
        let state = &*DESKTOP.0.get();
        (
            state.scene_paints,
            state.cursor_paints,
            state.last_paint_ticks,
            state.max_paint_ticks,
            state.fast_paints,
            state.slow_paints,
        )
    }
}

/// Clears the paint-duration high-water mark.
///
/// Gate 4 calls this when the Linux window opens so the summary describes
/// interactive application frames, not the boot desktop paint.
pub fn reset_paint_stats() {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupt-serialized update; IRQs never enter the desktop.
    unsafe {
        let state = &mut *DESKTOP.0.get();
        state.scene_paints = 0;
        state.cursor_paints = 0;
        state.last_paint_ticks = 0;
        state.max_paint_ticks = 0;
        state.last_paint_us = 0;
        state.max_paint_us = 0;
        state.total_paint_us = 0;
        state.fast_paints = 0;
        state.slow_paints = 0;
    }
}

/// Returns true when a compositor pass has pending damage.
///
/// The timer path uses this to give the idle/compositor task one bounded
/// slice after a present instead of letting a continuously rendering guest
/// starve presentation until its next block.
pub fn frame_pending() -> bool {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupt-serialized snapshot; IRQs never enter the desktop.
    unsafe {
        let state = &*DESKTOP.0.get();
        state.wm.is_some()
            && (state.linux_dirty
                || state.linux_frame_dirty
                || state.clock_dirty
                || state.live_dirty
                || state.files_dirty)
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
    paint_frame(&wm, Damage::from_rect(wm.screen()));
}

fn paint_frame(wm: &Wm, mut damage: Damage) {
    let started = scheduler::ticks();
    let started_tsc = timer::tsc();
    let palette = wm.palette();
    let glass = effective_glass(wm.settings());
    let screen = wm.screen();
    graphics::clear_clip();
    graphics::clear_damage();
    let old_cursor = graphics::restore_cursor_background();
    // SAFETY: The idle task is the only painter; no mutable reference escapes.
    let theme = unsafe { (*DESKTOP.0.get()).wallpaper_high_contrast };
    let theme_changed = theme != Some(wm.settings().high_contrast);
    // A pure present from an opaque Linux surface rewrites every pixel in its
    // content rectangle. Restoring the wallpaper underneath it first is wasted
    // bandwidth on every keystroke and hover frame.
    let opaque_content = !theme_changed
        && [WindowKind::Linux, WindowKind::Linux2]
            .into_iter()
            .filter_map(|kind| {
                let slot = kind.linux_slot()?;
                let frame = wm.window_frame(wm.window_id(kind)?)?;
                (damage.rects() == [layout::content_rect(frame)]
                    && crate::display::surface_opaque(slot))
                .then_some(())
            })
            .next()
            .is_some();
    // Without a cached wallpaper (first frame or direct mode) every region
    // restore fails alike, so the first rectangle decides.
    let restorable = opaque_content
        || damage.rects().first().is_some_and(|rect| {
            graphics::restore_wallpaper(rect.x, rect.y, rect.width, rect.height)
        });
    if theme_changed || !restorable {
        damage = Damage::from_rect(screen);
        paint_wallpaper(screen, palette, wm.settings().high_contrast);
        graphics::cache_wallpaper();
    }
    // Rectangles may overlap. Each one restores its wallpaper and repaints
    // every layer, so an overlap is painted twice but never blended twice.
    for (index, rect) in damage.rects().iter().enumerate() {
        if index > 0 {
            graphics::restore_wallpaper(rect.x, rect.y, rect.width, rect.height);
        }
        graphics::set_damage(rect.x, rect.y, rect.width, rect.height);
        paint_windows(wm, palette, *rect);
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
    }
    graphics::save_cursor_background(wm.cursor().x, wm.cursor().y);
    paint_cursor(wm.cursor(), palette);
    if damage.rects() == [screen] {
        graphics::present();
    } else {
        for rect in damage.rects() {
            graphics::present_region(rect.x, rect.y, rect.width, rect.height);
        }
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
    let micros = timer::tsc_micros(timer::tsc().saturating_sub(started_tsc), finished);
    let _guard = InterruptGuard::enter();
    // SAFETY: No rendering or service borrow is live during this state update.
    unsafe {
        let state = &mut *DESKTOP.0.get();
        state.painted = Some(*wm);
        state.wallpaper_high_contrast = Some(wm.settings().high_contrast);
        state.files_dirty = false;
        state.linux_dirty = false;
        state.linux_frame_dirty = false;
        state.clock_dirty = false;
        state.live_dirty = false;
        // Pace frame starts, not frame completions: painting already consumes
        // the frame budget and must not be followed by an extra 30 ms delay.
        state.last_paint = started;
        state.scene_paints = state.scene_paints.saturating_add(1);
        state.last_paint_ticks = finished.saturating_sub(started);
        state.max_paint_ticks = state.max_paint_ticks.max(state.last_paint_ticks);
        state.last_paint_us = micros;
        state.max_paint_us = state.max_paint_us.max(micros);
        state.total_paint_us = state.total_paint_us.saturating_add(micros);
        if state.last_paint_ticks < 3 {
            state.fast_paints = state.fast_paints.saturating_add(1);
        } else if state.last_paint_ticks > 10 {
            state.slow_paints = state.slow_paints.saturating_add(1);
        }
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
    sync_network_prompt(&mut wm, now);
    let (events, mouse) = input::drain_mouse_events();
    for event in events.into_iter().flatten().chain(core::iter::once(mouse)) {
        route_pointer(&mut wm, &mut last_buttons, event, now);
    }
    // Feed the focused Linux endpoint. Coordinates are translated to the
    // window's client area here, not by the guest.
    let focused_slot = wm
        .focused_window()
        .and_then(|window| window.kind.linux_slot().map(|slot| (slot, window.id)));
    let cursor = wm.cursor();
    for slot in 0..crate::linux::MAX_APPS {
        let focused = focused_slot.filter(|(focused, _)| *focused == slot);
        let content = focused
            .and_then(|(_, id)| wm.window_frame(id))
            .map(layout::content_rect);
        crate::display::sync_input(
            slot,
            focused.map(|(_, id)| id),
            content,
            cursor.x,
            cursor.y,
            mouse.buttons,
        );
    }

    let second = now / 100;
    let clock_changed = second != last_second;
    if clock_changed {
        // SAFETY: Only the idle task writes desktop state. A clock tick only
        // changes the menu bar; it must not re-blit an idle Linux surface.
        unsafe {
            (*DESKTOP.0.get()).clock_dirty = true;
        }
    }
    store(wm, now, second, last_buttons);
    reconcile_linux_windows(&wm);
    let mut damage = painted.map_or(Damage::from_rect(wm.screen()), |before| {
        wm.damage_region_since(&before)
    });
    let mut include = |rect: Rect| damage.include(rect);
    // SAFETY: Only the idle task writes desktop state; IRQs only enqueue input.
    let (files_dirty, linux_dirty, linux_frame_dirty, live_dirty, clock_dirty) = unsafe {
        (
            (*DESKTOP.0.get()).files_dirty,
            (*DESKTOP.0.get()).linux_dirty,
            (*DESKTOP.0.get()).linux_frame_dirty,
            (*DESKTOP.0.get()).live_dirty,
            (*DESKTOP.0.get()).clock_dirty,
        )
    };
    if files_dirty
        && let Some(id) = wm.window_id(WindowKind::Files)
        && let Some(frame) = wm.window_frame(id)
    {
        include(frame);
    }
    if linux_dirty || linux_frame_dirty {
        for kind in [WindowKind::Linux, WindowKind::Linux2] {
            if let Some(id) = wm.window_id(kind)
                && let Some(frame) = wm.window_frame(id)
            {
                // A pure surface present changes only the content rectangle.
                // Keeping the damage inside it avoids repainting the title
                // bar, body, and two shadow layers for every frame.
                if linux_frame_dirty {
                    include(frame);
                } else {
                    include(layout::content_rect(frame));
                }
            }
        }
    }
    if clock_dirty {
        include(layout::menu_bar(wm.screen()));
    }
    // The Terminal window owns a shell while it exists; opening it from the
    // dock or search starts one and closing it ends the session.
    let terminal = wm.window_id(WindowKind::Terminal);
    crate::terminal::sync_window(terminal.is_some());
    if let Some(id) = terminal
        && crate::terminal::needs_paint()
    {
        let shown = wm
            .window(id)
            .is_some_and(|window| window.is_visible() && window.workspace == wm.workspace());
        match wm.window_frame(id) {
            Some(frame) if shown => include(layout::content_rect(frame)),
            // A hidden grid is painted in full when its window is shown again.
            _ => crate::terminal::mark_painted(),
        }
    }
    if live_dirty {
        include(layout::menu_bar(wm.screen()));
        for kind in [
            WindowKind::Security,
            WindowKind::Portal,
            WindowKind::Log,
            WindowKind::Applications,
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
    let damage = damage.clipped(wm.screen());
    if !damage.is_empty() {
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
                .is_some_and(|window| window.kind == WindowKind::Applications)
            && let Some(frame) = wm.window_frame(id)
        {
            applications_click(layout::content_rect(frame), point);
        } else if let aether_wm::Hit::Window(id) = hit
            && wm
                .window(id)
                .is_some_and(|window| window.kind == WindowKind::Security)
            && let Some(frame) = wm.window_frame(id)
        {
            network_click(layout::content_rect(frame), point, now);
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
    let global_shortcut =
        matches!(key, Key::Function(3 | 4 | 10)) || (modifiers.command() && key == Key::Char(b'w'));
    let linux_focused = wm
        .focused_window()
        .and_then(|window| window.kind.linux_slot())
        .filter(|_| {
            !global_shortcut
                && !wm.overview_open()
                && !wm.spotlight_open()
                && wm.menu_state().is_none()
        });
    if let Some(slot) = linux_focused {
        crate::display::key(slot, key, modifiers);
        return;
    }
    let applications_focused = wm
        .focused_window()
        .is_some_and(|window| window.kind == WindowKind::Applications)
        && !wm.overview_open()
        && !wm.spotlight_open()
        && wm.menu_state().is_none()
        && !modifiers.command();
    if applications_focused {
        let count = crate::linux::pack_len();
        if count > 0 {
            let columns = wm
                .focused_window()
                .and_then(|window| wm.window_frame(window.id))
                .map(|frame| app_grid(layout::content_rect(frame), count).columns)
                .unwrap_or(4);
            let mut selected = applications_selected().min(count - 1);
            match key {
                Key::Left => selected = selected.saturating_sub(1),
                Key::Right if selected + 1 < count => selected += 1,
                Key::Up => selected = selected.saturating_sub(columns),
                Key::Down if selected + columns < count => selected += columns,
                Key::Enter => {
                    set_applications_selected(selected);
                    launch_craft(selected);
                    return;
                }
                Key::Escape => {
                    if let Some(id) = wm.window_id(WindowKind::Applications) {
                        wm.close_window(id);
                        let now = scheduler::ticks();
                        store(wm, now, now / 100, input::mouse().buttons);
                    }
                    return;
                }
                _ => return,
            }
            set_applications_selected(selected);
            let now = scheduler::ticks();
            store(wm, now, now / 100, input::mouse().buttons);
            mark_live_dirty();
            return;
        }
    }
    if key == Key::Function(4) {
        set_applications_selected(0);
    }
    let terminal_focused = wm
        .focused_window()
        .is_some_and(|window| window.kind == WindowKind::Terminal)
        && !wm.overview_open()
        && !wm.spotlight_open()
        && wm.menu_state().is_none();
    if terminal_focused && !desktop_shortcut(key, modifiers) {
        crate::terminal::key(key, modifiers);
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

/// Returns `true` for the keys the desktop keeps while the Terminal has focus:
/// window and Space switching and search. Everything else, Ctrl letters
/// included, belongs to the terminal program.
fn desktop_shortcut(key: Key, modifiers: Modifiers) -> bool {
    if modifiers.super_key || modifiers.alt {
        return true;
    }
    modifiers.ctrl
        && matches!(
            key,
            Key::Char(b' ' | b'1'..=b'4') | Key::Tab | Key::Left | Key::Right | Key::Up
        )
}

/// Opens (or focuses) the Terminal window. Used by the console's `term open`.
pub fn open_terminal() {
    let Some(mut wm) = current() else {
        return;
    };
    if wm.open(WindowKind::Terminal).is_some() {
        wm.finish_animations();
        let now = scheduler::ticks();
        store(wm, now, now / 100, input::mouse().buttons);
    }
    crate::terminal::sync_window(true);
}

/// Closes the Terminal window, ending its session.
pub fn close_terminal() {
    let Some(mut wm) = current() else {
        return;
    };
    if let Some(id) = wm.window_id(WindowKind::Terminal) {
        wm.close_window(id);
        let now = scheduler::ticks();
        store(wm, now, now / 100, input::mouse().buttons);
    }
    crate::terminal::sync_window(false);
}

/// Opens or resizes the single Linux surface window.
///
/// The frame is sized so its client area matches the requested surface within
/// the compositor's work area. The surface is scaled by the painter when the
/// frame is clamped smaller. Returns `None` when no desktop exists.
fn linux_kind(slot: usize) -> WindowKind {
    if slot == 0 {
        WindowKind::Linux
    } else {
        WindowKind::Linux2
    }
}

pub fn open_linux_window(slot: usize, width: u32, height: u32, title: &str) -> Option<u8> {
    let _ = title;
    let mut wm = current()?;
    let id = wm.open(linux_kind(slot))?;
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
    reset_paint_stats();
    mark_linux_frame_dirty();
    Some(id)
}

/// Retires the Linux window when the endpoint closes or the task exits.
pub fn close_linux_window(slot: usize, id: u8) {
    let Some(mut wm) = current() else {
        return;
    };
    let kind = linux_kind(slot);
    if wm.window(id).is_none_or(|window| window.kind != kind) {
        return;
    }
    wm.close_window(id);
    let now = scheduler::ticks();
    store(wm, now, now / 100, input::mouse().buttons);
    mark_linux_frame_dirty();
}

/// Tells an app to exit when its compositor window has been closed.
///
/// Closing a window is the user's request to quit that app, matching the dock
/// taskbar model. The app gets one close event through the display endpoint;
/// when its task exits the domain is released for a later launch.
fn reconcile_linux_windows(wm: &Wm) {
    for slot in 0..crate::linux::MAX_APPS {
        if let Some(id) = crate::display::window_id(slot)
            && wm.window(id).is_none()
        {
            crate::display::request_close(slot);
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
        let mut trash_buffer = [0u8; 16];
        let trashed = deleted_original
            && fs::read_path("/home/Trash/mutate.txt", &mut trash_buffer).is_ok()
            && fs::read_path("/home/Trash/renamed.txt", &mut trash_buffer).is_ok();
        let mut trash = Browser::new();
        let trash_nav = trashed && trash.navigate("/home/Trash", &mut fs::BrowserFiles);
        let trash_select = trash_nav && trash.select_name("mutate.txt");
        let restored = if trash_select {
            trash.open_selected(&mut fs::BrowserFiles);
            fs::read_path(MUTATE_PATH, &mut trash_buffer).is_ok()
                && fs::read_path("/home/Trash/mutate.txt", &mut trash_buffer).is_err()
        } else {
            false
        };
        mutate_ok = navigated
            && selected
            && duplicated
            && copied
            && renamed
            && reselected
            && deleted_copy
            && original
            && deleted_original
            && trashed
            && restored;
        if !mutate_ok {
            serial_line!(
                "desktop: mutation steps nav={navigated} select={selected} copy={duplicated} \
                 copy_select={copied} rename={renamed} select2={reselected} delete_copy={deleted_copy} \
                 select3={original} delete={deleted_original} trash={trashed} restore={restored} \
                 error={:?}",
                files.error()
            );
        }
        let _ = fs::remove_path(MUTATE_PATH);
        let _ = fs::purge_trash_path("/home/Trash/renamed.txt");
    }
    if mutate_ok {
        serial_line!("AETHER_FILES_MUTATE_OK copy=1 rename=1 delete=1 trash=1 restore=1");
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

/// Self-test for persisted settings across a reboot
/// (`settings-persist-selftest`).
///
/// The unified filesystem on this platform is a bring-up RAM disk, so its
/// contents cannot survive a power cycle; a reboot is therefore simulated the
/// way this codebase simulates one (see the rollback self-test): re-run the
/// exact boot sequence — a fresh window manager with default settings, as
/// `Wm::new` gives at boot, then the before-first-frame settings load. The
/// saved record must win, so the user's accessibility choices are applied
/// before any frame is painted.
#[cfg(feature = "settings-persist-selftest")]
pub fn settings_persist_self_test() -> Result<(), aether_fs::FsError> {
    let Some(live) = current() else {
        serial_line!("AETHER_BOOT_FAILED reason=settings-persist not-initialized");
        return Err(aether_fs::FsError::Corrupt);
    };
    let mut wm = live;
    // Change accessibility settings through the same toggle path the
    // settings window and menus use.
    wm.toggle_setting(1);
    wm.toggle_setting(0);
    let expected = wm.settings();
    if !expected.high_contrast || !expected.reduce_motion {
        serial_line!("AETHER_BOOT_FAILED reason=settings-persist preconditions");
        return Err(aether_fs::FsError::Corrupt);
    }
    // Store the changed state; `store` is the single choke point that flushes
    // the dirty settings to the volume, which is the real production path.
    let now = scheduler::ticks();
    store(wm, now, now / 100, input::mouse().buttons);
    // The record must be on the volume, independently of the read-back used
    // by the simulated reboot.
    let mut stored = [0u8; aether_wm::SETTINGS_RECORD_LEN];
    let Ok(length) = fs::read_path(crate::settings::SETTINGS_PATH, &mut stored) else {
        serial_line!("AETHER_BOOT_FAILED reason=settings-persist write");
        return Err(aether_fs::FsError::Corrupt);
    };
    if Settings::parse(&stored[..length]) != Ok(expected) {
        serial_line!("AETHER_BOOT_FAILED reason=settings-persist record");
        return Err(aether_fs::FsError::Corrupt);
    }
    // Simulate the reboot: fresh in-memory state, same volume, boot load.
    let mut booted = Wm::new(live.screen());
    crate::settings::load(&mut booted);
    if booted.settings() != expected {
        serial_line!("AETHER_BOOT_FAILED reason=settings-persist not-applied");
        return Err(aether_fs::FsError::Corrupt);
    }
    serial_line!(
        "AETHER_SETTINGS_PERSIST_OK high_contrast={} reduce_motion={} glass={} magnify_dock={} across_reboot=1",
        expected.high_contrast,
        expected.reduce_motion,
        expected.glass.label(),
        expected.magnify_dock
    );
    Ok(())
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
        "desktop: render scene={} cursor={} last_ms={} max_ms={} last_us={} max_us={} total_us={}",
        state.scene_paints,
        state.cursor_paints,
        state.last_paint_ticks * 10,
        state.max_paint_ticks * 10,
        state.last_paint_us,
        state.max_paint_us,
        state.total_paint_us
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

/// Bounds of the two-layer window shadow. Used to test whether a window can
/// contribute to a damage rectangle before walking its surface.
const SHADOW_MARGIN: i32 = 24;

fn paint_windows(wm: &Wm, palette: aether_wm::Palette, damage: Rect) {
    let screen = wm.screen();
    for id in wm.order() {
        let Some(window) = wm.window(*id) else {
            continue;
        };
        if !window.is_visible() {
            continue;
        }
        let Some(frame) = wm.window_frame(*id) else {
            continue;
        };
        // The wallpaper restore already cleared the damage region. A window
        // whose frame and shadow lie outside it has nothing to repaint, so do
        // not walk its full surface just to have every pixel clipped.
        let shadow = Rect::new(
            frame.x - SHADOW_MARGIN,
            frame.y - SHADOW_MARGIN,
            frame.width + SHADOW_MARGIN * 2,
            frame.height + SHADOW_MARGIN * 2,
        );
        if !shadow.intersects(damage) {
            continue;
        }
        paint_window(
            wm,
            window,
            frame,
            wm.focus_id() == Some(*id),
            palette,
            damage,
        );
    }
    if wm.transition().active {
        let label = workspace_label(wm.workspace());
        let pill = layout::space_pill(screen);
        draw_pill(
            pill.x,
            pill.y,
            pill.width,
            pill.height,
            palette,
            label,
            palette.accent,
        );
    }
}

fn paint_window(
    wm: &Wm,
    window: &Window,
    frame: Rect,
    focused: bool,
    palette: aether_wm::Palette,
    damage: Rect,
) {
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
    let content = layout::content_rect(frame);
    let linux_opaque = window
        .kind
        .linux_slot()
        .is_some_and(crate::display::surface_opaque);
    // Every pixel of the content rectangle is painted opaquely below (a solid
    // fill or an opaque Linux surface), except in the bottom rows that the
    // rounded silhouette trims. Shadow and body blends under that core are
    // overwritten in the same frame, so they skip it.
    let silhouette_radius = radius.min(frame.width / 2).min(frame.height / 2);
    let core_bottom = content.bottom().min(frame.bottom() - silhouette_radius);
    let core_top = content.y.max(frame.y + silhouette_radius);
    let core = (
        content.x,
        core_top,
        content.width,
        (core_bottom - core_top).max(0),
    );

    // Layered shadow: darker nearest the frame, softer further away. Omit it
    // for the window under a drag/resize gesture and during a Spaces slide:
    // the two translucent layers are a third of a window's cost, and the
    // window is moving too fast to read the shadow. The gesture or slide end
    // damages the whole frame, so the shadow returns. Windows the gesture
    // only uncovers keep theirs, since nothing repaints them afterwards.
    let moving = wm.dragging_id() == Some(window.id);
    if !moving && !wm.transition().active {
        for (offset, spread, strength) in [(0, 10, 78u8), (4, 20, 34)] {
            graphics::blend_round_rect_around(
                frame.x - spread / 2,
                frame.y + offset,
                frame.width + spread,
                frame.height + spread / 2,
                radius + spread / 2,
                palette.shadow,
                strength,
                core,
            );
        }
    }

    let body = if focused {
        palette.window
    } else {
        palette.window_inactive
    };
    if moving {
        // An opaque base avoids a full-window alpha blend on every drag step.
        graphics::fill_round_rect(frame.x, frame.y, frame.width, frame.height, radius, body);
    } else {
        graphics::blend_round_rect_around(
            frame.x,
            frame.y,
            frame.width,
            frame.height,
            radius,
            body,
            alpha,
            core,
        );
    }

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

    // Damage that touches only the shadow or title bar, such as a hover over
    // the traffic lights, leaves the content pixels as they are; every pixel
    // would be clipped, but walking a text-heavy view still costs.
    if content.intersects(damage) {
        graphics::set_round_clip(frame.x, frame.y, frame.width, frame.height, radius);
        // An opaque Linux surface is about to overwrite this rectangle completely.
        if !linux_opaque {
            graphics::fill_rect(
                content.x,
                content.y,
                content.width,
                content.height,
                palette.window,
            );
        }
        graphics::set_clip(content.x, content.y, content.width, content.height);
        paint_content(wm, window, content, focused, palette);
        graphics::clear_clip();
        graphics::clear_round_clip();
    }

    let lights = layout::traffic_lights(frame);
    paint_traffic_lights(wm, &lights, focused, palette);
    let title = if let Some(slot) = window.kind.linux_slot() {
        let requested = crate::display::title(slot);
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
        WindowKind::Terminal => crate::terminal::paint(content),
        WindowKind::Linux => crate::display::paint_content(0, content),
        WindowKind::Linux2 => crate::display::paint_content(1, content),
        WindowKind::Applications => paint_applications(wm, content, palette),
    }
}

struct AppGrid {
    columns: usize,
    cell_width: i32,
    cell_height: i32,
    tile: i32,
}

fn app_grid(content: Rect, count: usize) -> AppGrid {
    let columns = if content.width >= 760 {
        4
    } else if content.width >= 520 {
        3
    } else {
        2
    }
    .max(1);
    let rows = count.div_ceil(columns).max(1) as i32;
    let cell_width = (content.width - 40).max(1) / columns as i32;
    let available_height = (content.height - 72).max(1);
    let cell_height = (available_height / rows).clamp(92, 132);
    let tile = (cell_width.min(cell_height) - 36).clamp(42, 84);
    AppGrid {
        columns,
        cell_width,
        cell_height,
        tile,
    }
}

fn application_rect(content: Rect, grid: &AppGrid, index: usize) -> Rect {
    let column = index % grid.columns;
    let row = index / grid.columns;
    let cell_x = content.x + 20 + column as i32 * grid.cell_width;
    let cell_y = content.y + 66 + row as i32 * grid.cell_height;
    Rect::new(
        cell_x + (grid.cell_width - grid.tile) / 2,
        cell_y + 6,
        grid.tile,
        grid.tile,
    )
}

fn applications_click(content: Rect, point: aether_wm::Point) {
    let count = crate::linux::pack_len();
    if count == 0 {
        return;
    }
    let grid = app_grid(content, count);
    for index in 0..count {
        let tile = application_rect(content, &grid, index);
        let target = Rect::new(tile.x - 8, tile.y - 4, tile.width + 16, tile.height + 30);
        if target.contains(point) {
            set_applications_selected(index);
            launch_craft(index);
            return;
        }
    }
}

fn launch_craft(index: usize) {
    if let Some(slot) = crate::linux::running_slot(index) {
        if let Some(mut wm) = current() {
            let kind = linux_kind(slot);
            if wm.window_id(kind).is_some() {
                let _ = wm.open(kind);
                let now = scheduler::ticks();
                store(wm, now, now / 100, input::mouse().buttons);
                mark_linux_frame_dirty();
            } else {
                serial_line!("desktop: craft app index={index} is still closing");
            }
        }
        return;
    }
    match crate::linuxd::request_launch(index) {
        Ok(slot) => serial_line!("desktop: craft launch requested index={index} slot={slot}"),
        Err(error) => serial_line!("desktop: craft launch failed index={index} error={error:?}"),
    }
}

fn paint_applications(wm: &Wm, content: Rect, palette: aether_wm::Palette) {
    graphics::fill_rect(
        content.x,
        content.y,
        content.width,
        content.height,
        palette.window,
    );
    draw_text(
        content.x + 22,
        content.y + 18,
        "APPLICATIONS",
        palette.text,
        2,
    );
    draw_text(
        content.x + 22,
        content.y + 40,
        "Launch Craft apps from here. The dock keeps running apps one click away.",
        palette.muted,
        1,
    );
    let count = crate::linux::pack_len();
    if count == 0 {
        draw_text(
            content.x + 22,
            content.y + 82,
            "This image has no multi-app pack.",
            palette.faint,
            1,
        );
        return;
    }
    let grid = app_grid(content, count);
    let selected = applications_selected().min(count - 1);
    let mut running = 0usize;
    for index in 0..count {
        let tile = application_rect(content, &grid, index);
        icons::craft_tile(index, tile, palette);
        if index == selected {
            graphics::outline_round_rect(
                tile.x - 4,
                tile.y - 4,
                tile.width + 8,
                tile.height + 8,
                tile.width / 4,
                palette.accent,
            );
        }
        if crate::linux::running_slot(index).is_some() {
            running += 1;
            graphics::fill_circle(tile.center().x, tile.bottom() + 7, 3, palette.accent);
        }
        let name = crate::linux::pack_entry_name(index).unwrap_or("?");
        let cell_x = tile.center().x - grid.cell_width / 2;
        draw_text_centered(
            cell_x.max(content.x),
            (cell_x + grid.cell_width).min(content.right()),
            tile.bottom() + 12,
            name,
            palette.text,
            1,
        );
    }
    let footer = alloc::format!(
        "{count} Craft apps · {running} running · {} app domains",
        crate::linux::MAX_APPS
    );
    draw_text(
        content.x + 22,
        content.bottom() - 24,
        &footer,
        palette.faint,
        1,
    );
    let _ = wm;
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
    let network = network_layout(content);
    let top = network
        .prompt
        .map_or(banner.bottom(), |prompt| prompt.panel.bottom())
        + 10;
    let available = network.section.y - 12 - top;
    // Show as many card rows as fit at a legible height; the network prompt
    // takes priority over the counters on a small window.
    let rows = ((available + gap) / (38 + gap)).clamp(0, 3);
    let card_height = if rows == 0 {
        0
    } else {
        (available - gap * (rows - 1)) / rows
    };
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

    for (index, (label, value)) in cards.iter().enumerate().take(rows as usize * columns) {
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
    paint_network(&network, palette);
}

const NET_ROWS: usize = 3;
const NET_ROW_HEIGHT: i32 = 22;
const NET_PROMPT_HEIGHT: i32 = 78;
const NET_BUTTON_WIDTH: i32 = 64;
/// Allow ignores clicks for this long after the prompt appears (500 ms), so
/// a click aimed at whatever was under the pointer cannot answer it.
const NET_PROMPT_ARM_TICKS: u64 = 50;

#[derive(Clone, Copy)]
struct AppLabel {
    id: [u8; aether_net::consent::MAX_ID],
    len: usize,
}

impl AppLabel {
    fn new(app: &str) -> Self {
        let mut id = [0u8; aether_net::consent::MAX_ID];
        let len = app.len().min(id.len());
        id[..len].copy_from_slice(&app.as_bytes()[..len]);
        Self { id, len }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.id[..self.len]).unwrap_or("")
    }
}

#[derive(Clone, Copy)]
struct NetworkPrompt {
    app: AppLabel,
    panel: Rect,
    allow: Rect,
    deny: Rect,
}

#[derive(Clone, Copy)]
struct NetworkRow {
    app: AppLabel,
    allowed: bool,
    row: Rect,
    button: Rect,
}

/// Security Center's network area: the pending first-use prompt under the
/// banner and the per-app list at the bottom. The list has a fixed height so
/// rows never move under the pointer when an answer is added.
struct NetworkLayout {
    prompt: Option<NetworkPrompt>,
    section: Rect,
    rows: [Option<NetworkRow>; NET_ROWS],
    more: usize,
}

fn network_layout(content: Rect) -> NetworkLayout {
    let mut pending = [0u8; aether_net::consent::MAX_ID];
    let prompt = consent::pending(&mut pending).map(|len| {
        let panel = Rect::new(
            content.x + 14,
            content.y + 56,
            content.width - 28,
            NET_PROMPT_HEIGHT,
        );
        let allow = Rect::new(
            panel.x + 10,
            panel.bottom() - PORTAL_BUTTON_HEIGHT - 8,
            PORTAL_BUTTON_WIDTH,
            PORTAL_BUTTON_HEIGHT,
        );
        let deny = Rect::new(
            allow.right() + 8,
            allow.y,
            PORTAL_BUTTON_WIDTH,
            PORTAL_BUTTON_HEIGHT,
        );
        NetworkPrompt {
            app: AppLabel::new(core::str::from_utf8(&pending[..len]).unwrap_or("")),
            panel,
            allow,
            deny,
        }
    });
    let height = 18 + NET_ROWS as i32 * NET_ROW_HEIGHT + 12;
    let section = Rect::new(
        content.x + 14,
        content.bottom() - 8 - height,
        content.width - 28,
        height,
    );
    let mut rows = [None; NET_ROWS];
    let mut count = 0;
    let mut more = 0;
    consent::for_each(|app, allowed| {
        if count == NET_ROWS {
            more += 1;
            return;
        }
        let y = section.y + 18 + count as i32 * NET_ROW_HEIGHT;
        let row = Rect::new(section.x, y, section.width, NET_ROW_HEIGHT - 4);
        let button = Rect::new(
            row.right() - NET_BUTTON_WIDTH,
            y,
            NET_BUTTON_WIDTH,
            row.height,
        );
        rows[count] = Some(NetworkRow {
            app: AppLabel::new(app),
            allowed,
            row,
            button,
        });
        count += 1;
    });
    NetworkLayout {
        prompt,
        section,
        rows,
        more,
    }
}

/// Opens Security Center when an application first asks for the network,
/// and repaints it whenever a request or an answer changes.
fn sync_network_prompt(wm: &mut Wm, now: u64) {
    let generation = consent::generation();
    let pending = consent::pending(&mut [0u8; aether_net::consent::MAX_ID]).is_some();
    let (seen, shown) = {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupt-serialized read; IRQs never enter the desktop.
        let state = unsafe { &*DESKTOP.0.get() };
        (state.network_generation, state.network_prompt)
    };
    if generation == seen {
        return;
    }
    let shown = match (pending, shown) {
        (true, None) => {
            let _ = wm.open(WindowKind::Security);
            serial_line!("desktop: network consent prompt shown");
            Some(now)
        }
        (true, Some(shown)) => Some(shown),
        (false, _) => None,
    };
    {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupt-serialized update; IRQs never enter the desktop.
        unsafe {
            (*DESKTOP.0.get()).network_generation = generation;
            (*DESKTOP.0.get()).network_prompt = shown;
        }
    }
    mark_live_dirty();
}

fn network_click(content: Rect, point: aether_wm::Point, now: u64) {
    let network = network_layout(content);
    if let Some(prompt) = network.prompt {
        let app = prompt.app.as_str();
        if prompt.deny.contains(point) {
            consent::decide(app, false);
            return;
        }
        if prompt.allow.contains(point) {
            let shown = {
                let _guard = InterruptGuard::enter();
                // SAFETY: Interrupt-serialized read.
                unsafe { (*DESKTOP.0.get()).network_prompt }
            };
            if shown.is_none_or(|shown| now.saturating_sub(shown) < NET_PROMPT_ARM_TICKS) {
                serial_line!("desktop: network allow ignored; prompt just appeared");
                return;
            }
            consent::decide(app, true);
            return;
        }
    }
    for row in network.rows.iter().flatten() {
        if row.button.contains(point) {
            consent::decide(row.app.as_str(), !row.allowed);
            return;
        }
    }
}

fn paint_network(network: &NetworkLayout, palette: aether_wm::Palette) {
    if let Some(prompt) = network.prompt {
        let panel = prompt.panel;
        graphics::fill_round_rect(panel.x, panel.y, panel.width, panel.height, 8, palette.card);
        graphics::outline_round_rect(
            panel.x,
            panel.y,
            panel.width,
            panel.height,
            8,
            palette.accent,
        );
        draw_text(
            panel.x + 10,
            panel.y + 8,
            "NETWORK REQUEST",
            palette.accent,
            1,
        );
        draw_text_fit(
            panel.x + 10,
            panel.y + 21,
            prompt.app.as_str(),
            panel.width - 20,
            palette.text,
            1,
        );
        draw_text_fit(
            panel.x + 10,
            panel.y + 33,
            "WANTS TO CONNECT TO THE INTERNET",
            panel.width - 20,
            palette.muted,
            1,
        );
        let allow = prompt.allow;
        graphics::fill_round_rect(
            allow.x,
            allow.y,
            allow.width,
            allow.height,
            6,
            palette.accent,
        );
        draw_text_centered(
            allow.x,
            allow.right(),
            allow.y + 5,
            "ALLOW",
            palette.window,
            1,
        );
        let deny = prompt.deny;
        graphics::fill_round_rect(deny.x, deny.y, deny.width, deny.height, 6, palette.card);
        graphics::outline_round_rect(deny.x, deny.y, deny.width, deny.height, 6, palette.warning);
        draw_text_centered(deny.x, deny.right(), deny.y + 5, "DENY", palette.warning, 1);
    }

    let section = network.section;
    graphics::fill_rect(section.x, section.y - 6, section.width, 1, palette.divider);
    draw_text(section.x, section.y, "NETWORK ACCESS", palette.accent, 1);
    let mut status = [0u8; 32];
    let length = match net::with(|net| net.address()) {
        None => copy_label(&mut status, "OFFLINE"),
        Some(None) => copy_label(&mut status, "NO ADDRESS YET"),
        Some(Some(address)) => {
            let mut length = 0;
            for (index, octet) in address.octets().iter().enumerate() {
                if index > 0 {
                    status[length] = b'.';
                    length += 1;
                }
                length += format_u64(&mut status[length..], u64::from(*octet));
            }
            length
        }
    };
    let status_width =
        graphics::text_width(core::str::from_utf8(&status[..length]).unwrap_or(""), 1);
    draw_text_bytes(
        section.right() - status_width,
        section.y,
        &status[..length],
        palette.muted,
        1,
    );
    if network.rows[0].is_none() {
        draw_text(
            section.x + 2,
            section.y + 22,
            "NO APP HAS ASKED FOR THE NETWORK",
            palette.faint,
            1,
        );
    }
    for row in network.rows.iter().flatten() {
        let (state, color, action) = if row.allowed {
            ("ALLOWED", palette.success, "BLOCK")
        } else {
            ("BLOCKED", palette.warning, "ALLOW")
        };
        let state_width = graphics::text_width(state, 1);
        draw_text_fit(
            row.row.x + 2,
            row.row.y + 5,
            row.app.as_str(),
            row.button.x - row.row.x - state_width - 16,
            palette.text,
            1,
        );
        draw_text(
            row.button.x - state_width - 8,
            row.row.y + 5,
            state,
            color,
            1,
        );
        let button = row.button;
        graphics::fill_round_rect(
            button.x,
            button.y,
            button.width,
            button.height,
            6,
            palette.card,
        );
        graphics::outline_round_rect(
            button.x,
            button.y,
            button.width,
            button.height,
            6,
            palette.divider,
        );
        draw_text_centered(
            button.x,
            button.right(),
            button.y + 5,
            action,
            palette.text,
            1,
        );
    }
    if network.more > 0 {
        let mut text = [0u8; 40];
        let mut length = copy_label(&mut text, "+");
        length += format_u64(&mut text[length..], network.more as u64);
        length += copy_label(&mut text[length..], " MORE (CONSOLE: NET)");
        draw_text_bytes(
            section.x + 2,
            section.y + 18 + NET_ROWS as i32 * NET_ROW_HEIGHT,
            &text[..length],
            palette.faint,
            1,
        );
    }
}

fn copy_label(out: &mut [u8], label: &str) -> usize {
    let length = label.len().min(out.len());
    out[..length].copy_from_slice(&label.as_bytes()[..length]);
    length
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
        "SETTINGS ARE SAVED AUTOMATICALLY",
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
                let craft_running = *kind == WindowKind::Applications
                    && [WindowKind::Linux, WindowKind::Linux2]
                        .into_iter()
                        .any(|kind| wm.is_running(kind));
                let craft_active = *kind == WindowKind::Applications
                    && wm
                        .focused_window()
                        .is_some_and(|window| window.kind.linux_slot().is_some());
                if wm.is_running(*kind) || craft_running {
                    graphics::fill_circle(
                        rect.center().x,
                        panel.bottom() - 5,
                        2,
                        if wm.is_active(*kind) || craft_active {
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
