// SPDX-License-Identifier: Apache-2.0
//! Host tests for the window-manager model.
//!
//! These tests cover the rules that are expensive to debug in a boot test:
//! geometry, focus, z-order, workspaces, menus, pointer hit testing, animation
//! completion, and responsive layout.

use crate::anim::{Anim, ease_out_cubic};
use crate::geometry::{Point, Rect};
use crate::layout::{
    self, DOCK_ITEMS, DOCK_SLOTS, DockSlot, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH,
    default_window_frame, dock_panel, overview_cards, traffic_lights, work_area,
};
use crate::model::{Command, Key, MenuKind, Modifiers, WORKSPACES, WindowKind, WindowState, Wm};
use crate::theme::{Palette, ThemeKind};

fn sample() -> Wm {
    let mut wm = Wm::new(Rect::new(0, 0, 1280, 800));
    wm.seed();
    wm
}

fn ctrl() -> Modifiers {
    Modifiers {
        ctrl: true,
        ..Modifiers::default()
    }
}

fn super_m() -> Modifiers {
    Modifiers {
        super_key: true,
        ..Modifiers::default()
    }
}

#[test]
fn geometry_respects_half_open_edges() {
    let rect = Rect::new(10, 20, 30, 40);
    assert!(rect.contains(Point::new(10, 20)));
    assert!(rect.contains(Point::new(39, 59)));
    assert!(!rect.contains(Point::new(40, 59)));
    assert!(!rect.contains(Point::new(39, 60)));
    assert_eq!(rect.right(), 40);
    assert_eq!(rect.bottom(), 60);
    assert_eq!(rect.center(), Point::new(25, 40));
    assert_eq!(
        rect.intersection(Rect::new(35, 15, 20, 20)),
        Some(Rect::new(35, 20, 5, 15))
    );
    assert!(
        Rect::new(0, 0, 1, 1)
            .intersection(Rect::new(2, 2, 3, 3))
            .is_none()
    );
}

#[test]
fn clamp_keeps_windows_on_screen() {
    let bounds = Rect::new(0, 0, 100, 100);
    let rect = Rect::new(90, 90, 20, 20).clamp_inside(bounds);
    assert_eq!(rect, Rect::new(80, 80, 20, 20));
    let oversized = Rect::new(10, 10, 200, 200).clamp_inside(bounds);
    assert_eq!(oversized, Rect::new(0, 0, 200, 200));
}

#[test]
fn lerp_is_endpoint_exact() {
    let first = Rect::new(0, 0, 100, 100);
    let second = Rect::new(100, 50, 200, 300);
    assert_eq!(first.lerp(second, 0), first);
    assert_eq!(first.lerp(second, 256), second);
    assert_eq!(first.lerp(second, 128), Rect::new(50, 25, 150, 200));
}

#[test]
fn animation_advances_by_elapsed_ticks() {
    let mut anim = Anim::starting(10);
    assert!(!anim.is_complete());
    assert!(anim.advance(5));
    assert_eq!(anim.value(), 130);
    assert!(anim.advance(5));
    assert!(anim.is_complete());
    assert_eq!(anim.value(), 256);
    assert!(!anim.advance(1));
    anim.retarget(0, 4);
    assert!(anim.advance(4));
    assert_eq!(anim.value(), 0);
}

#[test]
fn zero_duration_animations_complete_without_waiting_for_a_tick() {
    let mut anim = Anim::starting(0);
    assert_eq!(anim.value(), 256);
    assert!(anim.is_complete());
    anim.retarget(0, 0);
    anim.restart(0);
    assert_eq!(anim.value(), 256);
    assert!(!anim.is_active());
}

#[test]
fn easing_is_monotonic_and_bounded() {
    let mut previous = 0;
    for value in 0..=256u16 {
        let eased = ease_out_cubic(value);
        assert!(eased >= previous);
        assert!(eased <= 256);
        previous = eased;
    }
    assert_eq!(ease_out_cubic(0), 0);
    assert_eq!(ease_out_cubic(256), 256);
}

#[test]
fn seed_opens_three_windows_and_focuses_welcome() {
    let wm = sample();
    assert_eq!(wm.window_count(), 3);
    assert_eq!(wm.workspace(), 0);
    assert_eq!(wm.focus_id(), wm.window_id(WindowKind::Welcome));
    assert_eq!(wm.focus_title(), "WELCOME");
    assert_eq!(wm.order().len(), 3);
}

#[test]
fn opening_an_existing_app_focuses_it_instead_of_duplicating() {
    let mut wm = sample();
    let files = wm.window_id(WindowKind::Files).unwrap();
    let id = wm.open(WindowKind::Files).unwrap();
    assert_eq!(id, files);
    assert_eq!(wm.window_count(), 3);
    assert_eq!(wm.focus_id(), Some(files));
}

#[test]
fn windows_are_brought_to_front_on_focus() {
    let mut wm = sample();
    let files = wm.window_id(WindowKind::Files).unwrap();
    wm.focus_window(files);
    assert_eq!(wm.order().last(), Some(&files));
    let welcome = wm.window_id(WindowKind::Welcome).unwrap();
    wm.focus_window(welcome);
    assert_eq!(wm.order().last(), Some(&welcome));
}

#[test]
fn focus_cycle_skips_minimized_windows() {
    let mut wm = sample();
    let welcome = wm.window_id(WindowKind::Welcome).unwrap();
    wm.minimize_window(welcome);
    wm.tick(30);
    assert_ne!(wm.focus_id(), Some(welcome));
    let seen: Vec<u8> = wm.order().to_vec();
    assert!(seen.contains(&welcome));
    wm.focus_next(1);
    assert_ne!(wm.focus_id(), Some(welcome));
    assert_eq!(
        wm.window(welcome).map(|window| window.state),
        Some(WindowState::Minimized)
    );
    let id = wm.open(WindowKind::Welcome).unwrap();
    assert_eq!(id, welcome);
    assert_eq!(wm.focus_id(), Some(welcome));
    assert_eq!(
        wm.window(welcome).map(|window| window.state),
        Some(WindowState::Normal)
    );
}

#[test]
fn close_animates_then_reaps_the_slot() {
    let mut wm = sample();
    let welcome = wm.window_id(WindowKind::Welcome).unwrap();
    wm.close_window(welcome);
    assert!(wm.window(welcome).is_some_and(|window| window.closing));
    for _ in 0..20 {
        wm.tick(1);
    }
    assert!(wm.window(welcome).is_none());
    assert_eq!(wm.window_count(), 2);
    assert!(!wm.order().contains(&welcome));
    assert!(wm.focus_id().is_some());
}

#[test]
fn reopening_a_closing_window_cancels_its_close() {
    let mut wm = sample();
    let welcome = wm.window_id(WindowKind::Welcome).unwrap();
    wm.close_window(welcome);
    assert!(wm.window(welcome).is_some_and(|window| window.closing));
    let reopened = wm.open(WindowKind::Welcome).unwrap();
    assert_eq!(reopened, welcome);
    assert!(wm.window(welcome).is_some_and(|window| !window.closing));
    assert_eq!(wm.focus_id(), Some(welcome));
}

#[test]
fn zoom_restores_the_previous_frame() {
    let mut wm = sample();
    let files = wm.window_id(WindowKind::Files).unwrap();
    wm.focus_window(files);
    let before = wm.window(files).unwrap().frame;
    wm.zoom_window(files);
    let maximized = wm.window(files).unwrap();
    assert_eq!(maximized.state, WindowState::Maximized);
    assert!(maximized.frame.width > before.width);
    assert!(
        work_area(wm.screen()).contains(Point::new(maximized.frame.x + 1, maximized.frame.y + 1))
    );
    wm.zoom_window(files);
    assert_eq!(wm.window(files).unwrap().frame, before);
    assert_eq!(wm.window(files).unwrap().state, WindowState::Normal);
}

#[test]
fn workspace_switch_moves_focus_and_preserves_windows() {
    let mut wm = sample();
    let welcome = wm.window_id(WindowKind::Welcome).unwrap();
    wm.switch_workspace(2);
    assert_eq!(wm.workspace(), 2);
    assert_eq!(wm.workspace_window_count(2), 0);
    assert!(wm.transition().active);
    for _ in 0..30 {
        wm.tick(1);
    }
    assert!(!wm.transition().active);
    assert_eq!(wm.focus_id(), None);
    let portal = wm.open(WindowKind::Portal).unwrap();
    assert_eq!(wm.window(portal).unwrap().workspace, 2);
    wm.switch_workspace(0);
    for _ in 0..30 {
        wm.tick(1);
    }
    assert_eq!(wm.workspace(), 0);
    assert_eq!(wm.focus_id(), Some(welcome));
    assert_eq!(wm.workspace_window_count(2), 1);
}

#[test]
fn workspace_cycling_wraps_both_directions() {
    let mut wm = sample();
    wm.cycle_workspace(-1);
    assert_eq!(wm.workspace(), WORKSPACES - 1);
    wm.cycle_workspace(1);
    assert_eq!(wm.workspace(), 0);
}

#[test]
fn overview_keyboard_selects_a_space() {
    let mut wm = sample();
    wm.toggle_overview();
    assert!(wm.overview_open());
    let outcome = wm.key(Key::Right, Modifiers::default());
    assert!(outcome.changed);
    assert_eq!(wm.overview_selection(), 1);
    let outcome = wm.key(Key::Enter, Modifiers::default());
    assert!(outcome.changed);
    assert_eq!(wm.workspace(), 1);
    assert!(!wm.overview_open());
}

#[test]
fn menu_selection_skips_separators_and_executes() {
    let mut wm = sample();
    let logo = layout::menu_logo_rect(wm.screen()).center();
    wm.pointer_down(logo, 0);
    assert_eq!(
        wm.menu_state().map(|menu| menu.kind),
        Some(MenuKind::System)
    );
    let selected = wm.menu_state().unwrap().selected;
    assert!(!MenuKind::System.items()[selected].is_separator());
    wm.key(Key::Down, Modifiers::default());
    assert!(
        !MenuKind::System
            .items()
            .get(wm.menu_state().unwrap().selected)
            .unwrap()
            .is_separator()
    );
    let outcome = wm.key(Key::Escape, Modifiers::default());
    assert!(outcome.changed);
    assert!(wm.menu_state().is_none());
}

#[test]
fn menu_title_opens_the_matching_menu() {
    let mut wm = sample();
    let titles = layout::menu_title_rects(wm.screen(), wm.app_name());
    wm.pointer_down(titles[1].center(), 0);
    assert_eq!(wm.menu_state().map(|menu| menu.kind), Some(MenuKind::File));
}

#[test]
fn super_chords_control_windows_and_spaces() {
    let mut wm = sample();
    let welcome = wm.window_id(WindowKind::Welcome).unwrap();
    wm.key(Key::Char(b'2'), super_m());
    assert_eq!(wm.workspace(), 1);
    wm.key(Key::Char(b'1'), super_m());
    assert_eq!(wm.workspace(), 0);
    wm.focus_window(welcome);
    wm.key(Key::Char(b'm'), super_m());
    assert_eq!(wm.window(welcome).unwrap().state, WindowState::Minimized);
    wm.open(WindowKind::Welcome);
    wm.key(Key::Char(b'f'), super_m());
    assert_eq!(wm.window(welcome).unwrap().state, WindowState::Maximized);
    wm.key(Key::Char(b'f'), super_m());
    assert_eq!(wm.window(welcome).unwrap().state, WindowState::Normal);
    wm.key(Key::Char(b'w'), super_m());
    assert!(wm.window(welcome).unwrap().closing);
}

#[test]
fn plain_ctrl_and_alt_chords_no_longer_control_the_window_manager() {
    let mut wm = sample();
    let id = wm.focus_id().unwrap();
    let before = wm.window(id).unwrap().frame;
    // Ctrl used to grab window and Space switching; with Super as the only
    // system modifier these must no-op, so they reach the focused app.
    wm.key(Key::Char(b'w'), ctrl());
    assert!(!wm.window(id).unwrap().closing);
    wm.key(Key::Char(b'm'), ctrl());
    assert_eq!(wm.window(id).unwrap().state, WindowState::Normal);
    wm.key(Key::Char(b'q'), ctrl());
    assert!(!wm.window(id).unwrap().closing);
    wm.key(Key::Char(b' '), ctrl());
    assert!(!wm.spotlight_open());
    wm.key(Key::Char(b'2'), ctrl());
    assert_eq!(wm.workspace(), 0);
    wm.key(Key::Right, ctrl());
    wm.key(
        Key::Right,
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(wm.window(id).unwrap().frame, before);
    // Alt alone is never a system modifier either.
    let mods = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    wm.key(Key::Char(b'w'), mods);
    assert!(!wm.window(id).unwrap().closing);
    wm.key(Key::Right, mods);
    assert_eq!(wm.window(id).unwrap().frame, before);
}

#[test]
fn super_tab_cycles_focus_in_both_directions() {
    let mut wm = sample();
    let first = wm.focus_id();
    wm.key(Key::Tab, super_m());
    let second = wm.focus_id();
    assert!(second.is_some());
    assert_ne!(first, second);
    let mods = Modifiers {
        super_key: true,
        shift: true,
        ..Modifiers::default()
    };
    wm.key(Key::Tab, mods);
    assert_eq!(wm.focus_id(), first);
}

#[test]
fn super_backtick_cycles_focus_forward() {
    let mut wm = sample();
    let first = wm.focus_id();
    wm.key(Key::Char(b'`'), super_m());
    assert_ne!(first, wm.focus_id());
}

#[test]
fn super_arrows_move_the_focused_window_without_resizing() {
    let mut wm = sample();
    let id = wm.focus_id().unwrap();
    let before = wm.window(id).unwrap().frame;
    wm.key(Key::Left, super_m());
    let moved = wm.window(id).unwrap().frame;
    assert_eq!(moved.x, before.x - 8);
    assert_eq!(moved.y, before.y);
    assert_eq!((moved.width, moved.height), (before.width, before.height));
    wm.key(Key::Down, super_m());
    let moved = wm.window(id).unwrap().frame;
    assert_eq!(moved.y, before.y + 8);
    assert_eq!((moved.width, moved.height), (before.width, before.height));
}

#[test]
fn super_shift_arrows_resize_the_focused_window() {
    let mut wm = sample();
    let id = wm.focus_id().unwrap();
    let before = wm.window(id).unwrap().frame;
    let mut mods = super_m();
    mods.shift = true;
    wm.key(Key::Right, mods);
    assert_eq!(wm.window(id).unwrap().frame.width, before.width + 8);
    wm.key(Key::Up, mods);
    let frame = wm.window(id).unwrap().frame;
    assert_eq!(frame.height, before.height - 8);
    wm.key(Key::Down, mods);
    assert_eq!(wm.window(id).unwrap().frame.height, before.height);
    wm.key(Key::Left, mods);
    assert_eq!(wm.window(id).unwrap().frame.width, before.width);
}

#[test]
fn super_alt_arrows_cycle_spaces_while_plain_arrows_move() {
    let mut wm = sample();
    assert_eq!(wm.workspace(), 0);
    let mods = Modifiers {
        super_key: true,
        alt: true,
        ..Modifiers::default()
    };
    wm.key(Key::Right, mods);
    assert_eq!(wm.workspace(), 1);
    wm.key(Key::Left, mods);
    assert_eq!(wm.workspace(), 0);
    wm.cycle_workspace(-1);
    assert_eq!(wm.workspace(), crate::model::WORKSPACES - 1);
}

#[test]
fn plain_shift_arrows_never_move_or_resize_the_window() {
    let mut wm = sample();
    let id = wm.focus_id().unwrap();
    let before = wm.window(id).unwrap().frame;
    let mods = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    for key in [Key::Left, Key::Right, Key::Up, Key::Down] {
        wm.key(key, mods);
    }
    assert_eq!(wm.window(id).unwrap().frame, before);
}

#[test]
fn super_h_hides_the_focused_window_and_recedes_it() {
    let mut wm = sample();
    let welcome = wm.focus_id().unwrap();
    wm.key(Key::Char(b'h'), super_m());
    assert_eq!(wm.window(welcome).unwrap().state, WindowState::Minimized);
    // Focus hands over to another live window.
    assert_ne!(wm.focus_id(), Some(welcome));
    assert!(wm.focus_id().is_some());
}

#[test]
fn super_q_closes_the_focused_app_window() {
    let mut wm = sample();
    let welcome = wm.focus_id().unwrap();
    wm.key(Key::Char(b'q'), super_m());
    assert!(wm.window(welcome).unwrap().closing);
}

#[test]
fn ctrl_alt_stands_in_for_super_only_with_fallback_enabled() {
    let mut wm = sample();
    let id = wm.focus_id().unwrap();
    let fallback = Modifiers {
        ctrl: true,
        alt: true,
        ..Modifiers::default()
    };
    let ctrl_only = ctrl();
    let alt_only = Modifiers {
        alt: true,
        ..Modifiers::default()
    };

    // Fallback off: Ctrl+Alt is not a command.
    wm.key(Key::Char(b'2'), fallback);
    assert_eq!(wm.workspace(), 0);

    // Fallback on: Ctrl+Alt acts as Super.
    wm.toggle_setting(4);
    assert!(wm.settings().system_key_fallback);
    wm.key(Key::Char(b'2'), fallback);
    assert_eq!(wm.workspace(), 1);
    wm.key(Key::Char(b' '), fallback);
    assert!(wm.spotlight_open());
    wm.key(Key::Escape, Modifiers::default());

    // But both keys are required; plain Ctrl or Alt still belong to the app.
    wm.key(Key::Char(b'2'), ctrl_only);
    assert_eq!(wm.workspace(), 1);
    wm.key(Key::Char(b'2'), alt_only);
    assert_eq!(wm.workspace(), 1);
    wm.key(Key::Char(b'w'), ctrl_only);
    wm.key(Key::Char(b'w'), alt_only);
    assert!(!wm.window(id).unwrap().closing);
}

#[test]
fn tab_cycles_focus_but_ctrl_and_alt_tab_do_not() {
    let mut wm = sample();
    let first = wm.focus_id().unwrap();

    wm.key(Key::Tab, Modifiers::default());
    let second = wm.focus_id().unwrap();
    assert_ne!(first, second);

    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    wm.key(Key::Tab, shift);
    assert_eq!(wm.focus_id(), Some(first));

    // Ctrl+Tab and Alt+Tab are app chords and must not be swallowed by the
    // manager's focus cycling.
    wm.key(Key::Tab, ctrl());
    assert_eq!(wm.focus_id(), Some(first));
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    wm.key(Key::Tab, alt);
    assert_eq!(wm.focus_id(), Some(first));
}

#[test]
fn is_wm_chord_reserves_only_the_manager_chords() {
    let wm = sample();
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };

    // Plain Ctrl / Alt / Shift chords still belong to the app.
    assert!(!wm.is_wm_chord(Key::Char(b'w'), ctrl()));
    assert!(!wm.is_wm_chord(Key::Tab, ctrl()));
    assert!(!wm.is_wm_chord(Key::Char(b'f'), ctrl()));
    assert!(!wm.is_wm_chord(Key::Left, shift));
    assert!(!wm.is_wm_chord(Key::Right, shift));
    assert!(!wm.is_wm_chord(Key::Char(b'f'), alt));
    assert!(!wm.is_wm_chord(Key::Char(b'a'), alt));

    // Super chords are reserved, including the Space and resize forms.
    let super_alt = Modifiers {
        super_key: true,
        alt: true,
        ..Modifiers::default()
    };
    let super_shift = Modifiers {
        super_key: true,
        shift: true,
        ..Modifiers::default()
    };
    assert!(wm.is_wm_chord(Key::Char(b'w'), super_m()));
    assert!(wm.is_wm_chord(Key::Tab, super_m()));
    assert!(wm.is_wm_chord(Key::Char(b' '), super_m()));
    assert!(wm.is_wm_chord(Key::Char(b'1'), super_m()));
    assert!(wm.is_wm_chord(Key::Left, super_m()));
    assert!(wm.is_wm_chord(Key::Right, super_m()));
    assert!(wm.is_wm_chord(Key::Up, super_m()));
    assert!(wm.is_wm_chord(Key::Down, super_m()));
    assert!(wm.is_wm_chord(Key::Left, super_alt));
    assert!(wm.is_wm_chord(Key::Left, super_shift));

    // App-independent keys are reserved even without a modifier — and with one.
    let plain = Modifiers::default();
    assert!(wm.is_wm_chord(Key::Function(3), plain));
    assert!(wm.is_wm_chord(Key::Function(4), plain));
    assert!(wm.is_wm_chord(Key::Function(10), plain));
    assert!(wm.is_wm_chord(Key::SuperTap, plain));
    assert!(wm.is_wm_chord(Key::Function(3), ctrl()));
    assert!(wm.is_wm_chord(Key::SuperTap, super_m()));
}

#[test]
fn is_wm_chord_under_the_fallback_requires_ctrl_and_alt_together() {
    let mut wm = sample();
    wm.toggle_setting(4);
    assert!(wm.settings().system_key_fallback);
    let fallback = Modifiers {
        ctrl: true,
        alt: true,
        ..Modifiers::default()
    };

    // Ctrl+Alt stands in for Super: those chords are reserved.
    assert!(wm.is_wm_chord(Key::Char(b'w'), fallback));
    assert!(wm.is_wm_chord(Key::Tab, fallback));
    assert!(wm.is_wm_chord(Key::Char(b'2'), fallback));
    assert!(wm.is_wm_chord(Key::Left, fallback));
    assert!(wm.is_wm_chord(Key::Right, fallback));

    // Either modifier alone still belongs to the app.
    assert!(!wm.is_wm_chord(Key::Char(b'w'), ctrl()));
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    assert!(!wm.is_wm_chord(Key::Char(b'f'), alt));

    // App-independent keys stay reserved either way.
    assert!(wm.is_wm_chord(Key::Function(3), ctrl()));
    assert!(wm.is_wm_chord(Key::Function(10), Modifiers::default()));
    assert!(wm.is_wm_chord(Key::SuperTap, Modifiers::default()));
}

#[test]
fn linux_key_slot_routes_app_chords_but_keeps_manager_chords_and_overlays() {
    let mut wm = sample();
    let linux = wm.open(WindowKind::Linux).unwrap();
    wm.focus_window(linux);

    // App input passes to the Linux surface — including the Ctrl and Alt
    // chords that are no longer manager chords.
    assert_eq!(wm.linux_key_slot(Key::Char(b'w'), ctrl()), Some(0));
    assert_eq!(wm.linux_key_slot(Key::Tab, ctrl()), Some(0));
    assert_eq!(
        wm.linux_key_slot(Key::Char(b'a'), Modifiers::default()),
        Some(0)
    );

    // Manager chords and the app-independent keys stay native over the
    // Linux surface.
    assert_eq!(wm.linux_key_slot(Key::Char(b'w'), super_m()), None);
    assert_eq!(
        wm.linux_key_slot(Key::Function(3), Modifiers::default()),
        None
    );
    assert_eq!(
        wm.linux_key_slot(Key::Function(10), Modifiers::default()),
        None
    );
    assert_eq!(wm.linux_key_slot(Key::SuperTap, Modifiers::default()), None);

    // A native window routes nothing into Linux.
    wm.open(WindowKind::Welcome);
    assert_eq!(wm.linux_key_slot(Key::Char(b'w'), ctrl()), None);

    // Overlays and the menu sit above the Linux surface.
    wm.focus_window(linux);
    wm.open_spotlight();
    assert_eq!(
        wm.linux_key_slot(Key::Char(b'a'), Modifiers::default()),
        None
    );
    wm.key(Key::Escape, Modifiers::default());
    assert!(!wm.spotlight_open());
    wm.toggle_overview();
    assert_eq!(
        wm.linux_key_slot(Key::Char(b'a'), Modifiers::default()),
        None
    );
    wm.key(Key::Escape, Modifiers::default());
    assert!(!wm.overview_open());
    wm.key(Key::Function(10), Modifiers::default());
    assert!(wm.menu_state().is_some());
    assert_eq!(
        wm.linux_key_slot(Key::Char(b'a'), Modifiers::default()),
        None
    );
    wm.key(Key::Escape, Modifiers::default());
    assert!(wm.menu_state().is_none());
    assert_eq!(
        wm.linux_key_slot(Key::Char(b'a'), Modifiers::default()),
        Some(0)
    );
}

#[test]
fn fallback_ctrl_alt_arrows_move_and_shift_resizes_the_focused_window() {
    let mut wm = sample();
    assert_eq!(wm.workspace(), 0);
    wm.toggle_setting(4);
    assert!(wm.settings().system_key_fallback);
    let id = wm.focus_id().unwrap();
    let before = wm.window(id).unwrap().frame;
    let fallback = Modifiers {
        ctrl: true,
        alt: true,
        ..Modifiers::default()
    };
    let fallback_shift = Modifiers {
        ctrl: true,
        alt: true,
        shift: true,
        ..Modifiers::default()
    };

    // Ctrl+Alt+arrows moves the focused window instead of cycling spaces:
    // the Super+Alt+arrows form is not pressable under the fallback.
    wm.key(Key::Left, fallback);
    let frame = wm.window(id).unwrap().frame;
    assert_eq!(frame.x, before.x - 8);
    assert_eq!(frame.y, before.y);
    assert_eq!(wm.workspace(), 0);

    // Ctrl+Alt+Shift+arrows resizes it; plain move and resize do not touch
    // the workspace either way.
    wm.key(Key::Right, fallback_shift);
    assert_eq!(wm.window(id).unwrap().frame.width, before.width + 8);
    assert_eq!(wm.workspace(), 0);

    // Under the fallback, space switching is the direct jump.
    wm.key(Key::Char(b'2'), fallback);
    assert_eq!(wm.workspace(), 1);
}

#[test]
fn render_shortcut_applies_the_symbol_and_fallback_choice() {
    let mut wm = sample();
    fn render(settings: crate::model::Settings, chord: &str) -> String {
        let mut out = [0u8; 64];
        let len = settings.render_shortcut(chord, &mut out);
        core::str::from_utf8(&out[..len.min(out.len())])
            .unwrap()
            .to_string()
    }

    let settings = wm.settings();
    // Default renders with the Windows symbol; plain keys pass through.
    assert_eq!(render(settings, "Super+W"), "Win+W");
    assert_eq!(render(settings, "Super+Alt+Left"), "Win+Alt+Left");
    assert_eq!(render(settings, "F3"), "F3");

    // The symbol cycles Windows, Mac, then Super.
    wm.toggle_setting(5);
    assert_eq!(render(wm.settings(), "Super+W"), "Cmd+W");
    wm.toggle_setting(5);
    assert_eq!(render(wm.settings(), "Super+W"), "Super+W");
    wm.toggle_setting(5);
    assert_eq!(
        wm.settings().shortcut_symbol,
        crate::model::ShortcutSymbol::Windows
    );

    // The fallback key overrides the symbol and drops the overlapping Alt,
    // so "Super+Alt+Left" is the keys actually pressed.
    wm.toggle_setting(4);
    let settings = wm.settings();
    assert_eq!(render(settings, "Super+W"), "Ctrl+Alt+W");
    assert_eq!(render(settings, "Super+Alt+Left"), "Ctrl+Alt+Left");
    assert_eq!(render(settings, "F3"), "F3");

    // Short output buffers truncate the copy but report the full length.
    let mut out = [0u8; 5];
    assert_eq!(settings.render_shortcut("Super+W", &mut out), 10);
    assert_eq!(&out[..5], b"Ctrl+");
}

#[test]
fn traffic_light_click_starts_close() {
    let mut wm = sample();
    let welcome = *wm.focused_window().unwrap();
    let lights = traffic_lights(welcome.frame);
    wm.pointer_down(lights.close.center(), 0);
    assert!(wm.window(welcome.id).unwrap().closing);
}

#[test]
fn title_bar_drag_moves_a_window() {
    let mut wm = sample();
    let welcome = wm.focused_window().unwrap();
    let frame = welcome.frame;
    let id = welcome.id;
    let start = Point::new(frame.x + frame.width / 2, frame.y + 12);
    wm.pointer_down(start, 0);
    wm.pointer_moved(start.offset(24, 18));
    wm.pointer_up();
    let moved = wm.window(id).unwrap().frame;
    assert_eq!(moved.x, frame.x + 24);
    assert_eq!(moved.y, frame.y + 18);
}

#[test]
fn ending_a_drag_damages_the_window_and_its_shadow() {
    let mut wm = sample();
    let welcome = wm.focused_window().unwrap();
    let id = welcome.id;
    let start = Point::new(
        welcome.frame.x + welcome.frame.width / 2,
        welcome.frame.y + 12,
    );
    wm.pointer_down(start, 0);
    wm.pointer_moved(start.offset(24, 18));
    assert_eq!(wm.dragging_id(), Some(id));
    let before = wm;
    wm.pointer_up();
    assert_eq!(wm.dragging_id(), None);
    let shadow = wm.window(id).unwrap().frame.inset(-24);
    let damage = wm.damage_since(&before).unwrap();
    assert_eq!(damage.union(shadow), damage);
}

#[test]
fn resize_grip_enforces_minimum_size() {
    let mut wm = sample();
    let welcome = wm.focused_window().unwrap();
    let frame = welcome.frame;
    let id = welcome.id;
    let grip = layout::resize_grip(frame).center();
    wm.pointer_down(grip, 0);
    wm.pointer_moved(grip.offset(-500, -500));
    wm.pointer_up();
    let resized = wm.window(id).unwrap().frame;
    assert_eq!(resized.width, MIN_WINDOW_WIDTH);
    assert_eq!(resized.height, MIN_WINDOW_HEIGHT);
}

#[test]
fn dock_click_opens_an_application() {
    let mut wm = Wm::new(Rect::new(0, 0, 1280, 800));
    let icons = layout::dock_icon_rects(wm.screen(), &[0; DOCK_ITEMS]);
    wm.pointer_down(icons[0].center(), 0);
    assert_eq!(wm.window_count(), 1);
    assert_eq!(wm.focused_window().unwrap().kind, WindowKind::Files);
    assert_eq!(
        DOCK_SLOTS[0],
        DockSlot::App(WindowKind::Files),
        "dock order is part of the public layout contract"
    );
}

#[test]
fn overview_dock_icon_opens_the_overview() {
    let mut wm = sample();
    let icons = layout::dock_icon_rects(wm.screen(), &[0; DOCK_ITEMS]);
    wm.pointer_down(icons[DOCK_ITEMS - 1].center(), 0);
    assert!(wm.overview_open());
    assert_eq!(DOCK_SLOTS[DOCK_ITEMS - 1], DockSlot::Overview);
}

#[test]
fn overview_card_click_switches_workspace() {
    let mut wm = sample();
    wm.toggle_overview();
    let cards = overview_cards(wm.screen());
    wm.pointer_down(cards[2].center(), 0);
    assert_eq!(wm.workspace(), 2);
    assert!(!wm.overview_open());
}

#[test]
fn settings_rows_are_clickable_and_accessible() {
    let mut wm = Wm::new(Rect::new(0, 0, 800, 600));
    let settings = wm.open(WindowKind::Settings).unwrap();
    wm.focus_window(settings);
    wm.finish_animations();
    for row in 0..crate::model::Settings::row_count() {
        assert!(wm.settings_row_rect(settings, row).is_some(), "row {row}");
    }
    let row = wm.settings_row_rect(settings, 0).unwrap().center();
    wm.pointer_down(row, 0);
    assert!(wm.settings().reduce_motion);
    assert!(wm.window(settings).is_some());
    // The multivalue symbol row advances its value, the fallback row toggles.
    wm.pointer_down(wm.settings_row_rect(settings, 5).unwrap().center(), 8);
    assert_eq!(
        wm.settings().shortcut_symbol,
        crate::model::ShortcutSymbol::Mac
    );
    wm.pointer_down(wm.settings_row_rect(settings, 4).unwrap().center(), 16);
    assert!(wm.settings().system_key_fallback);
}

#[test]
fn glass_setting_cycles_and_is_shared_by_the_palette_contract() {
    let mut wm = sample();
    assert_eq!(wm.settings().glass, crate::theme::GlassLevel::Balanced);
    wm.toggle_setting(2);
    assert_eq!(wm.settings().glass, crate::theme::GlassLevel::Tinted);
    assert!(
        crate::theme::GlassLevel::Tinted.panel_alpha()
            > crate::theme::GlassLevel::Clear.panel_alpha()
    );
    wm.toggle_setting(2);
    assert_eq!(wm.settings().glass, crate::theme::GlassLevel::Clear);
}

#[test]
fn spotlight_filters_and_executes_results() {
    let mut wm = sample();
    wm.open_spotlight();
    assert!(wm.spotlight_open());
    assert!(wm.search_result_count() > 0);
    for byte in b"security" {
        wm.key(Key::Char(*byte), Modifiers::default());
    }
    let count = wm.search_result_count();
    assert!(count > 0 && count < 8);
    let first = wm.search_result(0).unwrap().label;
    assert!(first.to_ascii_lowercase().contains("security"));
    let outcome = wm.key(Key::Enter, Modifiers::default());
    assert!(outcome.changed);
    assert!(!wm.spotlight_open());
    assert_eq!(wm.focused_window().unwrap().kind, WindowKind::Security);
}

#[test]
fn spotlight_status_item_and_shortcut_open_search() {
    let mut wm = sample();
    let status = layout::status_items(wm.screen());
    wm.pointer_down(status.search.center(), 0);
    assert!(wm.spotlight_open());
    wm.key(Key::Escape, Modifiers::default());
    assert!(!wm.spotlight_open());
    wm.key(
        Key::Char(b' '),
        Modifiers {
            super_key: true,
            ..Modifiers::default()
        },
    );
    assert!(wm.spotlight_open());
    wm.key(
        Key::Char(b' '),
        Modifiers {
            super_key: true,
            ..Modifiers::default()
        },
    );
    assert!(!wm.spotlight_open());
}

#[test]
fn super_tap_opens_and_closes_search() {
    let mut wm = sample();
    let outcome = wm.key(Key::SuperTap, Modifiers::default());
    assert!(outcome.changed);
    assert!(wm.spotlight_open());
    let outcome = wm.key(Key::SuperTap, Modifiers::default());
    assert!(outcome.changed);
    assert!(!wm.spotlight_open());
}

#[test]
fn menu_bar_status_items_open_their_destinations() {
    let mut wm = sample();
    let status = layout::status_items(wm.screen());
    wm.pointer_down(status.spaces.center(), 0);
    assert!(wm.overview_open());
    wm.key(Key::Escape, Modifiers::default());
    assert!(!wm.overview_open());
    wm.pointer_down(status.security.center(), 0);
    assert_eq!(wm.focused_window().unwrap().kind, WindowKind::Security);
}

#[test]
fn reduce_motion_finishes_running_animations() {
    let mut wm = sample();
    wm.switch_workspace(1);
    assert!(wm.transition().active);
    wm.toggle_setting(0);
    assert!(!wm.transition().active);
    assert!(!wm.is_animating());
}

#[test]
fn high_contrast_has_legible_contrast() {
    let palette = ThemeKind::HighContrast.palette();
    assert!(Palette::contrast_ratio(palette.text, palette.window) >= 1500);
    assert!(Palette::contrast_ratio(palette.text, palette.menu_bar) >= 1500);
    assert!(Palette::contrast_ratio(palette.accent, palette.window) >= 500);
    assert_eq!(ThemeKind::Aurora.toggled(), ThemeKind::HighContrast);
}

#[test]
fn default_frames_fit_every_supported_resolution() {
    for (width, height) in [
        (640, 480),
        (800, 600),
        (1024, 768),
        (1280, 800),
        (1920, 1080),
    ] {
        let screen = Rect::new(0, 0, width, height);
        let area = work_area(screen);
        for kind in WindowKind::ALL {
            let frame = default_window_frame(kind, screen, 0);
            assert!(
                frame.x >= area.x
                    && frame.y >= area.y
                    && frame.right() <= area.right()
                    && frame.bottom() <= area.bottom(),
                "{kind:?} at {width}x{height} escaped {area:?}: {frame:?}"
            );
            assert!(frame.width >= MIN_WINDOW_WIDTH);
            assert!(frame.height >= MIN_WINDOW_HEIGHT);
            let lights = traffic_lights(frame);
            assert!(lights.close.right() < lights.minimize.x);
            assert!(lights.minimize.right() < lights.zoom.x);
            assert!(lights.zoom.right() <= frame.right());
        }
        let dock = dock_panel(screen);
        assert!(dock.x >= 0 && dock.right() <= width);
        assert!(dock.bottom() <= height);
        let cards = overview_cards(screen);
        for card in cards {
            assert!(card.x >= 0 && card.right() <= width);
            assert!(card.y >= 0 && card.bottom() <= height);
        }
    }
}

#[test]
fn menu_items_are_bounded_and_have_actions() {
    for kind in [
        MenuKind::System,
        MenuKind::App,
        MenuKind::File,
        MenuKind::View,
        MenuKind::Window,
        MenuKind::Help,
    ] {
        let items = kind.items();
        assert!(!items.is_empty());
        for item in items {
            if item.is_separator() {
                assert_eq!(item.command, Command::None);
            } else {
                assert!(!item.label.is_empty());
            }
        }
    }
}

#[test]
fn max_windows_is_enforced_without_panicking() {
    let mut wm = Wm::new(Rect::new(0, 0, 1280, 800));
    for _ in 0..crate::model::MAX_WINDOWS * 2 {
        for kind in WindowKind::ALL {
            let _ = wm.open(kind);
        }
    }
    assert!(wm.window_count() <= crate::model::MAX_WINDOWS);
    for _ in 0..20 {
        wm.tick(1);
    }
}

#[test]
fn minimize_immediately_transfers_focus_to_the_topmost_eligible_window() {
    let mut wm = sample();
    let welcome = wm.focus_id().unwrap();
    wm.minimize_window(welcome);
    assert_ne!(wm.focus_id(), Some(welcome));
    for _ in 0..10 {
        wm.focus_next(-1);
        assert_ne!(wm.focus_id(), Some(welcome));
    }
    for kind in [WindowKind::Files, WindowKind::Security] {
        wm.minimize_window(wm.window_id(kind).unwrap());
    }
    assert_eq!(wm.focus_id(), None);
}

#[test]
fn reopening_and_minimizing_preserve_zoom_state_and_restore_geometry() {
    let mut wm = sample();
    let id = wm.window_id(WindowKind::Files).unwrap();
    let original = wm.window(id).unwrap().frame;
    wm.zoom_window(id);
    let zoomed = wm.window(id).unwrap().frame;
    wm.open(WindowKind::Files);
    assert_eq!(wm.window(id).unwrap().state, WindowState::Maximized);
    wm.minimize_window(id);
    wm.open(WindowKind::Files);
    assert_eq!(wm.window(id).unwrap().frame, zoomed);
    assert_eq!(wm.window(id).unwrap().state, WindowState::Maximized);
    wm.zoom_window(id);
    assert_eq!(wm.window(id).unwrap().frame, original);
}

#[test]
fn repeated_focus_cycles_visit_every_window_in_both_directions() {
    for direction in [-1, 1] {
        let mut wm = sample();
        let start = wm.focus_id().unwrap();
        let mut seen = vec![start];
        for _ in 0..2 {
            wm.focus_next(direction);
            let id = wm.focus_id().unwrap();
            assert!(!seen.contains(&id));
            seen.push(id);
        }
        wm.focus_next(direction);
        assert_eq!(wm.focus_id(), Some(start));
    }
}

#[test]
fn dock_restore_switches_to_the_minimized_windows_workspace() {
    let mut wm = sample();
    let id = wm.window_id(WindowKind::Files).unwrap();
    wm.minimize_window(id);
    wm.switch_workspace(2);
    wm.finish_animations();
    let icon = layout::dock_icon_rects(wm.screen(), wm.dock_hover())[0];
    wm.pointer_down(icon.center(), 100);
    assert_eq!(wm.workspace(), 0);
    assert_eq!(wm.focus_id(), Some(id));
    assert_eq!(wm.window(id).unwrap().state, WindowState::Normal);
}

#[test]
fn closing_windows_remain_painted_but_cannot_receive_input() {
    let mut wm = sample();
    let id = wm.focus_id().unwrap();
    wm.close_window(id);
    assert!(wm.window(id).unwrap().is_visible());
    let point = traffic_lights(wm.window(id).unwrap().frame).close.center();
    assert_ne!(wm.hit_test(point), crate::Hit::Close(id));
    wm.tick(30);
    assert!(wm.window(id).is_none());
}

#[test]
fn spotlight_field_and_padding_never_execute_the_first_result() {
    let mut wm = sample();
    wm.open_spotlight();
    let panel = layout::spotlight_panel(wm.screen());
    for point in [
        layout::spotlight_field(panel).center(),
        Point::new(panel.x + 2, panel.y + 60),
        Point::new(panel.x + 20, panel.y + 53),
    ] {
        wm.pointer_down(point, 1);
        assert!(wm.spotlight_open());
        assert_eq!(wm.focused_window().unwrap().kind, WindowKind::Welcome);
    }
    wm.pointer_down(layout::spotlight_result_rect(panel, 0).center(), 2);
    assert!(!wm.spotlight_open());
    assert_eq!(wm.focused_window().unwrap().kind, WindowKind::Files);
}

#[test]
fn menu_padding_does_not_execute_actions_and_logo_toggles_closed() {
    let mut wm = sample();
    let logo = layout::menu_logo_rect(wm.screen()).center();
    wm.pointer_down(logo, 1);
    let panel = wm.menu_panel().unwrap();
    wm.pointer_down(Point::new(panel.x + 1, panel.y + 1), 2);
    assert!(wm.menu_state().is_some());
    wm.pointer_down(logo, 3);
    assert!(wm.menu_state().is_none());
}

#[test]
fn keyboard_can_open_every_menu_and_resize_without_a_pointer() {
    let mut wm = sample();
    wm.key(Key::Function(10), Modifiers::default());
    for kind in MenuKind::ALL {
        assert_eq!(wm.menu_state().unwrap().kind, kind);
        assert!(wm.menu_state().unwrap().keyboard);
        wm.key(Key::Right, Modifiers::default());
    }
    wm.key(Key::Escape, Modifiers::default());
    let before = wm.focused_window().unwrap().frame;
    let mut mods = super_m();
    mods.shift = true;
    wm.key(Key::Right, mods);
    assert_eq!(wm.focused_window().unwrap().frame.width, before.width + 8);
}

#[test]
fn window_ids_do_not_alias_live_windows_after_wrapping() {
    let mut wm = Wm::new(Rect::new(0, 0, 1280, 800));
    let files = wm.open(WindowKind::Files).unwrap();
    for _ in 0..600 {
        let other = wm.open(WindowKind::Welcome).unwrap();
        assert_ne!(files, other);
        wm.close_window(other);
        wm.tick(30);
        assert_eq!(wm.window(files).unwrap().kind, WindowKind::Files);
        assert_eq!(wm.order(), &[files]);
    }
}

#[test]
fn reduce_motion_also_finishes_dock_magnification() {
    let mut wm = sample();
    wm.toggle_setting(0);
    let icon = layout::dock_icon_rects(wm.screen(), wm.dock_hover())[0];
    wm.pointer_moved(icon.center());
    assert_eq!(wm.dock_hover()[0], 256);
    assert!(!wm.is_animating());
    wm.pointer_moved(Point::new(0, 0));
    assert_eq!(*wm.dock_hover(), [0; DOCK_ITEMS]);
    assert!(!wm.is_animating());
}

#[test]
fn menu_alpha_never_wraps_at_the_tinted_setting() {
    use crate::GlassLevel;
    assert_eq!(GlassLevel::Clear.menu_alpha(), 208);
    assert_eq!(GlassLevel::Balanced.menu_alpha(), 238);
    assert_eq!(GlassLevel::Tinted.menu_alpha(), 250);
}

#[test]
fn cursor_only_movement_does_not_damage_the_scene() {
    let mut wm = sample();
    let before = wm;
    wm.pointer_moved(wm.cursor().offset(2, 2));
    assert_eq!(wm.damage_since(&before), None);
}

#[test]
fn static_overlays_do_not_force_continuous_full_screen_repaints() {
    let mut wm = sample();
    wm.open_spotlight();
    let before = wm;
    wm.tick(1);
    assert_eq!(wm.damage_since(&before), None);
    wm.close_spotlight();
    wm.toggle_overview();
    wm.finish_animations();
    let before = wm;
    wm.tick(1);
    assert_eq!(wm.damage_since(&before), None);
}

#[test]
fn typing_in_search_damages_only_the_panel_and_its_shadow() {
    let mut wm = sample();
    wm.open_spotlight();
    let before = wm;
    wm.key(Key::Char(b's'), Modifiers::default());
    let panel = crate::layout::spotlight_panel(wm.screen());
    let damage = wm.damage_since(&before).unwrap();
    // The panel shadow is offset (5, 10) from the panel.
    let shadow = Rect::new(panel.x + 5, panel.y + 10, panel.width, panel.height);
    assert_eq!(damage.intersection(panel), Some(panel));
    assert_eq!(damage.intersection(shadow), Some(shadow));
    assert!(damage.width < wm.screen().width);
    let before = wm;
    wm.key(Key::Down, Modifiers::default());
    assert_eq!(wm.damage_since(&before), Some(damage));
    // Closing search undims the whole screen.
    let before = wm;
    wm.close_spotlight();
    assert_eq!(wm.damage_since(&before), Some(wm.screen()));
}

#[test]
fn moving_a_window_damages_old_and_new_frames_including_shadows() {
    let mut wm = sample();
    let before = wm;
    let old = wm.focused_window().unwrap().frame;
    wm.move_focused(8, 8);
    let new = wm.focused_window().unwrap().frame;
    let damage = wm.damage_since(&before).unwrap();
    assert_eq!(damage.intersection(old.inset(-24)), Some(old.inset(-24)));
    assert_eq!(damage.intersection(new.inset(-24)), Some(new.inset(-24)));
    assert!(damage.width < wm.screen().width);
    assert!(damage.height < wm.screen().height);
}

#[test]
fn damage_keeps_distant_rectangles_apart_and_merges_free_ones() {
    use crate::Damage;
    let mut damage = Damage::new();
    assert!(damage.is_empty());
    damage.include(Rect::new(0, 0, 0, 10));
    assert!(damage.is_empty());
    // A menu bar and a dock strip stay separate.
    damage.include(Rect::new(0, 0, 1280, 27));
    damage.include(Rect::new(400, 700, 480, 90));
    assert_eq!(damage.rects().len(), 2);
    assert_eq!(damage.bounds(), Some(Rect::new(0, 0, 1280, 790)));
    // An adjacent strip of the same width merges without waste.
    damage.include(Rect::new(0, 27, 1280, 10));
    assert_eq!(damage.rects().len(), 2);
    assert!(damage.rects().contains(&Rect::new(0, 0, 1280, 37)));
    // A rectangle inside another is absorbed.
    damage.include(Rect::new(500, 710, 10, 10));
    assert_eq!(damage.rects().len(), 2);
    // Past capacity, rectangles merge but every pixel stays covered.
    let parts = [
        Rect::new(0, 100, 10, 10),
        Rect::new(300, 200, 10, 10),
        Rect::new(600, 300, 10, 10),
        Rect::new(900, 400, 10, 10),
    ];
    for part in parts {
        damage.include(part);
    }
    assert!(damage.rects().len() <= crate::DAMAGE_RECTS);
    for part in parts {
        assert!(
            damage
                .rects()
                .iter()
                .any(|rect| rect.intersection(part) == Some(part))
        );
    }
    let clipped = damage.clipped(Rect::new(0, 0, 640, 480));
    assert!(
        clipped
            .rects()
            .iter()
            .all(|rect| rect.right() <= 640 && rect.bottom() <= 480)
    );
}

#[test]
fn switching_spaces_keeps_menu_bar_and_dock_damage_separate() {
    let mut wm = sample();
    wm.finish_animations();
    let before = wm;
    wm.cycle_workspace(1);
    let region = wm.damage_region_since(&before);
    assert!(region.rects().len() > 1);
    let painted: i64 = region
        .rects()
        .iter()
        .map(|rect| i64::from(rect.width) * i64::from(rect.height))
        .sum();
    let screen = wm.screen();
    assert!(painted < i64::from(screen.width) * i64::from(screen.height));
}

#[test]
fn workspace_slide_damages_only_moving_windows_and_the_pill() {
    let mut wm = sample();
    wm.finish_animations();
    let screen = wm.screen();
    let pill = crate::layout::space_pill(screen);
    let mut before = wm;
    wm.cycle_workspace(1);
    assert!(wm.transition().active);
    let first = wm.damage_since(&before).unwrap();
    assert_eq!(first.intersection(pill), Some(pill));
    assert_eq!(
        first.intersection(crate::layout::menu_bar(screen)),
        Some(crate::layout::menu_bar(screen))
    );
    let mut frames = 0;
    while wm.transition().active {
        before = wm;
        wm.tick(3);
        frames += 1;
        let damage = wm.damage_since(&before).unwrap();
        // Every window frame painted before or after the step is covered.
        for id in before.order().iter().chain(wm.order()) {
            for frame in [before.window_frame(*id), wm.window_frame(*id)]
                .into_iter()
                .flatten()
            {
                if let Some(visible) = frame.inset(-24).intersection(screen) {
                    assert_eq!(damage.intersection(visible), Some(visible));
                }
            }
        }
        // The menu bar and dock rows are static and not repainted wholesale.
        assert!(damage.height < screen.height);
    }
    assert!(frames > 1);
    // The step that ends the slide removes the pill.
    let last = wm.damage_since(&before).unwrap();
    assert_eq!(last.intersection(pill), Some(pill));
}

#[test]
fn hit_testing_tracks_animated_not_logical_window_frames() {
    let mut wm = Wm::new(Rect::new(0, 0, 1280, 800));
    let id = wm.open(WindowKind::Settings).unwrap();
    let frame = wm.window_frame(id).unwrap();
    assert_ne!(frame, wm.window(id).unwrap().frame);
    assert_eq!(
        wm.hit_test(traffic_lights(frame).close.center()),
        crate::Hit::Close(id)
    );
    wm.finish_animations();
    assert_eq!(wm.window_frame(id), Some(wm.window(id).unwrap().frame));
}

#[test]
fn terminal_frame_fits_its_character_grid() {
    let screen = Rect::new(0, 0, 1280, 800);
    let frame = default_window_frame(WindowKind::Terminal, screen, 0);
    let content = layout::content_rect(frame);
    assert_eq!(content.width, 80 * 8 + layout::TERMINAL_PADDING * 2);
    assert_eq!(content.height, 24 * 16 + layout::TERMINAL_PADDING * 2);
    assert!(work_area(screen).contains(Point::new(frame.x, frame.y)));
    assert!(DOCK_SLOTS.contains(&DockSlot::App(WindowKind::Terminal)));

    let mut wm = sample();
    let id = wm.open(WindowKind::Terminal).unwrap();
    assert_eq!(wm.focused_window().unwrap().kind, WindowKind::Terminal);
    assert_eq!(wm.window_id(WindowKind::Terminal), Some(id));
}
