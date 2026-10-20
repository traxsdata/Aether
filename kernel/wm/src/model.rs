// SPDX-License-Identifier: Apache-2.0
//! Window, workspace, focus, menu, dock, pointer, and keyboard state.
//!
//! The window manager is a bounded data structure with no authority: it cannot
//! open a file, mint a capability, or call a service. It decides presentation
//! and focus, while the kernel's filesystem, portal, and supervisor services
//! remain the authorities for their resources.

use crate::anim::Anim;
use crate::geometry::{Point, Rect};
use crate::layout::{self, DOCK_ITEMS, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH};
use crate::theme::{GlassLevel, Palette, ThemeKind};
pub use aether_input::{Key, Modifiers};

/// Maximum number of simultaneously allocated window slots.
pub const MAX_WINDOWS: usize = 10;
/// Number of virtual workspaces.
pub const WORKSPACES: u8 = 4;

const OPEN_TICKS: u16 = 14;
const CLOSE_TICKS: u16 = 12;
const MINIMIZE_TICKS: u16 = 14;
const WORKSPACE_TICKS: u16 = 18;
const OVERVIEW_TICKS: u16 = 14;
const DOCK_TICKS: u16 = 8;
const DOUBLE_CLICK_TICKS: u64 = 35;

/// A kind of native application window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowKind {
    /// File browser over the kernel file service.
    Files,
    /// Live security and isolation counters.
    Security,
    /// File-portal grants and denials.
    Portal,
    /// Appearance and accessibility settings.
    Settings,
    /// Current and previous system images.
    Recovery,
    /// Live diagnostic log.
    Log,
    /// Welcome, shortcuts, and clean-room notice.
    Welcome,
}

impl WindowKind {
    /// All application kinds in dock order.
    pub const ALL: [Self; 7] = [
        Self::Files,
        Self::Security,
        Self::Portal,
        Self::Settings,
        Self::Recovery,
        Self::Log,
        Self::Welcome,
    ];

    /// Window title.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Files => "FILES",
            Self::Security => "SECURITY CENTER",
            Self::Portal => "FILE PORTAL",
            Self::Settings => "SETTINGS",
            Self::Recovery => "RECOVERY",
            Self::Log => "SYSTEM LOG",
            Self::Welcome => "WELCOME",
        }
    }

    /// Short application name used in the menu bar and dock.
    #[must_use]
    pub const fn app_name(self) -> &'static str {
        match self {
            Self::Files => "Files",
            Self::Security => "Security",
            Self::Portal => "Portal",
            Self::Settings => "Settings",
            Self::Recovery => "Recovery",
            Self::Log => "Log",
            Self::Welcome => "Welcome",
        }
    }

    /// A one-line description used by settings and help views.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Files => "Browse the capability-scoped filesystem",
            Self::Security => "Verified state, denials, and isolation counters",
            Self::Portal => "Exact-file grants requested by applications",
            Self::Settings => "Appearance, motion, dock, and contrast",
            Self::Recovery => "Current and previous image records",
            Self::Log => "Live kernel and input counters",
            Self::Welcome => "Desktop guide and keyboard shortcuts",
        }
    }
}

/// The lifecycle state of a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowState {
    /// Ordinary floating window.
    Normal,
    /// Filling the work area.
    Maximized,
    /// Collapsed into the dock.
    Minimized,
}

/// A managed window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// Stable identifier.
    pub id: u8,
    /// Application kind.
    pub kind: WindowKind,
    /// Virtual workspace index.
    pub workspace: u8,
    /// Logical frame used for hit testing and persistence.
    pub frame: Rect,
    /// Frame restored when unzooming.
    pub restore_frame: Rect,
    /// Lifecycle state.
    pub state: WindowState,
    state_before_minimize: WindowState,
    /// Open animation progress.
    pub open: Anim,
    /// Minimize animation progress, where 256 is fully collapsed.
    pub minimize: Anim,
    /// Close animation progress.
    pub close: Anim,
    /// Whether the window is being removed.
    pub closing: bool,
}

impl Window {
    /// Returns `true` while a close animation is running.
    #[must_use]
    pub const fn is_closing(self) -> bool {
        self.closing
    }

    /// Returns `true` when the window should be painted.
    #[must_use]
    pub fn is_visible(self) -> bool {
        !(self.closing && self.close.is_complete()
            || self.state == WindowState::Minimized && self.minimize.is_complete())
    }

    /// Returns the frame to paint, after open, close, and minimize motion.
    #[must_use]
    pub fn animated_frame(self, minimize_target: Rect) -> Rect {
        let mut frame = self.frame;
        if self.minimize.value() > 0 {
            frame = frame.lerp(minimize_target, self.minimize.eased());
        }
        if self.open.value() < 256 && !self.closing {
            let remaining = 256 - self.open.value();
            let inset = remaining as i32 / 26;
            frame = Rect::new(
                frame.x + inset,
                frame.y + inset + remaining as i32 / 20,
                (frame.width - inset * 2).max(24),
                (frame.height - inset * 2).max(20),
            );
        }
        if self.closing {
            let closing = self.close.eased();
            let inset = closing as i32 / 13;
            frame = Rect::new(
                frame.x + inset,
                frame.y + inset + closing as i32 / 26,
                (frame.width - inset * 2).max(12),
                (frame.height - inset * 2).max(10),
            );
        }
        frame
    }
}

/// Appearance and accessibility preferences held in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Finish transitions without intermediate frames.
    pub reduce_motion: bool,
    /// Use the high-contrast palette.
    pub high_contrast: bool,
    /// Liquid Glass transparency level.
    pub glass: GlassLevel,
    /// Enlarge dock icons under the pointer.
    pub magnify_dock: bool,
}

impl Settings {
    /// Default settings.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            reduce_motion: false,
            high_contrast: false,
            glass: GlassLevel::Balanced,
            magnify_dock: true,
        }
    }

    /// Returns the active theme.
    #[must_use]
    pub const fn theme(self) -> ThemeKind {
        if self.high_contrast {
            ThemeKind::HighContrast
        } else {
            ThemeKind::Aurora
        }
    }

    /// Returns the active palette.
    #[must_use]
    pub const fn palette(self) -> Palette {
        self.theme().palette()
    }

    /// Returns the number of settings rows.
    #[must_use]
    pub const fn row_count() -> usize {
        4
    }

    /// Returns a row's label and detail text.
    #[must_use]
    pub const fn row(index: usize) -> (&'static str, &'static str) {
        match index {
            0 => (
                "Reduce motion",
                "Finish transitions without intermediate frames",
            ),
            1 => (
                "High contrast",
                "Black background, white text, signal-yellow focus",
            ),
            2 => ("Liquid Glass", "Clear, Balanced, or Tinted translucency"),
            3 => ("Magnify dock", "Enlarge dock icons under the pointer"),
            _ => ("", ""),
        }
    }

    /// Returns the current value of a row.
    #[must_use]
    pub const fn value(self, index: usize) -> bool {
        match index {
            0 => self.reduce_motion,
            1 => self.high_contrast,
            2 => true,
            3 => self.magnify_dock,
            _ => false,
        }
    }

    /// Returns the display value for rows with more than two states.
    #[must_use]
    pub const fn value_label(self, index: usize) -> &'static str {
        match index {
            2 => self.glass.label(),
            _ => "",
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::new()
    }
}

/// A top-level menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKind {
    /// The system menu anchored by the Aether mark.
    System,
    /// The focused application menu.
    App,
    /// File actions.
    File,
    /// View, workspace, and accessibility actions.
    View,
    /// Window actions.
    Window,
    /// Help and about.
    Help,
}

impl MenuKind {
    /// All menus in keyboard navigation order.
    pub const ALL: [Self; 6] = [
        Self::System,
        Self::App,
        Self::File,
        Self::View,
        Self::Window,
        Self::Help,
    ];
    /// Menu titles displayed after the application name.
    pub const BAR: [Self; 5] = [Self::App, Self::File, Self::View, Self::Window, Self::Help];

    /// Returns the index in [`Self::BAR`] for a bar menu.
    #[must_use]
    pub const fn bar_index(self) -> Option<usize> {
        match self {
            Self::App => Some(0),
            Self::File => Some(1),
            Self::View => Some(2),
            Self::Window => Some(3),
            Self::Help => Some(4),
            Self::System => None,
        }
    }

    /// Returns the menu title.
    #[must_use]
    pub const fn label(self, app_name: &'static str) -> &'static str {
        match self {
            Self::System => "Aether",
            Self::App => app_name,
            Self::File => "File",
            Self::View => "View",
            Self::Window => "Window",
            Self::Help => "Help",
        }
    }

    /// Returns the menu's items.
    #[must_use]
    pub const fn items(self) -> &'static [MenuItem] {
        match self {
            Self::System => &SYSTEM_ITEMS,
            Self::App => &APP_ITEMS,
            Self::File => &FILE_ITEMS,
            Self::View => &VIEW_ITEMS,
            Self::Window => &WINDOW_ITEMS,
            Self::Help => &HELP_ITEMS,
        }
    }
}

/// An action a menu item or shortcut can invoke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Separator or no action.
    None,
    /// Open the welcome window.
    About,
    /// Open or focus an application.
    Open(WindowKind),
    /// Close the focused window.
    CloseFocused,
    /// Minimize the focused window.
    MinimizeFocused,
    /// Zoom or unzoom the focused window.
    ZoomFocused,
    /// Focus the next window.
    NextWindow,
    /// Focus the previous window.
    PreviousWindow,
    /// Show the workspace overview.
    ShowOverview,
    /// Switch to a workspace.
    Workspace(u8),
    /// Toggle reduced motion.
    ToggleReduceMotion,
    /// Toggle the high-contrast palette.
    ToggleHighContrast,
    /// Cycle the Liquid Glass transparency level.
    ToggleLiquidGlass,
    /// Toggle dock magnification.
    ToggleDockMagnification,
}

/// One menu row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuItem {
    /// Label, or `-` for a separator.
    pub label: &'static str,
    /// Optional shortcut text.
    pub shortcut: &'static str,
    /// Command to execute.
    pub command: Command,
}

impl MenuItem {
    /// Returns `true` for a separator.
    #[must_use]
    pub const fn is_separator(self) -> bool {
        self.label.len() == 1 && self.label.as_bytes()[0] == b'-'
    }
}

/// One command offered by Spotlight-style search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchItem {
    /// Result title.
    pub label: &'static str,
    /// Result category.
    pub kind: &'static str,
    /// Command to execute.
    pub command: Command,
}

const SEARCH_ITEMS: [SearchItem; 15] = [
    SearchItem {
        label: "Open Files",
        kind: "Application",
        command: Command::Open(WindowKind::Files),
    },
    SearchItem {
        label: "Open Security Center",
        kind: "Application",
        command: Command::Open(WindowKind::Security),
    },
    SearchItem {
        label: "Open File Portal",
        kind: "Application",
        command: Command::Open(WindowKind::Portal),
    },
    SearchItem {
        label: "Open System Settings",
        kind: "Application",
        command: Command::Open(WindowKind::Settings),
    },
    SearchItem {
        label: "Open Recovery",
        kind: "Application",
        command: Command::Open(WindowKind::Recovery),
    },
    SearchItem {
        label: "Open System Log",
        kind: "Application",
        command: Command::Open(WindowKind::Log),
    },
    SearchItem {
        label: "Open Welcome",
        kind: "Application",
        command: Command::Open(WindowKind::Welcome),
    },
    SearchItem {
        label: "Show All Windows",
        kind: "Window",
        command: Command::ShowOverview,
    },
    SearchItem {
        label: "Space 1",
        kind: "Workspace",
        command: Command::Workspace(0),
    },
    SearchItem {
        label: "Space 2",
        kind: "Workspace",
        command: Command::Workspace(1),
    },
    SearchItem {
        label: "Space 3",
        kind: "Workspace",
        command: Command::Workspace(2),
    },
    SearchItem {
        label: "Space 4",
        kind: "Workspace",
        command: Command::Workspace(3),
    },
    SearchItem {
        label: "Toggle Reduce Motion",
        kind: "Accessibility",
        command: Command::ToggleReduceMotion,
    },
    SearchItem {
        label: "Toggle High Contrast",
        kind: "Accessibility",
        command: Command::ToggleHighContrast,
    },
    SearchItem {
        label: "Cycle Liquid Glass",
        kind: "Appearance",
        command: Command::ToggleLiquidGlass,
    },
];

const SYSTEM_ITEMS: [MenuItem; 13] = [
    MenuItem {
        label: "About This Aether",
        shortcut: "",
        command: Command::About,
    },
    MenuItem {
        label: "-",
        shortcut: "",
        command: Command::None,
    },
    MenuItem {
        label: "System Settings",
        shortcut: "Ctrl+,",
        command: Command::Open(WindowKind::Settings),
    },
    MenuItem {
        label: "Files",
        shortcut: "",
        command: Command::Open(WindowKind::Files),
    },
    MenuItem {
        label: "Security Center",
        shortcut: "",
        command: Command::Open(WindowKind::Security),
    },
    MenuItem {
        label: "File Portal",
        shortcut: "",
        command: Command::Open(WindowKind::Portal),
    },
    MenuItem {
        label: "System Log",
        shortcut: "",
        command: Command::Open(WindowKind::Log),
    },
    MenuItem {
        label: "-",
        shortcut: "",
        command: Command::None,
    },
    MenuItem {
        label: "Show All Windows",
        shortcut: "Ctrl+Up",
        command: Command::ShowOverview,
    },
    MenuItem {
        label: "-",
        shortcut: "",
        command: Command::None,
    },
    MenuItem {
        label: "Reduce Motion",
        shortcut: "",
        command: Command::ToggleReduceMotion,
    },
    MenuItem {
        label: "High Contrast",
        shortcut: "",
        command: Command::ToggleHighContrast,
    },
    MenuItem {
        label: "Magnify Dock",
        shortcut: "",
        command: Command::ToggleDockMagnification,
    },
];

const APP_ITEMS: [MenuItem; 8] = [
    MenuItem {
        label: "Close Window",
        shortcut: "Ctrl+W",
        command: Command::CloseFocused,
    },
    MenuItem {
        label: "Minimize",
        shortcut: "Ctrl+M",
        command: Command::MinimizeFocused,
    },
    MenuItem {
        label: "Zoom",
        shortcut: "Ctrl+F",
        command: Command::ZoomFocused,
    },
    MenuItem {
        label: "-",
        shortcut: "",
        command: Command::None,
    },
    MenuItem {
        label: "Next Window",
        shortcut: "Ctrl+Tab",
        command: Command::NextWindow,
    },
    MenuItem {
        label: "Previous Window",
        shortcut: "Ctrl+Shift+Tab",
        command: Command::PreviousWindow,
    },
    MenuItem {
        label: "-",
        shortcut: "",
        command: Command::None,
    },
    MenuItem {
        label: "System Settings",
        shortcut: "Ctrl+,",
        command: Command::Open(WindowKind::Settings),
    },
];

const FILE_ITEMS: [MenuItem; 5] = [
    MenuItem {
        label: "Open Files",
        shortcut: "Ctrl+N",
        command: Command::Open(WindowKind::Files),
    },
    MenuItem {
        label: "File Portal",
        shortcut: "",
        command: Command::Open(WindowKind::Portal),
    },
    MenuItem {
        label: "-",
        shortcut: "",
        command: Command::None,
    },
    MenuItem {
        label: "Close Window",
        shortcut: "Ctrl+W",
        command: Command::CloseFocused,
    },
    MenuItem {
        label: "Minimize",
        shortcut: "Ctrl+M",
        command: Command::MinimizeFocused,
    },
];

const VIEW_ITEMS: [MenuItem; 10] = [
    MenuItem {
        label: "Show All Windows",
        shortcut: "Ctrl+Up",
        command: Command::ShowOverview,
    },
    MenuItem {
        label: "-",
        shortcut: "",
        command: Command::None,
    },
    MenuItem {
        label: "Space 1",
        shortcut: "Ctrl+1",
        command: Command::Workspace(0),
    },
    MenuItem {
        label: "Space 2",
        shortcut: "Ctrl+2",
        command: Command::Workspace(1),
    },
    MenuItem {
        label: "Space 3",
        shortcut: "Ctrl+3",
        command: Command::Workspace(2),
    },
    MenuItem {
        label: "Space 4",
        shortcut: "Ctrl+4",
        command: Command::Workspace(3),
    },
    MenuItem {
        label: "-",
        shortcut: "",
        command: Command::None,
    },
    MenuItem {
        label: "Reduce Motion",
        shortcut: "",
        command: Command::ToggleReduceMotion,
    },
    MenuItem {
        label: "High Contrast",
        shortcut: "",
        command: Command::ToggleHighContrast,
    },
    MenuItem {
        label: "Magnify Dock",
        shortcut: "",
        command: Command::ToggleDockMagnification,
    },
];

const WINDOW_ITEMS: [MenuItem; 6] = [
    MenuItem {
        label: "Minimize",
        shortcut: "Ctrl+M",
        command: Command::MinimizeFocused,
    },
    MenuItem {
        label: "Zoom",
        shortcut: "Ctrl+F",
        command: Command::ZoomFocused,
    },
    MenuItem {
        label: "Next Window",
        shortcut: "Ctrl+Tab",
        command: Command::NextWindow,
    },
    MenuItem {
        label: "Previous Window",
        shortcut: "Ctrl+Shift+Tab",
        command: Command::PreviousWindow,
    },
    MenuItem {
        label: "-",
        shortcut: "",
        command: Command::None,
    },
    MenuItem {
        label: "Close Window",
        shortcut: "Ctrl+W",
        command: Command::CloseFocused,
    },
];

const HELP_ITEMS: [MenuItem; 4] = [
    MenuItem {
        label: "Welcome to Aether",
        shortcut: "",
        command: Command::Open(WindowKind::Welcome),
    },
    MenuItem {
        label: "Keyboard Shortcuts",
        shortcut: "",
        command: Command::Open(WindowKind::Welcome),
    },
    MenuItem {
        label: "-",
        shortcut: "",
        command: Command::None,
    },
    MenuItem {
        label: "About This Aether",
        shortcut: "",
        command: Command::About,
    },
];

/// An open menu and its keyboard selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuState {
    /// Which menu is open.
    pub kind: MenuKind,
    /// Selected row.
    pub selected: usize,
    /// Whether the selection should be drawn as a keyboard highlight.
    pub keyboard: bool,
}

/// The result of a hit test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// Nothing interactive.
    None,
    /// The menu bar background.
    MenuBar,
    /// The Aether mark.
    MenuLogo,
    /// A top-level menu title.
    MenuTitle(MenuKind),
    /// A row inside an open menu.
    MenuItem(usize),
    /// The open menu's background.
    MenuPanel,
    /// A Spotlight-style search result.
    SpotlightResult(usize),
    /// The Spotlight panel background.
    SpotlightPanel,
    /// The Spotlight query field.
    SpotlightField,
    /// The search status item.
    StatusSearch,
    /// The workspace status item.
    StatusSpaces,
    /// The security status item.
    StatusSecurity,
    /// A dock icon.
    DockIcon(usize),
    /// The dock background.
    DockPanel,
    /// A workspace card.
    OverviewCard(u8),
    /// The overview background.
    OverviewPanel,
    /// A window body.
    Window(u8),
    /// A window title bar.
    TitleBar(u8),
    /// A close control.
    Close(u8),
    /// A minimize control.
    Minimize(u8),
    /// A zoom control.
    Zoom(u8),
    /// A resize grip.
    Resize(u8),
    /// A settings row.
    SettingsRow(u8, usize),
}

/// The result of a key event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyOutcome {
    /// Whether the desktop must repaint.
    pub changed: bool,
    /// Optional action label for the serial transcript.
    pub announce: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Drag {
    id: u8,
    resize: bool,
    origin: Point,
    frame: Rect,
}

/// The workspace slide transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceTransition {
    /// Whether a transition is running.
    pub active: bool,
    /// Workspace leaving the screen.
    pub from: u8,
    /// Workspace entering the screen.
    pub to: u8,
    /// Positive when moving to a higher workspace.
    pub direction: i8,
    /// Progress animation.
    pub anim: Anim,
}

impl WorkspaceTransition {
    const fn idle() -> Self {
        Self {
            active: false,
            from: 0,
            to: 0,
            direction: 1,
            anim: Anim::COMPLETE,
        }
    }
}

/// The complete window-manager state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wm {
    screen: Rect,
    windows: [Option<Window>; MAX_WINDOWS],
    order: [u8; MAX_WINDOWS],
    order_len: usize,
    next_id: u8,
    workspace: u8,
    focus: Option<u8>,
    transition: WorkspaceTransition,
    overview_open: bool,
    overview: Anim,
    overview_selected: u8,
    spotlight_open: bool,
    spotlight_query: [u8; 32],
    spotlight_len: usize,
    spotlight_selected: usize,
    menu: Option<MenuState>,
    cursor: Point,
    dock_hover: [u16; DOCK_ITEMS],
    dock_target: Option<usize>,
    dragging: Option<Drag>,
    last_click: Option<(u8, u64)>,
    settings: Settings,
}

impl Wm {
    /// Creates an empty window manager for a screen.
    #[must_use]
    pub fn new(screen: Rect) -> Self {
        Self {
            screen,
            windows: [None; MAX_WINDOWS],
            order: [0; MAX_WINDOWS],
            order_len: 0,
            next_id: 1,
            workspace: 0,
            focus: None,
            transition: WorkspaceTransition::idle(),
            overview_open: false,
            overview: Anim::ZERO,
            overview_selected: 0,
            spotlight_open: false,
            spotlight_query: [0; 32],
            spotlight_len: 0,
            spotlight_selected: 0,
            menu: None,
            cursor: screen.center(),
            dock_hover: [0; DOCK_ITEMS],
            dock_target: None,
            dragging: None,
            last_click: None,
            settings: Settings::new(),
        }
    }

    /// Populates the initial desktop with three windows.
    pub fn seed(&mut self) {
        let _ = self.open(WindowKind::Files);
        let _ = self.open(WindowKind::Security);
        let _ = self.open(WindowKind::Welcome);
        for window in self.windows.iter_mut().flatten() {
            window.open.finish();
        }
        self.focus = self
            .find_kind(WindowKind::Welcome)
            .or_else(|| self.focused_id());
        self.bring_to_front(self.focus);
    }

    /// Returns the screen rectangle.
    #[must_use]
    pub const fn screen(self) -> Rect {
        self.screen
    }

    /// Returns all window slots.
    #[must_use]
    pub const fn windows(&self) -> &[Option<Window>; MAX_WINDOWS] {
        &self.windows
    }

    /// Returns a window by identifier.
    #[must_use]
    pub fn window(&self, id: u8) -> Option<&Window> {
        self.window_index(id)
            .and_then(|index| self.windows[index].as_ref())
    }

    /// Returns the focused window identifier.
    #[must_use]
    pub const fn focus_id(&self) -> Option<u8> {
        self.focus
    }

    /// Returns the focused window.
    #[must_use]
    pub fn focused_window(&self) -> Option<&Window> {
        self.focus.and_then(|id| self.window(id))
    }

    /// Returns the focused window title, or a fallback label.
    #[must_use]
    pub fn focus_title(&self) -> &'static str {
        self.focused_window()
            .map_or("AETHER", |window| window.kind.title())
    }

    /// Returns the active application name shown in the menu bar.
    #[must_use]
    pub fn app_name(&self) -> &'static str {
        self.focused_window()
            .map_or("Aether", |window| window.kind.app_name())
    }

    /// Returns the current workspace.
    #[must_use]
    pub const fn workspace(self) -> u8 {
        self.workspace
    }

    /// Returns the number of allocated, non-closing windows.
    #[must_use]
    pub fn window_count(&self) -> usize {
        self.windows
            .iter()
            .flatten()
            .filter(|window| !window.closing)
            .count()
    }

    /// Returns the number of windows on a workspace.
    #[must_use]
    pub fn workspace_window_count(&self, workspace: u8) -> usize {
        self.windows
            .iter()
            .flatten()
            .filter(|window| window.workspace == workspace && !window.closing)
            .count()
    }

    /// Returns the z-order, back to front.
    #[must_use]
    pub fn order(&self) -> &[u8] {
        &self.order[..self.order_len]
    }

    /// Returns the current workspace transition.
    #[must_use]
    pub const fn transition(self) -> WorkspaceTransition {
        self.transition
    }

    /// Returns `true` when the overview is open.
    #[must_use]
    pub const fn overview_open(self) -> bool {
        self.overview_open
    }

    /// Returns the overview animation progress.
    #[must_use]
    pub const fn overview_progress(self) -> u16 {
        self.overview.value()
    }

    /// Returns the selected workspace card.
    #[must_use]
    pub const fn overview_selection(self) -> u8 {
        self.overview_selected
    }

    /// Returns `true` when Spotlight-style search is open.
    #[must_use]
    pub const fn spotlight_open(self) -> bool {
        self.spotlight_open
    }

    /// Returns the current search query.
    #[must_use]
    pub fn spotlight_query(&self) -> &str {
        core::str::from_utf8(&self.spotlight_query[..self.spotlight_len]).unwrap_or("")
    }

    /// Returns the selected search result.
    #[must_use]
    pub const fn spotlight_selection(self) -> usize {
        self.spotlight_selected
    }

    /// Returns the number of visible search results, capped to one panel.
    #[must_use]
    pub fn search_result_count(&self) -> usize {
        self.filtered_results().take(8).count()
    }

    /// Returns one filtered search result.
    #[must_use]
    pub fn search_result(&self, index: usize) -> Option<&'static SearchItem> {
        self.filtered_results().nth(index)
    }

    fn filtered_results(&self) -> impl Iterator<Item = &'static SearchItem> + '_ {
        let query = &self.spotlight_query[..self.spotlight_len];
        SEARCH_ITEMS
            .iter()
            .filter(move |item| search_matches(item.label.as_bytes(), query))
    }

    /// Opens Spotlight-style search with a clean query.
    pub fn open_spotlight(&mut self) {
        self.spotlight_open = true;
        self.spotlight_query = [0; 32];
        self.spotlight_len = 0;
        self.spotlight_selected = 0;
        self.menu = None;
        self.overview_open = false;
        self.overview.retarget(0, 0);
    }

    /// Closes Spotlight-style search.
    pub fn close_spotlight(&mut self) {
        self.spotlight_open = false;
    }

    /// Returns the open menu.
    #[must_use]
    pub const fn menu_state(self) -> Option<MenuState> {
        self.menu
    }

    /// Returns the pointer position.
    #[must_use]
    pub const fn cursor(self) -> Point {
        self.cursor
    }

    /// Returns dock hover values.
    #[must_use]
    pub const fn dock_hover(&self) -> &[u16; DOCK_ITEMS] {
        &self.dock_hover
    }

    /// Returns the settings.
    #[must_use]
    pub const fn settings(self) -> Settings {
        self.settings
    }

    /// Returns the active palette.
    #[must_use]
    pub const fn palette(self) -> Palette {
        self.settings.palette()
    }

    /// Returns `true` when an application owns at least one window.
    #[must_use]
    pub fn is_running(&self, kind: WindowKind) -> bool {
        self.windows
            .iter()
            .flatten()
            .any(|window| window.kind == kind && !window.closing)
    }

    /// Returns the identifier of an application's window, if it has one.
    #[must_use]
    pub fn window_id(&self, kind: WindowKind) -> Option<u8> {
        self.find_kind(kind)
    }

    /// Returns `true` when the application is focused.
    #[must_use]
    pub fn is_active(&self, kind: WindowKind) -> bool {
        self.focused_window()
            .is_some_and(|window| window.kind == kind)
    }

    /// Opens or focuses an application window on the current workspace.
    ///
    /// If an instance already exists on another workspace, the manager switches
    /// to that workspace instead of silently creating a second instance.
    pub fn open(&mut self, kind: WindowKind) -> Option<u8> {
        self.menu = None;
        self.spotlight_open = false;
        self.overview_open = false;
        self.overview.retarget(0, 0);
        if let Some(id) = self.find_kind(kind) {
            let workspace = self.window(id).map(|window| window.workspace)?;
            if workspace != self.workspace {
                self.switch_workspace(workspace);
            }
            if let Some(index) = self.window_index(id)
                && let Some(window) = &mut self.windows[index]
                && window.closing
            {
                window.closing = false;
                window.close.retarget(0, 0);
                window.open.finish();
            }
            self.restore_window(id);
            self.focus_window(id);
            return Some(id);
        }
        let slot = self.windows.iter().position(Option::is_none)?;
        // IDs wrap in a long session. Never alias a still-live window.
        let id = (0..=MAX_WINDOWS).find_map(|_| {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            self.window(id).is_none().then_some(id)
        })?;
        let cascade = i32::from(id % 5) * 16;
        let frame = layout::default_window_frame(kind, self.screen, cascade);
        let mut open = Anim::starting(OPEN_TICKS);
        if self.settings.reduce_motion {
            open.finish();
        }
        self.windows[slot] = Some(Window {
            id,
            kind,
            workspace: self.workspace,
            frame,
            restore_frame: frame,
            state: WindowState::Normal,
            state_before_minimize: WindowState::Normal,
            open,
            minimize: Anim::at(0),
            close: Anim::at(0),
            closing: false,
        });
        self.order[self.order_len] = id;
        self.order_len += 1;
        self.focus = Some(id);
        self.menu = None;
        Some(id)
    }

    /// Closes a window, playing its close animation.
    pub fn close_window(&mut self, id: u8) {
        let Some(index) = self.window_index(id) else {
            return;
        };
        if self.windows[index].is_some_and(|window| window.closing) {
            return;
        }
        if let Some(window) = &mut self.windows[index] {
            window.closing = true;
            window.close = Anim::starting(CLOSE_TICKS);
            if self.settings.reduce_motion {
                window.close.finish();
            }
        }
        if self.focus == Some(id) {
            self.focus = None;
            self.focus_topmost();
        }
        if self.dragging.is_some_and(|drag| drag.id == id) {
            self.dragging = None;
        }
    }

    /// Minimizes a window into the dock.
    pub fn minimize_window(&mut self, id: u8) {
        let Some(index) = self.window_index(id) else {
            return;
        };
        if let Some(window) = &mut self.windows[index] {
            if window.state == WindowState::Minimized || window.closing {
                return;
            }
            window.state_before_minimize = window.state;
            window.state = WindowState::Minimized;
            window.minimize.retarget(
                256,
                if self.settings.reduce_motion {
                    0
                } else {
                    MINIMIZE_TICKS
                },
            );
        }
        if self.focus == Some(id) {
            self.focus = None;
            self.focus_topmost();
        }
        if self.dragging.is_some_and(|drag| drag.id == id) {
            self.dragging = None;
        }
    }

    /// Restores a minimized window without changing z-order.
    pub fn restore_window(&mut self, id: u8) {
        let Some(index) = self.window_index(id) else {
            return;
        };
        if let Some(window) = &mut self.windows[index] {
            if window.state != WindowState::Minimized || window.closing {
                return;
            }
            window.state = window.state_before_minimize;
            window.minimize.retarget(
                0,
                if self.settings.reduce_motion {
                    0
                } else {
                    MINIMIZE_TICKS
                },
            );
        }
    }

    /// Toggles a window between floating and the work area.
    pub fn zoom_window(&mut self, id: u8) {
        self.restore_window(id);
        let area = layout::work_area(self.screen).inset(6);
        let Some(index) = self.window_index(id) else {
            return;
        };
        if let Some(window) = &mut self.windows[index] {
            if window.state == WindowState::Maximized {
                window.frame = window.restore_frame.clamp_inside(area);
                window.state = WindowState::Normal;
            } else {
                window.restore_frame = window.frame;
                window.frame = area;
                window.state = WindowState::Maximized;
                window.minimize.retarget(0, 0);
            }
        }
        self.focus_window(id);
    }

    /// Focuses a window and raises it to the front.
    pub fn focus_window(&mut self, id: u8) {
        let Some(window) = self.window(id).copied() else {
            return;
        };
        if window.closing {
            return;
        }
        self.switch_workspace(window.workspace);
        self.restore_window(id);
        self.focus = Some(id);
        self.bring_to_front(Some(id));
        self.menu = None;
        self.spotlight_open = false;
    }

    /// Cycles focus through windows on the current workspace.
    pub fn focus_next(&mut self, direction: i8) {
        if self.order_len == 0 {
            self.focus = None;
            return;
        }
        // Cycle stable allocation slots, not z-order: raising the selected
        // window must not make reverse cycling bounce between just two windows.
        let current_position = self.focus.and_then(|id| self.window_index(id));
        let len = MAX_WINDOWS as i32;
        let step = if direction >= 0 { 1 } else { -1 };
        let start = current_position.map_or(if step > 0 { -1 } else { 0 }, |value| value as i32);
        for offset in 1..=len {
            let position = (start + step * offset).rem_euclid(len) as usize;
            let Some(id) = self.windows[position].map(|window| window.id) else {
                continue;
            };
            if self.is_eligible(id) {
                self.focus = Some(id);
                self.bring_to_front(Some(id));
                return;
            }
        }
        self.focus = None;
    }

    /// Moves the focused window by a screen-space delta.
    pub fn move_focused(&mut self, dx: i32, dy: i32) {
        let Some(id) = self.focus else {
            return;
        };
        let Some(index) = self.window_index(id) else {
            return;
        };
        let area = layout::work_area(self.screen);
        if let Some(window) = &mut self.windows[index]
            && window.state != WindowState::Maximized
        {
            window.frame = window.frame.translate(dx, dy).clamp_inside(area);
        }
    }

    /// Resizes the focused floating window, with the same bounds as a drag.
    pub fn resize_focused(&mut self, dx: i32, dy: i32) {
        let Some(window) = self.focused_window().copied() else {
            return;
        };
        if window.state != WindowState::Normal || window.closing {
            return;
        }
        self.apply_drag(
            Drag {
                id: window.id,
                resize: true,
                origin: Point::default(),
                frame: window.frame,
            },
            Point::new(dx, dy),
        );
    }

    /// Returns whether a pointer move is changing window geometry.
    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        self.dragging.is_some()
    }

    /// Returns the frame actually painted, including workspace and window motion.
    #[must_use]
    pub fn window_frame(&self, id: u8) -> Option<Rect> {
        let window = *self.window(id)?;
        if !window.is_visible() {
            return None;
        }
        let offset = if self.transition.active {
            let progress = i32::from(self.transition.anim.eased());
            let direction = i32::from(self.transition.direction);
            if window.workspace == self.transition.from {
                -direction * progress * self.screen.width / 256
            } else if window.workspace == self.transition.to {
                direction * (256 - progress) * self.screen.width / 256
            } else {
                return None;
            }
        } else if window.workspace == self.workspace {
            0
        } else {
            return None;
        };
        let icons = layout::dock_icon_rects(self.screen, &self.dock_hover);
        let target = layout::DOCK_SLOTS
            .iter()
            .position(|slot| *slot == layout::DockSlot::App(window.kind))
            .map_or(window.frame, |index| icons[index]);
        Some(window.animated_frame(target).translate(offset, 0))
    }

    /// Bounds of scene changes, excluding the cursor. Shadows are included.
    ///
    /// Repaint all intersecting layers inside these bounds, not just the window
    /// that moved. Overlays/workspace transitions conservatively damage all.
    #[must_use]
    pub fn damage_since(&self, before: &Self) -> Option<Rect> {
        if self.screen != before.screen
            || self.settings != before.settings
            || self.transition.active
            || before.transition.active
            || self.overview_progress() != before.overview_progress()
            || self.overview_open != before.overview_open
            || self.overview_selected != before.overview_selected
            || (self.overview_open
                && (self.windows != before.windows || self.order != before.order))
            || self.spotlight_open != before.spotlight_open
            || self.spotlight_query != before.spotlight_query
            || self.spotlight_len != before.spotlight_len
            || self.spotlight_selected != before.spotlight_selected
        {
            return Some(self.screen);
        }
        let mut damage: Option<Rect> = None;
        let mut include = |rect: Rect| {
            damage = Some(damage.map_or(rect, |old| old.union(rect)));
        };
        for id in self.order().iter().chain(before.order()) {
            if self.window(*id) != before.window(*id)
                || (self.focus != before.focus
                    && (self.focus == Some(*id) || before.focus == Some(*id)))
                || self.workspace != before.workspace
            {
                if let Some(frame) = before.window_frame(*id) {
                    include(frame.inset(-24));
                }
                if let Some(frame) = self.window_frame(*id) {
                    include(frame.inset(-24));
                }
            }
        }
        if self.focus != before.focus
            || self.workspace != before.workspace
            || self.menu != before.menu
        {
            include(layout::menu_bar(self.screen));
        }
        if self.menu != before.menu {
            if let Some(panel) = before.menu_panel() {
                include(panel.inset(-10));
            }
            if let Some(panel) = self.menu_panel() {
                include(panel.inset(-10));
            }
        }
        if self.dock_hover != before.dock_hover
            || self.window_count() != before.window_count()
            || self.focus != before.focus
        {
            let dock = layout::dock_panel(self.screen);
            include(Rect::new(
                dock.x - 20,
                dock.y - 52,
                dock.width + 40,
                dock.height + 72,
            ));
        }
        let previous_hit = before.hit_test(before.cursor);
        let hit = self.hit_test(self.cursor);
        if previous_hit != hit {
            for (wm, hit) in [(before, previous_hit), (self, hit)] {
                match hit {
                    Hit::Close(id)
                    | Hit::Minimize(id)
                    | Hit::Zoom(id)
                    | Hit::SettingsRow(id, _) => {
                        if let Some(frame) = wm.window_frame(id) {
                            include(frame);
                        }
                    }
                    Hit::MenuTitle(_) | Hit::MenuLogo => include(layout::menu_bar(self.screen)),
                    Hit::MenuItem(_) | Hit::MenuPanel => {
                        if let Some(panel) = wm.menu_panel() {
                            include(panel.inset(-10));
                        }
                    }
                    Hit::SpotlightResult(_) | Hit::SpotlightPanel | Hit::SpotlightField => {
                        include(layout::spotlight_panel(self.screen).inset(-16))
                    }
                    Hit::DockIcon(_) | Hit::DockPanel => {
                        let dock = layout::dock_panel(self.screen);
                        include(Rect::new(
                            dock.x - 20,
                            dock.y - 52,
                            dock.width + 40,
                            dock.height + 72,
                        ));
                    }
                    _ => {}
                }
            }
        }
        damage.and_then(|rect| rect.intersection(self.screen))
    }

    /// Switches to a workspace, sliding the window layers.
    pub fn switch_workspace(&mut self, target: u8) {
        let target = target.min(WORKSPACES - 1);
        if target == self.workspace {
            return;
        }
        if self.transition.active {
            self.transition.active = false;
        }
        let direction = if target > self.workspace { 1 } else { -1 };
        let from = self.workspace;
        self.workspace = target;
        self.dragging = None;
        self.menu = None;
        self.overview_open = false;
        self.spotlight_open = false;
        self.overview.retarget(0, 0);
        self.transition = WorkspaceTransition {
            active: !self.settings.reduce_motion,
            from,
            to: target,
            direction,
            anim: Anim::starting(if self.settings.reduce_motion {
                0
            } else {
                WORKSPACE_TICKS
            }),
        };
        self.focus = None;
        self.focus_topmost();
    }

    /// Switches to the next or previous workspace with wraparound.
    pub fn cycle_workspace(&mut self, direction: i8) {
        let target = if direction >= 0 {
            (self.workspace + 1) % WORKSPACES
        } else {
            (self.workspace + WORKSPACES - 1) % WORKSPACES
        };
        self.switch_workspace(target);
    }

    /// Opens or closes the workspace overview.
    pub fn toggle_overview(&mut self) {
        self.menu = None;
        self.spotlight_open = false;
        if self.overview_open {
            self.overview_open = false;
            self.overview.retarget(
                0,
                if self.settings.reduce_motion {
                    0
                } else {
                    OVERVIEW_TICKS
                },
            );
        } else {
            self.overview_open = true;
            self.overview_selected = self.workspace;
            self.overview.retarget(
                256,
                if self.settings.reduce_motion {
                    0
                } else {
                    OVERVIEW_TICKS
                },
            );
        }
    }

    /// Applies a command from a menu.
    pub fn execute(&mut self, command: Command) {
        match command {
            Command::None => {}
            Command::About => {
                self.open(WindowKind::Welcome);
            }
            Command::Open(kind) => {
                self.open(kind);
            }
            Command::CloseFocused => {
                if let Some(id) = self.focus {
                    self.close_window(id);
                }
            }
            Command::MinimizeFocused => {
                if let Some(id) = self.focus {
                    self.minimize_window(id);
                }
            }
            Command::ZoomFocused => {
                if let Some(id) = self.focus {
                    self.zoom_window(id);
                }
            }
            Command::NextWindow => self.focus_next(1),
            Command::PreviousWindow => self.focus_next(-1),
            Command::ShowOverview => {
                if !self.overview_open {
                    self.toggle_overview();
                }
            }
            Command::Workspace(index) => self.switch_workspace(index),
            Command::ToggleReduceMotion => self.toggle_setting(0),
            Command::ToggleHighContrast => self.toggle_setting(1),
            Command::ToggleLiquidGlass => self.toggle_setting(2),
            Command::ToggleDockMagnification => self.toggle_setting(3),
        }
    }

    /// Toggles an accessibility or appearance setting.
    pub fn toggle_setting(&mut self, row: usize) {
        match row {
            0 => {
                self.settings.reduce_motion = !self.settings.reduce_motion;
                if self.settings.reduce_motion {
                    self.finish_animations();
                }
            }
            1 => self.settings.high_contrast = !self.settings.high_contrast,
            2 => self.settings.glass = self.settings.glass.next(),
            3 => {
                self.settings.magnify_dock = !self.settings.magnify_dock;
                self.dock_target = layout::dock_slot_at(self.cursor, self.screen);
                self.dock_hover = [0; DOCK_ITEMS];
            }
            _ => {}
        }
    }

    /// Returns the rectangle of a settings row.
    #[must_use]
    pub fn settings_row_rect(&self, id: u8, row: usize) -> Option<Rect> {
        let window = self.window(id)?;
        if window.kind != WindowKind::Settings || row >= Settings::row_count() {
            return None;
        }
        let content = layout::content_rect(self.window_frame(id)?);
        let width = content.width - 32;
        let y = content.y + 54 + row as i32 * 32;
        let height = 30;
        if y + height > content.bottom() - 6 {
            return None;
        }
        Some(Rect::new(content.x + 16, y, width, height))
    }

    /// Returns the settings row at a point in a settings window.
    #[must_use]
    pub fn settings_row_at(&self, id: u8, point: Point) -> Option<usize> {
        (0..Settings::row_count()).find(|row| {
            self.settings_row_rect(id, *row)
                .is_some_and(|rect| rect.contains(point))
        })
    }

    /// Advances every animation by `ticks` timer ticks.
    ///
    /// Returns `true` when a repaint is required.
    pub fn tick(&mut self, ticks: u16) -> bool {
        if ticks == 0 {
            return false;
        }
        let mut changed = false;

        if self.transition.active {
            changed |= self.transition.anim.advance(ticks);
            if self.transition.anim.is_complete() {
                self.transition.active = false;
            }
        }

        changed |= self.overview.advance(ticks);

        for window in self.windows.iter_mut().flatten() {
            changed |= window.open.advance(ticks);
            changed |= window.minimize.advance(ticks);
            if window.closing {
                changed |= window.close.advance(ticks);
            }
        }

        let mut index = 0;
        while index < MAX_WINDOWS {
            let finished = self.windows[index]
                .is_some_and(|window| window.closing && window.close.is_complete());
            if finished {
                let id = self.windows[index].map(|window| window.id).unwrap_or(0);
                self.windows[index] = None;
                self.remove_from_order(id);
                if self.focus == Some(id) {
                    self.focus = None;
                }
                changed = true;
            }
            index += 1;
        }
        if self.focus.is_none() {
            self.focus_topmost();
        }

        for (index, hover) in self.dock_hover.iter_mut().enumerate() {
            let target = if self.dock_target == Some(index) && self.settings.magnify_dock {
                256
            } else {
                0
            };
            if *hover != target {
                let amount = if self.settings.reduce_motion {
                    256
                } else {
                    ((256 / DOCK_TICKS).max(1)).saturating_mul(ticks)
                };
                if *hover < target {
                    *hover = hover.saturating_add(amount).min(target);
                } else {
                    *hover = hover.saturating_sub(amount).max(target);
                }
                changed = true;
            }
        }

        changed
    }

    /// Finishes all running animations immediately.
    pub fn finish_animations(&mut self) {
        self.transition.anim.finish();
        self.transition.active = false;
        self.overview.finish();
        for window in self.windows.iter_mut().flatten() {
            window.open.finish();
            window.minimize.finish();
            if window.closing {
                window.close.finish();
            }
        }
        self.dock_hover = core::array::from_fn(|index| {
            if self.settings.magnify_dock && self.dock_target == Some(index) {
                256
            } else {
                0
            }
        });
    }

    /// Returns `true` while any animation is running.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.transition.active
            || self.overview.is_active()
            || self.windows.iter().flatten().any(|window| {
                window.open.is_active() || window.minimize.is_active() || window.close.is_active()
            })
            || self.dock_hover.iter().enumerate().any(|(index, value)| {
                *value
                    != if self.settings.magnify_dock && self.dock_target == Some(index) {
                        256
                    } else {
                        0
                    }
            })
    }

    /// Updates the pointer position. Returns `true` when a repaint is needed.
    pub fn pointer_moved(&mut self, point: Point) -> bool {
        let point = Point::new(
            point.x.clamp(self.screen.x, self.screen.right() - 1),
            point.y.clamp(self.screen.y, self.screen.bottom() - 1),
        );
        if point == self.cursor {
            return false;
        }
        self.cursor = point;
        if let Some(drag) = self.dragging {
            self.apply_drag(drag, point);
        }
        self.dock_target = layout::dock_slot_at(point, self.screen);
        if self.settings.reduce_motion {
            self.dock_hover = core::array::from_fn(|index| {
                if self.settings.magnify_dock && self.dock_target == Some(index) {
                    256
                } else {
                    0
                }
            });
        }
        true
    }

    /// Handles a primary-button press.
    pub fn pointer_down(&mut self, point: Point, ticks: u64) {
        self.pointer_moved(point);
        let point = self.cursor;

        if self.spotlight_open {
            match self.hit_test(point) {
                Hit::SpotlightResult(row) => {
                    let command = self.search_result(row).map(|item| item.command);
                    self.spotlight_open = false;
                    if let Some(command) = command {
                        self.execute(command);
                    }
                }
                Hit::SpotlightField | Hit::SpotlightPanel
                    if layout::spotlight_panel(self.screen).contains(point) => {}
                _ => self.spotlight_open = false,
            }
            return;
        }

        if let Some(menu) = self.menu {
            match self.hit_test(point) {
                Hit::MenuItem(row) => {
                    let item = menu.kind.items()[row];
                    if !item.is_separator() {
                        self.menu = None;
                        self.execute(item.command);
                    }
                }
                Hit::MenuLogo => self.toggle_menu(MenuKind::System),
                Hit::MenuTitle(kind) => self.toggle_menu(kind),
                Hit::MenuPanel => {}
                _ => self.menu = None,
            }
            return;
        }

        if self.overview_open {
            let cards = layout::overview_cards(self.screen);
            for (index, card) in cards.iter().enumerate() {
                if card.contains(point) {
                    self.switch_workspace(index as u8);
                    self.overview_open = false;
                    self.overview.retarget(
                        0,
                        if self.settings.reduce_motion {
                            0
                        } else {
                            OVERVIEW_TICKS
                        },
                    );
                    return;
                }
            }
            if point.y >= self.screen.y + layout::MENU_BAR_HEIGHT {
                self.overview_open = false;
                self.overview.retarget(
                    0,
                    if self.settings.reduce_motion {
                        0
                    } else {
                        OVERVIEW_TICKS
                    },
                );
                if point.y < layout::dock_panel(self.screen).y {
                    return;
                }
            }
        }

        match self.hit_test(point) {
            Hit::MenuLogo => self.toggle_menu(MenuKind::System),
            Hit::MenuTitle(kind) => self.toggle_menu(kind),
            Hit::MenuItem(_) | Hit::MenuPanel => {}
            Hit::StatusSearch => self.open_spotlight(),
            Hit::StatusSpaces => self.toggle_overview(),
            Hit::StatusSecurity => {
                self.open(WindowKind::Security);
            }
            Hit::SpotlightResult(_) | Hit::SpotlightPanel | Hit::SpotlightField => {}
            Hit::DockIcon(index) => {
                let Some(slot) = layout::DOCK_SLOTS.get(index).copied() else {
                    return;
                };
                match slot {
                    layout::DockSlot::App(kind) => {
                        self.open(kind);
                    }
                    layout::DockSlot::Overview => self.toggle_overview(),
                }
            }
            Hit::DockPanel => {}
            Hit::OverviewCard(index) => {
                self.switch_workspace(index);
                self.overview_open = false;
            }
            Hit::OverviewPanel => self.toggle_overview(),
            Hit::TitleBar(id) => {
                self.focus_window(id);
                let double_click = self.last_click.is_some_and(|(last, when)| {
                    last == id && ticks.saturating_sub(when) <= DOUBLE_CLICK_TICKS
                });
                self.last_click = Some((id, ticks));
                if double_click {
                    self.zoom_window(id);
                    self.dragging = None;
                } else if self
                    .window(id)
                    .is_some_and(|window| window.state != WindowState::Maximized)
                {
                    self.dragging = Some(Drag {
                        id,
                        resize: false,
                        origin: point,
                        frame: self
                            .window(id)
                            .map(|window| window.frame)
                            .unwrap_or_default(),
                    });
                }
            }
            Hit::Window(id) | Hit::SettingsRow(id, _) => {
                self.focus_window(id);
                if let Some(row) = self.settings_row_at(id, point) {
                    self.toggle_setting(row);
                }
            }
            Hit::Close(id) => self.close_window(id),
            Hit::Minimize(id) => self.minimize_window(id),
            Hit::Zoom(id) => self.zoom_window(id),
            Hit::Resize(id) => {
                self.focus_window(id);
                self.dragging = Some(Drag {
                    id,
                    resize: true,
                    origin: point,
                    frame: self
                        .window(id)
                        .map(|window| window.frame)
                        .unwrap_or_default(),
                });
            }
            Hit::None | Hit::MenuBar => {
                self.menu = None;
            }
        }
    }
    /// Ends the current drag or resize gesture.
    pub fn pointer_up(&mut self) {
        self.dragging = None;
    }

    /// Handles a key event.
    pub fn key(&mut self, key: Key, modifiers: Modifiers) -> KeyOutcome {
        let mut outcome = KeyOutcome::default();

        if self.spotlight_open {
            if (modifiers.ctrl || modifiers.super_key) && key == Key::Char(b' ') {
                self.spotlight_open = false;
                outcome.changed = true;
                return outcome;
            }
            match key {
                Key::Escape => {
                    self.spotlight_open = false;
                    outcome.changed = true;
                }
                Key::Enter => {
                    if let Some(item) = self.search_result(self.spotlight_selected).copied() {
                        self.spotlight_open = false;
                        self.execute(item.command);
                        outcome.changed = true;
                        outcome.announce = Some(item.label);
                    }
                }
                Key::Up => {
                    self.spotlight_selected = self.spotlight_selected.saturating_sub(1);
                    outcome.changed = true;
                }
                Key::Down => {
                    let count = self.search_result_count();
                    if self.spotlight_selected + 1 < count {
                        self.spotlight_selected += 1;
                    }
                    outcome.changed = true;
                }
                Key::Backspace => {
                    self.spotlight_len = self.spotlight_len.saturating_sub(1);
                    self.spotlight_selected = 0;
                    outcome.changed = true;
                }
                Key::Char(byte) if byte.is_ascii_graphic() || byte == b' ' => {
                    if self.spotlight_len < self.spotlight_query.len() {
                        self.spotlight_query[self.spotlight_len] = byte;
                        self.spotlight_len += 1;
                        self.spotlight_selected = 0;
                    }
                    outcome.changed = true;
                }
                Key::Tab | Key::Char(_) | Key::Function(_) | Key::Left | Key::Right => {}
            }
            return outcome;
        }

        if let Some(menu) = self.menu {
            match key {
                Key::Escape => {
                    self.menu = None;
                    outcome.changed = true;
                    return outcome;
                }
                Key::Up | Key::Down => {
                    let items = menu.kind.items();
                    let mut selected = menu.selected;
                    let step: isize = if key == Key::Up { -1 } else { 1 };
                    for _ in 0..items.len() {
                        selected =
                            (selected as isize + step).rem_euclid(items.len() as isize) as usize;
                        if !items[selected].is_separator() {
                            break;
                        }
                    }
                    self.menu = Some(MenuState {
                        selected,
                        keyboard: true,
                        ..menu
                    });
                    outcome.changed = true;
                    return outcome;
                }
                Key::Left | Key::Right => {
                    let index = MenuKind::ALL
                        .iter()
                        .position(|kind| *kind == menu.kind)
                        .unwrap_or(0);
                    let next = (index as isize + if key == Key::Left { -1 } else { 1 })
                        .rem_euclid(MenuKind::ALL.len() as isize)
                        as usize;
                    self.menu = None;
                    self.toggle_menu(MenuKind::ALL[next]);
                    if let Some(menu) = &mut self.menu {
                        menu.keyboard = true;
                    }
                    outcome.changed = true;
                    return outcome;
                }
                Key::Function(10) => {
                    self.menu = None;
                    outcome.changed = true;
                    return outcome;
                }
                Key::Enter => {
                    let item = menu.kind.items().get(menu.selected).copied();
                    self.menu = None;
                    if let Some(item) = item
                        && !item.is_separator()
                    {
                        self.execute(item.command);
                        outcome.changed = true;
                        outcome.announce = Some(item.label);
                    }
                    return outcome;
                }
                _ => {
                    self.menu = None;
                    outcome.changed = true;
                }
            }
        }

        if self.overview_open {
            match key {
                Key::Escape => {
                    self.overview_open = false;
                    self.overview.retarget(
                        0,
                        if self.settings.reduce_motion {
                            0
                        } else {
                            OVERVIEW_TICKS
                        },
                    );
                    outcome.changed = true;
                    return outcome;
                }
                Key::Left => {
                    self.overview_selected = (self.overview_selected + WORKSPACES - 1) % WORKSPACES;
                    outcome.changed = true;
                    return outcome;
                }
                Key::Right => {
                    self.overview_selected = (self.overview_selected + 1) % WORKSPACES;
                    outcome.changed = true;
                    return outcome;
                }
                Key::Up => {
                    self.overview_selected = self.overview_selected.saturating_sub(2);
                    outcome.changed = true;
                    return outcome;
                }
                Key::Down => {
                    self.overview_selected = (self.overview_selected + 2).min(WORKSPACES - 1);
                    outcome.changed = true;
                    return outcome;
                }
                Key::Enter => {
                    self.switch_workspace(self.overview_selected);
                    self.overview_open = false;
                    self.overview.retarget(
                        0,
                        if self.settings.reduce_motion {
                            0
                        } else {
                            OVERVIEW_TICKS
                        },
                    );
                    outcome.changed = true;
                    outcome.announce = Some("SPACE");
                    return outcome;
                }
                _ => {}
            }
        }

        let command = modifiers.command();
        if (modifiers.ctrl || modifiers.super_key) && key == Key::Char(b' ') {
            self.open_spotlight();
            outcome.changed = true;
            outcome.announce = Some("SEARCH");
            return outcome;
        }
        if command {
            match key {
                Key::Char(b'w') => {
                    self.execute(Command::CloseFocused);
                    outcome.changed = true;
                    outcome.announce = Some("CLOSE");
                    return outcome;
                }
                Key::Char(b'm') => {
                    self.execute(Command::MinimizeFocused);
                    outcome.changed = true;
                    outcome.announce = Some("MINIMIZE");
                    return outcome;
                }
                Key::Char(b'f') => {
                    self.execute(Command::ZoomFocused);
                    outcome.changed = true;
                    outcome.announce = Some("ZOOM");
                    return outcome;
                }
                Key::Char(b'n') => {
                    self.execute(Command::Open(WindowKind::Files));
                    outcome.changed = true;
                    outcome.announce = Some("FILES");
                    return outcome;
                }
                Key::Char(b',') => {
                    self.execute(Command::Open(WindowKind::Settings));
                    outcome.changed = true;
                    outcome.announce = Some("SETTINGS");
                    return outcome;
                }
                Key::Char(b'1'..=b'4') => {
                    if let Key::Char(value) = key {
                        self.switch_workspace(value - b'1');
                        outcome.changed = true;
                        outcome.announce = Some("SPACE");
                    }
                    return outcome;
                }
                Key::Tab => {
                    self.focus_next(if modifiers.shift { -1 } else { 1 });
                    outcome.changed = true;
                    outcome.announce = Some(self.focus_title());
                    return outcome;
                }
                Key::Left => {
                    self.cycle_workspace(-1);
                    outcome.changed = true;
                    return outcome;
                }
                Key::Right => {
                    self.cycle_workspace(1);
                    outcome.changed = true;
                    return outcome;
                }
                Key::Up => {
                    self.toggle_overview();
                    outcome.changed = true;
                    return outcome;
                }
                _ => {}
            }
        }

        if modifiers.shift && !command {
            let delta = match key {
                Key::Left => Some((-8, 0)),
                Key::Right => Some((8, 0)),
                Key::Up => Some((0, -8)),
                Key::Down => Some((0, 8)),
                _ => None,
            };
            if let Some((dx, dy)) = delta {
                self.resize_focused(dx, dy);
                outcome.changed = true;
                return outcome;
            }
        }

        match key {
            Key::Tab => {
                self.focus_next(if modifiers.shift { -1 } else { 1 });
                outcome.changed = true;
            }
            Key::Escape => {
                self.menu = None;
                outcome.changed = true;
            }
            Key::Up => {
                self.move_focused(0, -8);
                outcome.changed = true;
            }
            Key::Down => {
                self.move_focused(0, 8);
                outcome.changed = true;
            }
            Key::Left => {
                self.move_focused(-8, 0);
                outcome.changed = true;
            }
            Key::Right => {
                self.move_focused(8, 0);
                outcome.changed = true;
            }
            Key::Enter => {
                outcome.announce = Some(self.focus_title());
            }
            Key::Function(3) => {
                self.toggle_overview();
                outcome.changed = true;
            }
            Key::Function(10) => {
                self.toggle_menu(MenuKind::System);
                if let Some(menu) = &mut self.menu {
                    menu.keyboard = true;
                }
                outcome.changed = true;
            }
            Key::Function(_) | Key::Char(_) | Key::Backspace => {}
        }
        outcome
    }

    /// Returns the open menu's panel rectangle.
    #[must_use]
    pub fn menu_panel(&self) -> Option<Rect> {
        let menu = self.menu?;
        let anchor = layout::menu_anchor(self.screen, menu.kind, self.app_name());
        Some(layout::menu_panel_rect(
            self.screen,
            anchor,
            menu.kind.items().len(),
        ))
    }

    /// Hit-tests a screen point against the current interface.
    #[must_use]
    pub fn hit_test(&self, point: Point) -> Hit {
        if self.spotlight_open {
            let panel = layout::spotlight_panel(self.screen);
            if panel.contains(point) {
                if layout::spotlight_field(panel).contains(point) {
                    return Hit::SpotlightField;
                }
                if let Some(row) = (0..self.search_result_count())
                    .find(|row| layout::spotlight_result_rect(panel, *row).contains(point))
                {
                    return Hit::SpotlightResult(row);
                }
                return Hit::SpotlightPanel;
            }
            return Hit::SpotlightPanel;
        }

        if let Some(menu) = self.menu
            && let Some(panel) = self.menu_panel()
            && panel.contains(point)
        {
            if let Some(row) = (0..menu.kind.items().len())
                .find(|row| layout::menu_item_rect(panel, *row).contains(point))
            {
                return Hit::MenuItem(row);
            }
            return Hit::MenuPanel;
        }

        let status = layout::status_items(self.screen);
        if status.search.contains(point) {
            return Hit::StatusSearch;
        }
        if status.spaces.contains(point) {
            return Hit::StatusSpaces;
        }
        if status.security.contains(point) {
            return Hit::StatusSecurity;
        }

        if layout::menu_logo_rect(self.screen).contains(point) {
            return Hit::MenuLogo;
        }
        let titles = layout::menu_title_rects(self.screen, self.app_name());
        for (index, rect) in titles.iter().enumerate() {
            if rect.contains(point) {
                return Hit::MenuTitle(MenuKind::BAR[index]);
            }
        }
        if point.y < self.screen.y + layout::MENU_BAR_HEIGHT {
            return Hit::MenuBar;
        }

        let icons = layout::dock_icon_rects(self.screen, &self.dock_hover);
        for (index, rect) in icons.iter().enumerate().rev() {
            if rect.contains(point) {
                return Hit::DockIcon(index);
            }
        }
        if layout::dock_panel(self.screen).contains(point) {
            return Hit::DockPanel;
        }

        if self.overview_open {
            let cards = layout::overview_cards(self.screen);
            for (index, card) in cards.iter().enumerate() {
                if card.contains(point) {
                    return Hit::OverviewCard(index as u8);
                }
            }
            return Hit::OverviewPanel;
        }

        for id in self.order().iter().rev() {
            let Some(window) = self.window(*id) else {
                continue;
            };
            if window.workspace != self.workspace
                || window.closing
                || window.state == WindowState::Minimized
            {
                continue;
            }
            let Some(frame) = self.window_frame(window.id) else {
                continue;
            };
            let lights = layout::traffic_lights(frame);
            if lights.close.contains(point) {
                return Hit::Close(window.id);
            }
            if lights.minimize.contains(point) {
                return Hit::Minimize(window.id);
            }
            if lights.zoom.contains(point) {
                return Hit::Zoom(window.id);
            }
            if window.kind == WindowKind::Settings
                && let Some(row) = self.settings_row_at(window.id, point)
            {
                return Hit::SettingsRow(window.id, row);
            }
            if window.state != WindowState::Maximized && layout::resize_grip(frame).contains(point)
            {
                return Hit::Resize(window.id);
            }
            if layout::title_bar(frame).contains(point) {
                return Hit::TitleBar(window.id);
            }
            if frame.contains(point) {
                return Hit::Window(window.id);
            }
        }
        Hit::None
    }

    fn find_kind(&self, kind: WindowKind) -> Option<u8> {
        self.windows
            .iter()
            .flatten()
            .find(|window| window.kind == kind)
            .map(|window| window.id)
    }

    fn window_index(&self, id: u8) -> Option<usize> {
        self.windows
            .iter()
            .position(|window| window.is_some_and(|window| window.id == id))
    }

    fn focused_id(&self) -> Option<u8> {
        self.focus
    }

    fn toggle_menu(&mut self, kind: MenuKind) {
        if self.menu.is_some_and(|menu| menu.kind == kind) {
            self.menu = None;
            return;
        }
        let selected = kind
            .items()
            .iter()
            .position(|item| !item.is_separator())
            .unwrap_or(0);
        self.menu = Some(MenuState {
            kind,
            selected,
            keyboard: false,
        });
    }

    fn is_eligible(&self, id: u8) -> bool {
        self.window(id).is_some_and(|window| {
            window.workspace == self.workspace
                && !window.closing
                && window.state != WindowState::Minimized
        })
    }

    fn focus_topmost(&mut self) {
        for id in self.order().iter().rev() {
            if self.is_eligible(*id) {
                self.focus = Some(*id);
                return;
            }
        }
        self.focus = None;
    }

    fn bring_to_front(&mut self, id: Option<u8>) {
        let Some(id) = id else {
            return;
        };
        let Some(position) = self.order[..self.order_len]
            .iter()
            .position(|item| *item == id)
        else {
            return;
        };
        for index in position..self.order_len.saturating_sub(1) {
            self.order[index] = self.order[index + 1];
        }
        self.order[self.order_len - 1] = id;
    }

    fn remove_from_order(&mut self, id: u8) {
        if let Some(position) = self.order[..self.order_len]
            .iter()
            .position(|item| *item == id)
        {
            for index in position..self.order_len.saturating_sub(1) {
                self.order[index] = self.order[index + 1];
            }
            self.order_len = self.order_len.saturating_sub(1);
        }
    }

    fn apply_drag(&mut self, drag: Drag, point: Point) {
        let Some(index) = self.window_index(drag.id) else {
            return;
        };
        let area = layout::work_area(self.screen);
        let dx = point.x - drag.origin.x;
        let dy = point.y - drag.origin.y;
        if let Some(window) = &mut self.windows[index] {
            if drag.resize {
                let width = (drag.frame.width + dx)
                    .max(MIN_WINDOW_WIDTH)
                    .min(area.width);
                let height = (drag.frame.height + dy)
                    .max(MIN_WINDOW_HEIGHT)
                    .min(area.height);
                window.frame =
                    Rect::new(drag.frame.x, drag.frame.y, width, height).clamp_inside(area);
            } else {
                window.frame = drag.frame.translate(dx, dy).clamp_inside(area);
            }
        }
    }
}

fn search_matches(label: &[u8], query: &[u8]) -> bool {
    if query.is_empty() {
        return true;
    }
    if query.len() > label.len() {
        return false;
    }
    for start in 0..=label.len() - query.len() {
        if label[start..start + query.len()]
            .iter()
            .zip(query)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
        {
            return true;
        }
    }
    false
}
