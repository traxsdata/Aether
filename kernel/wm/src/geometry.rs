// SPDX-License-Identifier: Apache-2.0
//! Integer geometry for the compositor.
//!
//! Coordinates are framebuffer pixels. Rectangle edges follow the half-open
//! convention: a rectangle contains `x` when `x >= left && x < right`.

/// A point in framebuffer coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: i32,
    /// Vertical coordinate.
    pub y: i32,
}

impl Point {
    /// Creates a point.
    #[must_use]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// Returns a point translated by the given deltas.
    #[must_use]
    pub const fn offset(self, dx: i32, dy: i32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
        }
    }
}

/// A width and height.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size {
    /// Horizontal extent.
    pub width: i32,
    /// Vertical extent.
    pub height: i32,
}

impl Size {
    /// Creates a size.
    #[must_use]
    pub const fn new(width: i32, height: i32) -> Self {
        Self { width, height }
    }
}

/// An axis-aligned rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Horizontal extent.
    pub width: i32,
    /// Vertical extent.
    pub height: i32,
}

impl Rect {
    /// Creates a rectangle.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Creates a rectangle from a top-left and bottom-right point.
    #[must_use]
    pub const fn from_points(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }
    }

    /// Returns the smallest coordinate past the right edge.
    #[must_use]
    pub const fn right(self) -> i32 {
        self.x + self.width
    }

    /// Returns the smallest coordinate past the bottom edge.
    #[must_use]
    pub const fn bottom(self) -> i32 {
        self.y + self.height
    }

    /// Returns `true` when the rectangle has no area.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    /// Returns the centre point, rounded down.
    #[must_use]
    pub const fn center(self) -> Point {
        Point::new(self.x + self.width / 2, self.y + self.height / 2)
    }

    /// Returns `true` when the point is inside the half-open rectangle.
    #[must_use]
    pub const fn contains(self, point: Point) -> bool {
        point.x >= self.x && point.y >= self.y && point.x < self.right() && point.y < self.bottom()
    }

    /// Returns `true` when the point is within `margin` pixels of the edge.
    #[must_use]
    pub const fn contains_with_margin(self, point: Point, margin: i32) -> bool {
        point.x >= self.x - margin
            && point.y >= self.y - margin
            && point.x < self.right() + margin
            && point.y < self.bottom() + margin
    }

    /// Returns the intersection of two rectangles, if any.
    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= left || bottom <= top {
            None
        } else {
            Some(Self::from_points(left, top, right, bottom))
        }
    }

    /// Returns `true` when the rectangles overlap.
    #[must_use]
    pub fn intersects(self, other: Self) -> bool {
        self.intersection(other).is_some()
    }

    /// Smallest rectangle covering both rectangles.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self::from_points(
            self.x.min(other.x),
            self.y.min(other.y),
            self.right().max(other.right()),
            self.bottom().max(other.bottom()),
        )
    }

    /// Returns a rectangle inset by the same amount on every side.
    #[must_use]
    pub const fn inset(self, amount: i32) -> Self {
        Self::new(
            self.x + amount,
            self.y + amount,
            self.width - amount * 2,
            self.height - amount * 2,
        )
    }

    /// Returns a rectangle with independent horizontal and vertical insets.
    #[must_use]
    pub const fn inset_xy(self, x: i32, y: i32) -> Self {
        Self::new(
            self.x + x,
            self.y + y,
            self.width - x * 2,
            self.height - y * 2,
        )
    }

    /// Returns the rectangle translated by a delta.
    #[must_use]
    pub const fn translate(self, dx: i32, dy: i32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.width, self.height)
    }

    /// Returns the rectangle moved so it is fully inside `bounds`.
    ///
    /// A rectangle larger than the bounds keeps its size but is pinned to the
    /// top-left edge; callers that require a fit must clamp the size first.
    #[must_use]
    pub fn clamp_inside(self, bounds: Self) -> Self {
        let x = if self.width >= bounds.width {
            bounds.x
        } else {
            self.x.clamp(bounds.x, bounds.right() - self.width)
        };
        let y = if self.height >= bounds.height {
            bounds.y
        } else {
            self.y.clamp(bounds.y, bounds.bottom() - self.height)
        };
        Self::new(x, y, self.width, self.height)
    }

    /// Linearly interpolates position and size.
    ///
    /// `amount` is in the inclusive range 0..=256.
    #[must_use]
    pub fn lerp(self, other: Self, amount: u16) -> Self {
        let amount = i64::from(amount.min(256));
        let mix = |from: i32, to: i32| -> i32 {
            let from = i64::from(from);
            let to = i64::from(to);
            (from + (to - from) * amount / 256) as i32
        };
        Self::new(
            mix(self.x, other.x),
            mix(self.y, other.y),
            mix(self.width, other.width),
            mix(self.height, other.height),
        )
    }
}
