// SPDX-License-Identifier: Apache-2.0
//! Fixed-point animation primitives.
//!
//! Animations are expressed in timer ticks so the compositor's motion does not
//! depend on how long a paint takes. One unit of animation progress is 1/256 of
//! the transition; the easing functions are integer-only and monotonic.

/// The value representing a completed animation.
pub const COMPLETE: u16 = 256;

/// A linearly stepped value with an eased accessor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anim {
    value: u16,
    target: u16,
    rate: u16,
}

impl Anim {
    /// A completed animation.
    pub const COMPLETE: Self = Self {
        value: COMPLETE,
        target: COMPLETE,
        rate: COMPLETE,
    };

    /// An animation resting at zero.
    pub const ZERO: Self = Self {
        value: 0,
        target: 0,
        rate: COMPLETE,
    };

    /// Creates an animation starting at `value` and resting there.
    #[must_use]
    pub const fn at(value: u16) -> Self {
        let value = clamp(value);
        Self {
            value,
            target: value,
            rate: COMPLETE,
        }
    }

    /// Creates an animation at zero that will move to [`COMPLETE`].
    #[must_use]
    pub const fn starting(duration_ticks: u16) -> Self {
        Self {
            value: 0,
            target: COMPLETE,
            rate: rate_for(duration_ticks),
        }
    }

    /// Returns the current raw progress.
    #[must_use]
    pub const fn value(self) -> u16 {
        self.value
    }

    /// Returns the eased progress: fast departure, gentle arrival.
    #[must_use]
    pub fn eased(self) -> u16 {
        ease_out_cubic(self.value)
    }

    /// Returns `true` when the value has reached its target.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.value == self.target
    }

    /// Returns `true` when the animation is moving.
    #[must_use]
    pub const fn is_active(self) -> bool {
        self.value != self.target
    }

    /// Retargets the animation. A zero duration completes it immediately.
    pub fn retarget(&mut self, target: u16, duration_ticks: u16) {
        self.target = clamp(target);
        self.rate = rate_for(duration_ticks);
        if duration_ticks == 0 {
            self.value = self.target;
        }
    }

    /// Moves the animation to its target without intermediate frames.
    pub fn finish(&mut self) {
        self.value = self.target;
    }

    /// Resets to zero and animates to completion.
    pub fn restart(&mut self, duration_ticks: u16) {
        self.value = 0;
        self.target = COMPLETE;
        self.rate = rate_for(duration_ticks);
    }

    /// Advances the animation by one tick. Returns `true` when it moved.
    pub fn step(&mut self) -> bool {
        self.advance(1)
    }

    /// Advances the animation by several elapsed ticks.
    ///
    /// The compositor passes the number of timer ticks since the previous
    /// frame, so a slow paint does not also slow wall-clock motion.
    pub fn advance(&mut self, ticks: u16) -> bool {
        if self.value == self.target || ticks == 0 {
            return false;
        }
        let amount = self.rate.saturating_mul(ticks);
        if self.value < self.target {
            self.value = self.value.saturating_add(amount).min(self.target);
        } else {
            self.value = self.value.saturating_sub(amount).max(self.target);
        }
        true
    }
}

const fn clamp(value: u16) -> u16 {
    if value > COMPLETE { COMPLETE } else { value }
}

const fn rate_for(duration_ticks: u16) -> u16 {
    if duration_ticks == 0 {
        COMPLETE
    } else {
        let rate = COMPLETE.div_ceil(duration_ticks);
        if rate == 0 { 1 } else { rate }
    }
}

/// Cubic ease-out scaled to 0..=256.
#[must_use]
pub const fn ease_out_cubic(value: u16) -> u16 {
    let value = clamp(value) as u32;
    let inverse = 256 - value;
    let eased = 256 - (inverse * inverse * inverse) / (256 * 256);
    eased as u16
}

/// Cubic ease-in-out scaled to 0..=256.
#[must_use]
pub const fn ease_in_out(value: u16) -> u16 {
    let value = clamp(value) as u32;
    if value < 128 {
        ((2 * value * value) / 256) as u16
    } else {
        let inverse = 256 - value;
        (256 - (2 * inverse * inverse) / 256) as u16
    }
}
