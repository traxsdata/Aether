// SPDX-License-Identifier: Apache-2.0
//! Bounded Files presentation state. Resource access remains with the service.
//!
//! Navigation and previews are explicit requests through [`BrowserSource`]. The
//! source must enforce its own scope; a validated UI path is not authorization.

use crate::{Key, Point, Rect};

/// Maximum path bytes supported by the prototype filesystem.
pub const PATH_BYTES: usize = 128;
/// Maximum name bytes in the prototype filesystem.
pub const NAME_BYTES: usize = 16;
/// Maximum directory entries in the prototype filesystem.
pub const ENTRIES: usize = 25;
/// Maximum preview bytes; longer files are explicitly marked as truncated.
pub const PREVIEW_BYTES: usize = 512;

/// A resource service error suitable for a user-facing Files message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserError {
    /// The resource is outside the service's permitted scope.
    Access,
    /// The resource disappeared or could not be read.
    Unavailable,
    /// A path or directory response is malformed or exceeds a bound.
    InvalidData,
}

impl BrowserError {
    /// Human-readable error; never silently replace an error with an empty list.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Access => "ACCESS DENIED: PRIVATE APP DATA",
            Self::Unavailable => "RESOURCE UNAVAILABLE - TRY REFRESH",
            Self::InvalidData => "INVALID OR OVERSIZED DIRECTORY DATA",
        }
    }
}

/// The filesystem service interface needed by the read-only Files MVP.
pub trait BrowserSource {
    /// Lists `name<TAB>d|f<TAB>size\n` records. Enforce scope before returning data.
    fn list(&mut self, path: &str, out: &mut [u8]) -> Result<usize, BrowserError>;
    /// Reads a bounded prefix of a file. No application is executed by a preview.
    fn read(&mut self, path: &str, out: &mut [u8]) -> Result<usize, BrowserError>;
    /// Deletes a file or empty directory. The service must enforce scope and
    /// refuse deletion while a file capability is open.
    fn remove(&mut self, _path: &str) -> Result<(), BrowserError> {
        Err(BrowserError::Unavailable)
    }
    /// Renames a file or directory to `new_name` in the same directory.
    fn rename(&mut self, _path: &str, _new_name: &str) -> Result<(), BrowserError> {
        Err(BrowserError::Unavailable)
    }
    /// Duplicates a file or directory under a service-chosen, non-existent
    /// name in the same directory. The service owns the naming policy.
    fn duplicate(&mut self, _path: &str) -> Result<(), BrowserError> {
        Err(BrowserError::Unavailable)
    }
}

/// One validated directory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    name: [u8; NAME_BYTES],
    len: usize,
    /// Whether this entry is a directory.
    pub directory: bool,
    /// Reported file size.
    pub size: usize,
}

impl Entry {
    const EMPTY: Self = Self {
        name: [0; NAME_BYTES],
        len: 0,
        directory: false,
        size: 0,
    };

    /// Validated UTF-8 name.
    #[must_use]
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.len]).unwrap_or("")
    }
}

/// Shared geometry for painting and hitting the Files application controls.
#[derive(Clone, Copy, Debug)]
pub struct BrowserLayout {
    /// Sidebar rectangle.
    pub sidebar: Rect,
    /// Main content pane.
    pub pane: Rect,
    /// Parent/close-preview button.
    pub parent: Rect,
    /// Refresh button.
    pub refresh: Rect,
    /// Rows or preview region.
    pub body: Rect,
}

impl BrowserLayout {
    /// Derives responsive geometry from the window content rectangle.
    #[must_use]
    pub fn new(content: Rect) -> Self {
        let width = (content.width / 3).clamp(92, 138);
        let sidebar = Rect::new(content.x, content.y, width, content.height);
        let pane = Rect::new(
            sidebar.right(),
            content.y,
            content.width - width,
            content.height,
        );
        Self {
            sidebar,
            pane,
            parent: Rect::new(pane.x + 8, pane.y + 7, 24, 22),
            refresh: Rect::new(pane.right() - 30, pane.y + 7, 22, 22),
            body: Rect::new(
                pane.x + 8,
                pane.y + 38,
                (pane.width - 16).max(0),
                (pane.height - 60).max(0),
            ),
        }
    }

    /// Number of fully visible list rows.
    #[must_use]
    pub fn visible_rows(self) -> usize {
        (self.body.height / 24).max(1) as usize
    }

    /// A visible row's exact hit and paint rectangle.
    #[must_use]
    pub fn row(self, relative: usize) -> Option<Rect> {
        let rect = Rect::new(
            self.body.x,
            self.body.y + relative as i32 * 24,
            self.body.width,
            22,
        );
        (rect.bottom() <= self.body.bottom()).then_some(rect)
    }

    /// A sidebar destination's hit and paint rectangle.
    #[must_use]
    pub fn sidebar_row(self, index: usize) -> Rect {
        Rect::new(
            self.sidebar.x + 6,
            self.sidebar.y + 46 + index as i32 * 26,
            self.sidebar.width - 12,
            22,
        )
    }
}

/// Sidebar labels and service paths; no hidden destinations or mock buttons.
///
/// The order matches AETHER-UX-002 §2.1 and is shared by ordinary browsing,
/// Open, Save, and file-selection portals. `/system/applications` exposes
/// package metadata and user-visible documents, never private vault contents.
pub const LOCATIONS: [(&str, &str); 13] = [
    ("Home", "/home"),
    ("Recent", "/home/Recent"),
    ("Favorites", "/home/Favorites"),
    ("Documents", "/home/Documents"),
    ("Downloads", "/home/Downloads"),
    ("Pictures", "/home/Pictures"),
    ("Music", "/home/Music"),
    ("Videos", "/home/Videos"),
    ("Projects", "/home/Projects"),
    ("Shared", "/home/Shared"),
    ("Applications", "/system/applications"),
    ("Devices", "/system/devices"),
    ("Trash", "/home/Trash"),
];

/// Read-only folder navigation, selection, scrolling, and bounded text previews.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Browser {
    path: [u8; PATH_BYTES],
    path_len: usize,
    entries: [Entry; ENTRIES],
    count: usize,
    selected: usize,
    first: usize,
    visible: usize,
    preview: [u8; PREVIEW_BYTES],
    preview_len: usize,
    preview_name: Entry,
    preview_open: bool,
    error: Option<BrowserError>,
    last_click: Option<(usize, u64)>,
}

impl Browser {
    /// Creates an empty browser at the volume root. Call `refresh` to load it.
    #[must_use]
    pub const fn new() -> Self {
        let mut path = [0; PATH_BYTES];
        path[0] = b'/';
        Self {
            path,
            path_len: 1,
            entries: [Entry::EMPTY; ENTRIES],
            count: 0,
            selected: 0,
            first: 0,
            visible: 5,
            preview: [0; PREVIEW_BYTES],
            preview_len: 0,
            preview_name: Entry::EMPTY,
            preview_open: false,
            error: None,
            last_click: None,
        }
    }

    /// Current directory's canonical path.
    #[must_use]
    pub fn path(&self) -> &str {
        core::str::from_utf8(&self.path[..self.path_len]).unwrap_or("/")
    }
    /// Current directory entries.
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries[..self.count]
    }
    /// Selected directory entry index.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }
    /// First visible directory entry index.
    #[must_use]
    pub const fn first(&self) -> usize {
        self.first
    }
    /// Current service error.
    #[must_use]
    pub const fn error(&self) -> Option<BrowserError> {
        self.error
    }
    /// Whether a read-only preview is open.
    #[must_use]
    pub const fn preview_open(&self) -> bool {
        self.preview_open
    }
    /// Name of the previewed file.
    #[must_use]
    pub fn preview_name(&self) -> &str {
        self.preview_name.name()
    }
    /// Raw preview bytes; the renderer must treat these as data, not commands.
    #[must_use]
    pub fn preview_bytes(&self) -> &[u8] {
        &self.preview[..self.preview_len]
    }
    /// Whether bytes beyond the bounded preview were omitted.
    #[must_use]
    pub fn preview_truncated(&self) -> bool {
        self.preview_name.size > self.preview_len
    }

    /// Updates viewport height and keeps the selection visible.
    pub fn set_visible_rows(&mut self, rows: usize) {
        self.visible = rows.max(1);
        if self.selected < self.first {
            self.first = self.selected;
        }
        if self.selected >= self.first.saturating_add(self.visible) {
            self.first = self.selected + 1 - self.visible;
        }
    }

    /// Loads a directory transactionally: errors leave the previous view intact.
    pub fn navigate(&mut self, path: &str, source: &mut impl BrowserSource) -> bool {
        if !valid_path(path) {
            self.error = Some(BrowserError::InvalidData);
            return false;
        }
        let mut listing = [0; 1024];
        let result = source.list(path, &mut listing).and_then(|len| {
            let bytes = listing.get(..len).ok_or(BrowserError::InvalidData)?;
            parse_entries(bytes)
        });
        match result {
            Ok((entries, count)) => {
                self.path = [0; PATH_BYTES];
                self.path[..path.len()].copy_from_slice(path.as_bytes());
                self.path_len = path.len();
                self.entries = entries;
                self.count = count;
                self.selected = 0;
                self.first = 0;
                self.preview_open = false;
                self.preview_len = 0;
                self.error = None;
                self.last_click = None;
                true
            }
            Err(error) => {
                self.error = Some(error);
                false
            }
        }
    }

    /// Reloads the current folder, e.g. after a diagnostic shell file operation.
    pub fn refresh(&mut self, source: &mut impl BrowserSource) {
        let path = self.path;
        let len = self.path_len;
        self.navigate(core::str::from_utf8(&path[..len]).unwrap_or("/"), source);
    }

    /// Closes a preview or navigates to the parent without sending `..` to a service.
    pub fn parent(&mut self, source: &mut impl BrowserSource) {
        self.error = None;
        self.last_click = None;
        if self.preview_open {
            self.preview_open = false;
            return;
        }
        let path = self.path;
        let end = path[..self.path_len]
            .iter()
            .rposition(|byte| *byte == b'/')
            .unwrap_or(0)
            .max(1);
        self.navigate(core::str::from_utf8(&path[..end]).unwrap_or("/"), source);
    }

    /// Opens the selected folder or a read-only text preview through the service.
    pub fn open_selected(&mut self, source: &mut impl BrowserSource) {
        let Some(entry) = self.entries().get(self.selected).copied() else {
            return;
        };
        let mut path = self.path;
        let mut len = self.path_len;
        if len > 1 {
            if len == path.len() {
                self.error = Some(BrowserError::InvalidData);
                return;
            }
            path[len] = b'/';
            len += 1;
        }
        if len + entry.len > path.len() {
            self.error = Some(BrowserError::InvalidData);
            return;
        }
        path[len..len + entry.len].copy_from_slice(&entry.name[..entry.len]);
        let path = core::str::from_utf8(&path[..len + entry.len]).unwrap_or("");
        if entry.directory {
            self.navigate(path, source);
            return;
        }
        match source.read(path, &mut self.preview) {
            Ok(len) if len <= PREVIEW_BYTES => {
                self.preview_len = len;
                self.preview_name = entry;
                self.preview_open = true;
                self.error = None;
            }
            Ok(_) => self.error = Some(BrowserError::InvalidData),
            Err(error) => self.error = Some(error),
        }
        self.last_click = None;
    }

    /// Handles Files-specific keys. Global shortcuts and resize keys stay with WM.
    pub fn key(&mut self, key: Key, source: &mut impl BrowserSource) -> bool {
        match key {
            Key::Up if !self.preview_open => self.selected = self.selected.saturating_sub(1),
            Key::Down if !self.preview_open => {
                self.selected = (self.selected + 1).min(self.count.saturating_sub(1))
            }
            Key::Enter if !self.preview_open => self.open_selected(source),
            Key::Backspace => self.parent(source),
            Key::Delete => {
                self.delete_selected(source);
            }
            Key::Function(3) => {
                self.duplicate_selected(source);
            }
            Key::Escape if self.preview_open || self.error.is_some() => {
                self.preview_open = false;
                self.error = None;
            }
            Key::Function(5) => self.refresh(source),
            _ => return false,
        }
        self.set_visible_rows(self.visible);
        true
    }

    /// Name of the selected entry.
    #[must_use]
    pub fn selected_name(&self) -> Option<&str> {
        self.entries.get(self.selected).map(Entry::name)
    }

    /// Selects an entry by name. Returns `false` when it is not present.
    pub fn select_name(&mut self, name: &str) -> bool {
        let Some(index) = self.entries().iter().position(|entry| entry.name() == name) else {
            return false;
        };
        self.selected = index;
        self.set_visible_rows(self.visible);
        true
    }

    /// Full path of the selected entry.
    pub fn selected_path<'a>(&self, out: &'a mut [u8; PATH_BYTES]) -> Option<&'a str> {
        let entry = self.entries.get(self.selected).copied()?;
        let mut len = self.path_len;
        out.get_mut(..len)?.copy_from_slice(&self.path[..len]);
        if len > 1 {
            *out.get_mut(len)? = b'/';
            len += 1;
        }
        let end = len.checked_add(entry.len)?;
        out.get_mut(len..end)?
            .copy_from_slice(&entry.name[..entry.len]);
        core::str::from_utf8(&out[..end]).ok()
    }

    /// Deletes the selected file or empty directory, then reloads the folder.
    pub fn delete_selected(&mut self, source: &mut impl BrowserSource) -> bool {
        if self.preview_open {
            return false;
        }
        let mut path = [0u8; PATH_BYTES];
        let Some(path) = self.selected_path(&mut path) else {
            return false;
        };
        let mut owned = [0u8; PATH_BYTES];
        let length = path.len();
        owned[..length].copy_from_slice(path.as_bytes());
        let path = core::str::from_utf8(&owned[..length]).unwrap_or("");
        match source.remove(path) {
            Ok(()) => {
                self.refresh(source);
                true
            }
            Err(error) => {
                self.error = Some(error);
                true
            }
        }
    }

    /// Duplicates the selected file under a service-chosen new name.
    pub fn duplicate_selected(&mut self, source: &mut impl BrowserSource) -> bool {
        if self.preview_open {
            return false;
        }
        let mut path = [0u8; PATH_BYTES];
        let Some(path) = self.selected_path(&mut path) else {
            return false;
        };
        let mut owned = [0u8; PATH_BYTES];
        let length = path.len();
        owned[..length].copy_from_slice(path.as_bytes());
        let path = core::str::from_utf8(&owned[..length]).unwrap_or("");
        match source.duplicate(path) {
            Ok(()) => {
                self.refresh(source);
                true
            }
            Err(error) => {
                self.error = Some(error);
                true
            }
        }
    }

    /// Renames the selected entry within the current folder.
    pub fn rename_selected(&mut self, source: &mut impl BrowserSource, name: &str) -> bool {
        if self.preview_open || !valid_name(name) {
            self.error = Some(BrowserError::InvalidData);
            return false;
        }
        let mut path = [0u8; PATH_BYTES];
        let Some(path) = self.selected_path(&mut path) else {
            return false;
        };
        let mut owned = [0u8; PATH_BYTES];
        let length = path.len();
        owned[..length].copy_from_slice(path.as_bytes());
        let path = core::str::from_utf8(&owned[..length]).unwrap_or("");
        match source.rename(path, name) {
            Ok(()) => {
                self.refresh(source);
                true
            }
            Err(error) => {
                self.error = Some(error);
                true
            }
        }
    }

    /// Handles shared-layout controls and row selection/double-click activation.
    pub fn pointer(
        &mut self,
        layout: BrowserLayout,
        point: Point,
        ticks: u64,
        source: &mut impl BrowserSource,
    ) {
        self.set_visible_rows(layout.visible_rows());
        if layout.parent.contains(point) {
            self.parent(source);
            return;
        }
        if layout.refresh.contains(point) {
            self.refresh(source);
            return;
        }
        for (index, (_, path)) in LOCATIONS.iter().enumerate() {
            if layout.sidebar_row(index).contains(point) {
                self.navigate(path, source);
                return;
            }
        }
        if self.preview_open {
            return;
        }
        for index in self.first..self.count {
            let Some(rect) = layout.row(index - self.first) else {
                break;
            };
            if rect.contains(point) {
                let double = self
                    .last_click
                    .is_some_and(|(last, when)| last == index && ticks.saturating_sub(when) <= 35);
                self.selected = index;
                self.error = None;
                self.last_click = Some((index, ticks));
                if double {
                    self.open_selected(source);
                }
                return;
            }
        }
    }
}

impl Default for Browser {
    fn default() -> Self {
        Self::new()
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES
        && name != "."
        && name != ".."
        && !name
            .bytes()
            .any(|byte| byte == b'/' || byte.is_ascii_control())
}

fn valid_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= PATH_BYTES
        && (path == "/" || path[1..].split('/').all(valid_name))
}

fn parse_entries(bytes: &[u8]) -> Result<([Entry; ENTRIES], usize), BrowserError> {
    let text = core::str::from_utf8(bytes).map_err(|_| BrowserError::InvalidData)?;
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(BrowserError::InvalidData);
    }
    let mut entries = [Entry::EMPTY; ENTRIES];
    let mut count = 0;
    for line in text.lines() {
        let mut fields = line.split('\t');
        let name = fields.next().ok_or(BrowserError::InvalidData)?;
        if !valid_name(name)
            || count == ENTRIES
            || entries[..count].iter().any(|entry| entry.name() == name)
        {
            return Err(BrowserError::InvalidData);
        }
        let directory = match fields.next() {
            Some("d") => true,
            Some("f") => false,
            _ => return Err(BrowserError::InvalidData),
        };
        let size = fields
            .next()
            .and_then(|size| size.parse::<usize>().ok())
            .ok_or(BrowserError::InvalidData)?;
        if fields.next().is_some() {
            return Err(BrowserError::InvalidData);
        }
        let mut entry = Entry {
            directory,
            size,
            ..Entry::EMPTY
        };
        entry.name[..name.len()].copy_from_slice(name.as_bytes());
        entry.len = name.len();
        entries[count] = entry;
        count += 1;
    }
    Ok((entries, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Source;
    impl BrowserSource for Source {
        fn list(&mut self, path: &str, out: &mut [u8]) -> Result<usize, BrowserError> {
            let data: &[u8] = match path {
                "/" => b"home\td\t0\napps\td\t0\n",
                "/home" => b"welcome.txt\tf\t18\nempty\td\t0\n",
                "/home/empty" => b"",
                "/apps" => return Err(BrowserError::Access),
                _ => return Err(BrowserError::Unavailable),
            };
            out[..data.len()].copy_from_slice(data);
            Ok(data.len())
        }
        fn read(&mut self, path: &str, out: &mut [u8]) -> Result<usize, BrowserError> {
            assert_eq!(path, "/home/welcome.txt");
            let data = b"welcome to aether\n";
            out[..data.len()].copy_from_slice(data);
            Ok(data.len())
        }
    }

    #[test]
    fn keyboard_opens_folders_previews_files_and_returns_to_parent() {
        let mut browser = Browser::new();
        browser.refresh(&mut Source);
        browser.key(Key::Enter, &mut Source);
        assert_eq!(browser.path(), "/home");
        browser.key(Key::Enter, &mut Source);
        assert!(browser.preview_open());
        assert_eq!(browser.preview_bytes(), b"welcome to aether\n");
        browser.key(Key::Backspace, &mut Source);
        assert!(!browser.preview_open());
        assert_eq!(browser.path(), "/home");
        browser.key(Key::Backspace, &mut Source);
        assert_eq!(browser.path(), "/");
    }

    #[test]
    fn denied_navigation_preserves_the_last_successful_view() {
        let mut browser = Browser::new();
        browser.refresh(&mut Source);
        browser.key(Key::Down, &mut Source);
        browser.key(Key::Enter, &mut Source);
        assert_eq!(browser.path(), "/");
        assert_eq!(browser.entries().len(), 2);
        assert_eq!(browser.error(), Some(BrowserError::Access));
    }

    #[test]
    fn pointer_uses_shared_bounds_and_requires_a_double_click_on_rows() {
        let mut browser = Browser::new();
        browser.refresh(&mut Source);
        let layout = BrowserLayout::new(Rect::new(10, 10, 400, 300));
        let point = layout.row(0).unwrap().center();
        browser.pointer(layout, point, 10, &mut Source);
        assert_eq!(browser.path(), "/");
        browser.pointer(layout, point, 20, &mut Source);
        assert_eq!(browser.path(), "/home");
        browser.pointer(layout, layout.parent.center(), 30, &mut Source);
        assert_eq!(browser.path(), "/");
    }

    #[test]
    fn selection_scrolls_into_view_and_empty_folders_remain_navigable() {
        let mut browser = Browser::new();
        browser.navigate("/home", &mut Source);
        browser.set_visible_rows(1);
        browser.key(Key::Down, &mut Source);
        assert_eq!(browser.first(), 1);
        browser.key(Key::Enter, &mut Source);
        assert_eq!(browser.path(), "/home/empty");
        assert!(browser.entries().is_empty());
        browser.key(Key::Backspace, &mut Source);
        assert_eq!(browser.path(), "/home");
    }

    #[test]
    fn mutations_refresh_the_view_and_surface_service_errors() {
        struct MutatingSource {
            entries: [&'static str; 3],
            count: usize,
            fail: bool,
        }
        impl BrowserSource for MutatingSource {
            fn list(&mut self, _path: &str, out: &mut [u8]) -> Result<usize, BrowserError> {
                let mut written = 0usize;
                for name in self.entries.iter().take(self.count) {
                    out[written..written + name.len()].copy_from_slice(name.as_bytes());
                    written += name.len();
                    out[written..written + 5].copy_from_slice(b"\tf\t1\n");
                    written += 5;
                }
                Ok(written)
            }
            fn read(&mut self, _path: &str, out: &mut [u8]) -> Result<usize, BrowserError> {
                out[0] = b'x';
                Ok(1)
            }
            fn remove(&mut self, _path: &str) -> Result<(), BrowserError> {
                if self.fail {
                    return Err(BrowserError::Access);
                }
                self.entries = ["b.txt", "c.txt", ""];
                self.count = 2;
                Ok(())
            }
            fn duplicate(&mut self, _path: &str) -> Result<(), BrowserError> {
                self.entries = ["a.txt", "b.txt", "copy1"];
                self.count = 3;
                Ok(())
            }
            fn rename(&mut self, _path: &str, new_name: &str) -> Result<(), BrowserError> {
                if new_name == "b.txt" {
                    return Err(BrowserError::InvalidData);
                }
                self.entries[0] = "renamed";
                Ok(())
            }
        }
        let mut source = MutatingSource {
            entries: ["a.txt", "b.txt", "c.txt"],
            count: 3,
            fail: false,
        };
        let mut browser = Browser::new();
        browser.refresh(&mut source);
        assert!(browser.select_name("a.txt"));
        assert!(browser.duplicate_selected(&mut source));
        assert_eq!(browser.entries().len(), 3);
        assert!(browser.selected_name().is_some());
        assert!(browser.rename_selected(&mut source, "renamed"));
        assert!(!browser.rename_selected(&mut source, "../bad"));
        assert_eq!(browser.error(), Some(BrowserError::InvalidData));
        browser.error = None;
        assert!(browser.delete_selected(&mut source));
        assert_eq!(browser.entries().len(), 2);
        source.fail = true;
        assert!(browser.delete_selected(&mut source));
        assert_eq!(browser.error(), Some(BrowserError::Access));
    }

    #[test]
    fn malformed_responses_and_traversal_are_rejected() {
        for listing in [
            "../bad\tf\t1\n",
            "..\td\t0\n",
            "a\tf\t1",
            "a\tx\t1\n",
            "a\tf\t-1\n",
            "a\tf\t1\textra\n",
            "a\tf\t1\na\tf\t2\n",
        ] {
            assert!(parse_entries(listing.as_bytes()).is_err(), "{listing}");
        }
        let mut browser = Browser::new();
        for path in ["relative", "/home/../apps", "/home//empty", "/home\n"] {
            assert!(!browser.navigate(path, &mut Source));
        }
    }
}
