// SPDX-License-Identifier: Apache-2.0
//! Super-tap gesture tracking: Super pressed and released alone, quickly.

/// A Super tap must complete within this many scheduler ticks.
///
/// The timer runs at 100 Hz, so 30 ticks is 300 ms.
pub const SUPER_TAP_MAX_TICKS: u64 = 30;

/// Tracks Super presses for the tap gesture: Super goes down and back up
/// within the window, with no other key in between (Start-menu style).
///
/// The struct is pure and host-testable: the caller drives every transition
/// and passes the current timer tick. Nothing here reads clocks or globals.
#[derive(Clone, Copy, Debug, Default)]
pub struct SuperTapTracker {
    /// Timer tick at which Super last went down, when it is held.
    pressed_at: Option<u64>,
    /// A key other than Super went down during the hold, so what follows a
    /// Super break is a chord, not a tap.
    interrupted: bool,
}

impl SuperTapTracker {
    /// Creates a tracker with Super released and no tap pending.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pressed_at: None,
            interrupted: false,
        }
    }

    /// A Super key make scancode arrived.
    pub fn super_down(&mut self, now: u64) {
        self.pressed_at = Some(now);
        self.interrupted = false;
    }

    /// A make scancode for a key other than Super arrived while Super was
    /// held. Regular keys are visible as decoded events; Ctrl/Alt/Shift makes
    /// are not, so the caller reports both here.
    pub fn other_down(&mut self) {
        if self.pressed_at.is_some() {
            self.interrupted = true;
        }
    }

    /// A Super key break scancode arrived.
    ///
    /// Returns `true` when the hold was a tap: Super alone, and released
    /// within `SUPER_TAP_MAX_TICKS`. Every Super break consumes the hold.
    pub fn super_up(&mut self, now: u64) -> bool {
        let tapped = match self.pressed_at {
            Some(start) if !self.interrupted => now.saturating_sub(start) <= SUPER_TAP_MAX_TICKS,
            _ => false,
        };
        self.pressed_at = None;
        self.interrupted = false;
        tapped
    }
}
