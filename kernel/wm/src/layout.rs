// SPDX-License-Identifier: Apache-2.0
//! Layout rules for the menu bar, dock, windows, menus, and workspace cards.
//!
//! Layout is a pure function of the screen rectangle and current state. The
//! renderer and hit tester call the same functions, so a control cannot be
//! painted somewhere it is not clickable.

use crate::geometry::{Point, Rect, Size};
use crate::model::{MenuKind, WindowKind};

/// Height of the top menu bar.
pub const MENU_BAR_HEIGHT: i32 = 27;
/// Height of the dock panel.
pub const DOCK_HEIGHT: i32 = 64;
/// Distance between the dock and the bottom of the screen.
pub const DOCK_MARGIN: i32 = 10;
/// Base dock icon size.
pub const DOCK_ICON_BASE: i32 = 42;
/// Horizontal slot reserved for each dock item.
pub const DOCK_SLOT_WIDTH: i32 = 50;
/// Number of dock items.
pub const DOCK_ITEMS: usize = 10;
/// Window title-bar height.
pub const TITLE_BAR_HEIGHT: i32 = 31;
/// Window corner radius.
pub const WINDOW_RADIUS: i32 = 10;
/// Minimum window width for a resize gesture.
pub const MIN_WINDOW_WIDTH: i32 = 300;
/// Minimum window height for a resize gesture.
pub const MIN_WINDOW_HEIGHT: i32 = 230;

/// One dock slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockSlot {
    /// An application window.
    App(WindowKind),
    /// The workspace overview.
    Overview,
}

impl DockSlot {
    /// Returns the label shown above the icon.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::App(kind) => kind.app_name(),
            Self::Overview => "SPACES",
        }
    }
}

/// Dock contents in left-to-right order.
pub const DOCK_SLOTS: [DockSlot; DOCK_ITEMS] = [
    DockSlot::App(WindowKind::Files),
    DockSlot::App(WindowKind::Terminal),
    DockSlot::App(WindowKind::Security),
    DockSlot::App(WindowKind::Portal),
    DockSlot::App(WindowKind::Settings),
    DockSlot::App(WindowKind::Recovery),
    DockSlot::App(WindowKind::Log),
    DockSlot::App(WindowKind::Welcome),
    DockSlot::App(WindowKind::Applications),
    DockSlot::Overview,
];

/// Returns the menu bar rectangle.
#[must_use]
pub const fn menu_bar(screen: Rect) -> Rect {
    Rect::new(screen.x, screen.y, screen.width, MENU_BAR_HEIGHT)
}

/// Returns the Space name pill shown while a workspace slide runs.
#[must_use]
pub const fn space_pill(screen: Rect) -> Rect {
    Rect::new(
        screen.x + screen.width / 2 - 50,
        screen.y + screen.height - 110,
        100,
        24,
    )
}

/// Returns the usable area between the menu bar and dock.
#[must_use]
pub fn work_area(screen: Rect) -> Rect {
    let top = screen.y + MENU_BAR_HEIGHT;
    let bottom = screen.bottom() - DOCK_HEIGHT - DOCK_MARGIN * 2;
    Rect::from_points(screen.x, top, screen.right(), bottom)
}

/// Returns the dock panel rectangle, centred horizontally.
#[must_use]
pub fn dock_panel(screen: Rect) -> Rect {
    let width = DOCK_SLOT_WIDTH * DOCK_ITEMS as i32 + 24;
    let x = screen.x + (screen.width - width) / 2;
    let y = screen.bottom() - DOCK_HEIGHT - DOCK_MARGIN;
    Rect::new(x, y, width, DOCK_HEIGHT)
}

/// Returns the eight base slot rectangles used for hover targeting.
#[must_use]
pub fn dock_slot_rects(screen: Rect) -> [Rect; DOCK_ITEMS] {
    let panel = dock_panel(screen);
    let mut rects = [Rect::default(); DOCK_ITEMS];
    let start = panel.x + 12;
    for (index, rect) in rects.iter_mut().enumerate() {
        *rect = Rect::new(
            start + index as i32 * DOCK_SLOT_WIDTH,
            panel.y,
            DOCK_SLOT_WIDTH,
            DOCK_HEIGHT,
        );
    }
    rects
}

/// Returns the index of the dock slot under `point`, if any.
#[must_use]
pub fn dock_slot_at(point: Point, screen: Rect) -> Option<usize> {
    dock_slot_rects(screen)
        .iter()
        .position(|rect| rect.contains(point))
}

/// Returns icon rectangles, enlarged and lifted by the hover values.
#[must_use]
pub fn dock_icon_rects(screen: Rect, hover: &[u16; DOCK_ITEMS]) -> [Rect; DOCK_ITEMS] {
    let panel = dock_panel(screen);
    let mut rects = [Rect::default(); DOCK_ITEMS];
    let start = panel.x + 12;
    for (index, rect) in rects.iter_mut().enumerate() {
        let lift = hover[index].min(256);
        let size = DOCK_ICON_BASE + (lift as i32 * 14) / 256;
        let centre = start + index as i32 * DOCK_SLOT_WIDTH + DOCK_SLOT_WIDTH / 2;
        let bottom = panel.bottom() - 8 - (lift as i32 * 5) / 256;
        *rect = Rect::new(centre - size / 2, bottom - size, size, size);
    }
    rects
}

/// Returns the title-bar rectangle for a window frame.
#[must_use]
pub const fn title_bar(frame: Rect) -> Rect {
    Rect::new(frame.x, frame.y, frame.width, TITLE_BAR_HEIGHT)
}

/// Returns the content rectangle inside a window frame.
#[must_use]
pub const fn content_rect(frame: Rect) -> Rect {
    Rect::new(
        frame.x + 1,
        frame.y + TITLE_BAR_HEIGHT,
        frame.width - 2,
        frame.height - TITLE_BAR_HEIGHT - 1,
    )
}

/// The three title-bar control rectangles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrafficLights {
    /// Close control.
    pub close: Rect,
    /// Minimize control.
    pub minimize: Rect,
    /// Zoom control.
    pub zoom: Rect,
}

/// Returns the traffic-light rectangles for a window frame.
#[must_use]
pub fn traffic_lights(frame: Rect) -> TrafficLights {
    let y = frame.y + (TITLE_BAR_HEIGHT - 12) / 2;
    TrafficLights {
        close: Rect::new(frame.x + 15, y, 12, 12),
        minimize: Rect::new(frame.x + 33, y, 12, 12),
        zoom: Rect::new(frame.x + 51, y, 12, 12),
    }
}

/// Returns the resize grip rectangle at the bottom-right corner.
#[must_use]
pub const fn resize_grip(frame: Rect) -> Rect {
    Rect::new(frame.right() - 16, frame.bottom() - 16, 16, 16)
}

/// Padding between the terminal window's content edge and its character grid.
pub const TERMINAL_PADDING: i32 = 8;

/// Terminal frame size for an 80x24 grid of 8x16 cells: the grid, its
/// padding, the title bar, and the one-pixel window border.
pub const TERMINAL_FRAME: Size = Size {
    width: 80 * 8 + TERMINAL_PADDING * 2 + 2,
    height: 24 * 16 + TERMINAL_PADDING * 2 + TITLE_BAR_HEIGHT + 1,
};

/// Returns the default frame for a window kind.
///
/// The layout adapts to the available screen rather than assuming the QEMU
/// resolution, which keeps the desktop usable on VMware's 1024x768 GOP mode.
#[must_use]
pub fn default_window_frame(kind: WindowKind, screen: Rect, cascade: i32) -> Rect {
    let area = work_area(screen);
    let (width_percent, height_percent) = match kind {
        WindowKind::Files => (42, 60),
        WindowKind::Security => (40, 56),
        WindowKind::Portal => (40, 46),
        WindowKind::Settings => (42, 44),
        WindowKind::Recovery => (42, 44),
        WindowKind::Log => (44, 44),
        WindowKind::Welcome => (36, 46),
        WindowKind::Linux => (52, 62),
        WindowKind::Linux2 => (52, 62),
        WindowKind::Applications => (62, 68),
        WindowKind::Terminal => (0, 0),
    };
    let (width, height) = if kind == WindowKind::Terminal {
        // The terminal's frame fits its fixed character grid exactly.
        (TERMINAL_FRAME.width, TERMINAL_FRAME.height)
    } else {
        (
            area.width * width_percent / 100,
            area.height * height_percent / 100,
        )
    };
    let width = width.max(MIN_WINDOW_WIDTH).min(area.width);
    let height = height.max(MIN_WINDOW_HEIGHT).min(area.height);
    let cascade = cascade.clamp(0, 72);
    let (x, y) = match kind {
        WindowKind::Files => (area.x + 18 + cascade, area.y + 20 + cascade),
        WindowKind::Security => (area.right() - width - 18 - cascade, area.y + 16 + cascade),
        WindowKind::Portal => (area.right() - width - 26 - cascade, area.y + 54 + cascade),
        WindowKind::Settings => (area.x + 36 + cascade, area.y + 72 + cascade),
        WindowKind::Recovery => (
            area.x + (area.width - width) / 2 + cascade / 2,
            area.y + 32 + cascade,
        ),
        WindowKind::Log => (area.right() - width - 34 - cascade, area.y + 66 + cascade),
        WindowKind::Welcome => (
            area.x + (area.width - width) / 2 - cascade / 2,
            area.y + (area.height - height) / 2,
        ),
        WindowKind::Linux | WindowKind::Linux2 | WindowKind::Terminal => (
            area.x + (area.width - width) / 2 + cascade / 2,
            area.y + (area.height - height) / 2 + cascade / 2,
        ),
        WindowKind::Applications => (
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
        ),
    };
    Rect::new(x, y, width, height).clamp_inside(area)
}

/// Menu bar title rectangles for the application, File, View, Window, and Help
/// menus. The system menu is anchored by the logo and is not in this array.
#[must_use]
pub fn menu_title_rects(screen: Rect, app_name: &str) -> [Rect; 5] {
    let labels = [app_name, "File", "View", "Window", "Help"];
    let mut rects = [Rect::default(); 5];
    let mut x = screen.x + 42;
    for (index, label) in labels.iter().enumerate() {
        let width = label.len() as i32 * 6 + 18;
        rects[index] = Rect::new(x, screen.y, width, MENU_BAR_HEIGHT);
        x += width;
    }
    rects
}

/// Returns the clickable logo rectangle at the left of the menu bar.
#[must_use]
pub const fn menu_logo_rect(screen: Rect) -> Rect {
    Rect::new(screen.x + 10, screen.y + 5, 24, 17)
}

/// Right-hand menu-bar status items.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatusItems {
    /// Spotlight-style search.
    pub search: Rect,
    /// Workspace overview.
    pub spaces: Rect,
    /// Security Center.
    pub security: Rect,
    /// Uptime clock.
    pub clock: Rect,
}

/// Returns the status-item rectangles at the right of the menu bar.
#[must_use]
pub fn status_items(screen: Rect) -> StatusItems {
    let y = screen.y + 5;
    let clock = Rect::new(screen.right() - 62, y + 1, 52, 15);
    let security = Rect::new(clock.x - 25, y, 17, 17);
    let spaces = Rect::new(security.x - 25, y, 17, 17);
    let search = Rect::new(spaces.x - 25, y, 17, 17);
    StatusItems {
        search,
        spaces,
        security,
        clock,
    }
}

/// Returns the Spotlight-style search panel.
#[must_use]
pub fn spotlight_panel(screen: Rect) -> Rect {
    let width = 520.min(screen.width - 64);
    let height = 312.min(screen.height - 100);
    Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 3,
        width,
        height,
    )
}

/// Returns the Spotlight query field.
#[must_use]
pub const fn spotlight_field(panel: Rect) -> Rect {
    Rect::new(panel.x + 14, panel.y + 12, panel.width - 28, 34)
}

/// Returns one Spotlight result row.
#[must_use]
pub const fn spotlight_result_rect(panel: Rect, index: usize) -> Rect {
    Rect::new(
        panel.x + 8,
        panel.y + 54 + index as i32 * 28,
        panel.width - 16,
        26,
    )
}

/// Returns the rectangle anchoring a menu's drop-down panel.
#[must_use]
pub fn menu_anchor(screen: Rect, kind: MenuKind, app_name: &str) -> Rect {
    if kind == MenuKind::System {
        return menu_logo_rect(screen);
    }
    let index = kind.bar_index().unwrap_or(0);
    menu_title_rects(screen, app_name)[index]
}

/// Returns a drop-down panel rectangle for a menu with `items` entries.
#[must_use]
pub fn menu_panel_rect(screen: Rect, anchor: Rect, items: usize) -> Rect {
    let width = 262;
    let height = 10 + items as i32 * 24;
    let x = anchor.x.min(screen.right() - width - 8).max(screen.x + 8);
    let y = screen.y + MENU_BAR_HEIGHT + 2;
    Rect::new(
        x,
        y,
        width,
        height.min(screen.height - MENU_BAR_HEIGHT - 10),
    )
}

/// Returns one menu row rectangle.
#[must_use]
pub fn menu_item_rect(panel: Rect, index: usize) -> Rect {
    Rect::new(
        panel.x + 6,
        panel.y + 5 + index as i32 * 24,
        panel.width - 12,
        22,
    )
}

/// Returns the four workspace cards used by the overview.
#[must_use]
pub fn overview_cards(screen: Rect) -> [Rect; 4] {
    let columns = if screen.width >= 1100 { 4 } else { 2 };
    let rows = 4 / columns;
    let margin = (screen.width / 14).clamp(24, 96);
    let gap = 18;
    let top = screen.y + MENU_BAR_HEIGHT + 42;
    let bottom = screen.bottom() - DOCK_HEIGHT - DOCK_MARGIN * 2 - 24;
    let card_width = (screen.width - margin * 2 - gap * (columns as i32 - 1)) / columns as i32;
    let card_height = (bottom - top - gap * (rows as i32 - 1)) / rows as i32;
    let mut cards = [Rect::default(); 4];
    for (index, card) in cards.iter_mut().enumerate() {
        let column = index % columns;
        let row = index / columns;
        *card = Rect::new(
            screen.x + margin + column as i32 * (card_width + gap),
            top + row as i32 * (card_height + gap),
            card_width,
            card_height,
        );
    }
    cards
}

/// Returns preview rectangles inside a workspace card.
#[must_use]
pub fn overview_preview_rects(card: Rect) -> [Rect; 3] {
    let inner = Rect::new(card.x + 12, card.y + 34, card.width - 24, card.height - 46);
    let width = inner.width * 74 / 100;
    let height = (inner.height * 34 / 100).clamp(34, 110);
    let stack_height = height * 3 + 20;
    let top = inner.y + ((inner.height - stack_height).max(0) / 2).max(8);
    let mut rects = [Rect::default(); 3];
    for (index, rect) in rects.iter_mut().enumerate() {
        *rect = Rect::new(
            inner.x + 8 + index as i32 * 10,
            top + index as i32 * (height + 10),
            width,
            height,
        )
        .intersection(inner)
        .unwrap_or(inner);
    }
    rects
}
