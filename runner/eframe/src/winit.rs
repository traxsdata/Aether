//! A tiny stand-in for the parts of `winit` that apps reach through
//! [`crate::Frame::winit_window`]. æther has no winit, so that call always
//! returns `None`. These types exist only so code written against the real
//! window still type-checks; none of them can be built from outside.

/// Pixel positions and sizes.
pub mod dpi {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct PhysicalPosition<P> {
        pub x: P,
        pub y: P,
    }

    impl<P> PhysicalPosition<P> {
        pub const fn new(x: P, y: P) -> Self {
            Self { x, y }
        }
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct PhysicalSize<P> {
        pub width: P,
        pub height: P,
    }

    impl<P> PhysicalSize<P> {
        pub const fn new(width: P, height: P) -> Self {
            Self { width, height }
        }
    }
}

/// Errors from window calls.
pub mod error {
    use std::fmt;

    #[derive(Clone, Debug)]
    pub struct NotSupportedError(pub(super) ());

    impl fmt::Display for NotSupportedError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("the requested operation is not supported on æther")
        }
    }

    impl std::error::Error for NotSupportedError {}

    #[derive(Debug)]
    pub enum ExternalError {
        NotSupported(NotSupportedError),
    }

    impl fmt::Display for ExternalError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::NotSupported(e) => e.fmt(f),
            }
        }
    }

    impl std::error::Error for ExternalError {}
}

/// Monitors.
pub mod monitor {
    use super::dpi::{PhysicalPosition, PhysicalSize};

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub struct MonitorHandle(());

    impl MonitorHandle {
        pub fn name(&self) -> Option<String> {
            None
        }
        pub fn position(&self) -> PhysicalPosition<i32> {
            PhysicalPosition::new(0, 0)
        }
        pub fn size(&self) -> PhysicalSize<u32> {
            PhysicalSize::new(0, 0)
        }
        pub fn scale_factor(&self) -> f64 {
            1.0
        }
    }
}

/// The window.
pub mod window {
    use super::dpi::{PhysicalPosition, PhysicalSize};
    use super::error::{ExternalError, NotSupportedError};
    use super::monitor::MonitorHandle;

    #[derive(Debug)]
    pub struct Window(());

    impl Window {
        pub fn current_monitor(&self) -> Option<MonitorHandle> {
            None
        }
        pub fn available_monitors(&self) -> impl Iterator<Item = MonitorHandle> {
            std::iter::empty()
        }
        pub fn primary_monitor(&self) -> Option<MonitorHandle> {
            None
        }
        pub fn inner_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
            Err(NotSupportedError(()))
        }
        pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
            Err(NotSupportedError(()))
        }
        pub fn inner_size(&self) -> PhysicalSize<u32> {
            PhysicalSize::new(0, 0)
        }
        pub fn scale_factor(&self) -> f64 {
            1.0
        }
        pub fn set_cursor_position<P: Into<PhysicalPosition<i32>>>(&self, _position: P) -> Result<(), ExternalError> {
            Err(ExternalError::NotSupported(NotSupportedError(())))
        }
        pub fn request_redraw(&self) {}
    }
}
