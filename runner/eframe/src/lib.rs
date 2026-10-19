//! eframe for æther: the public API of eframe 0.36, drawing egui's output in
//! software into an æther window (or, on a Linux host, into a headless
//! window that tests can script and dump as PNG files).
//!
//! Apps build against it unmodified through
//! `[patch.crates-io] eframe = { path = "runner/eframe" }`.
//! Every renderer choice (wgpu, glow) runs the same software path; with the
//! `wgpu` feature the real `wgpu` and `egui-wgpu` crates are re-exported so
//! code naming their types compiles, but no GPU device ever exists.

pub use egui;
pub use egui::{emath, epaint};

#[cfg(feature = "wgpu_no_default_features")]
pub use {egui_wgpu, egui_wgpu::wgpu, egui_wgpu::SurfaceConfig, egui_wgpu::WgpuConfiguration};

#[cfg(feature = "glow")]
pub use glow;

mod epi;
pub use epi::*;

pub mod aether;
pub mod icon_data;
mod input;
pub mod raster;
mod run;
#[cfg(feature = "persistence")]
mod storage;
pub mod winit;

/// The folder where the app with this id keeps its state (a relative path
/// on æther, where an app sees only its private folder).
#[cfg(feature = "persistence")]
pub use storage::storage_dir;

/// Opens a window and runs the app until it closes.
///
/// The window gets `native_options.viewport`'s inner size, clamped to what
/// æther allows (1600x1000). Returns when the app or the user closes it.
pub fn run_native(app_name: &str, native_options: NativeOptions, app_creator: AppCreator<'_>) -> Result {
    run::run(app_name, native_options, None, app_creator)
}

/// Like [`run_native`], with an `egui::Context` made by the caller.
pub fn run_native_ext(
    app_name: &str,
    native_options: NativeOptions,
    egui_ctx: Option<egui::Context>,
    app_creator: AppCreator<'_>,
) -> Result {
    run::run(app_name, native_options, egui_ctx, app_creator)
}

/// The simplest way to run a UI: a closure called every frame.
pub fn run_ui_native(
    app_name: &str,
    native_options: NativeOptions,
    ui_fun: impl FnMut(&mut egui::Ui, &mut Frame) + 'static,
) -> Result {
    struct SimpleApp<U>(U);
    impl<U: FnMut(&mut egui::Ui, &mut Frame) + 'static> App for SimpleApp<U> {
        fn ui(&mut self, ui: &mut egui::Ui, frame: &mut Frame) {
            (self.0)(ui, frame);
        }
    }
    run_native(app_name, native_options, Box::new(|_cc| Ok(Box::new(SimpleApp(ui_fun)))))
}

/// The different problems that can occur when trying to run `eframe`.
#[derive(Debug)]
pub enum Error {
    /// The app creator returned an error, or the window couldn't be made.
    AppCreation(Box<dyn std::error::Error + Send + Sync>),
    /// Never produced on æther; here so `if let Err(Error::Wgpu(e))` compiles.
    #[cfg(feature = "wgpu_no_default_features")]
    Wgpu(egui_wgpu::WgpuError),
}

impl std::error::Error for Error {}

#[cfg(feature = "wgpu_no_default_features")]
impl From<egui_wgpu::WgpuError> for Error {
    fn from(err: egui_wgpu::WgpuError) -> Self {
        Self::Wgpu(err)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AppCreation(err) => write!(f, "app creation error: {err}"),
            #[cfg(feature = "wgpu_no_default_features")]
            Self::Wgpu(err) => write!(f, "WGPU error: {err}"),
        }
    }
}

/// Short for `Result<T, eframe::Error>`.
pub type Result<T = (), E = Error> = std::result::Result<T, E>;
