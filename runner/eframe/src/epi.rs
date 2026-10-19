//! epi.rs - eframe's app-facing types, with the same names and signatures as
//! eframe 0.36 on native targets.

use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle};

type DynError = Box<dyn std::error::Error + Send + Sync>;

/// This is how your app is created.
pub type AppCreator<'app> =
    Box<dyn 'app + FnOnce(&CreationContext<'_>) -> Result<Box<dyn 'app + App>, DynError>>;

/// Hook into the building of the native window.
pub type WindowBuilderHook = Box<dyn FnOnce(egui::ViewportBuilder) -> egui::ViewportBuilder>;

/// Data passed to the app creator.
pub struct CreationContext<'s> {
    pub egui_ctx: egui::Context,
    pub integration_info: IntegrationInfo,
    pub storage: Option<&'s dyn Storage>,
    #[cfg(feature = "glow")]
    pub gl: Option<std::sync::Arc<glow::Context>>,
    #[cfg(feature = "glow")]
    pub get_proc_address: Option<std::sync::Arc<dyn Fn(&std::ffi::CStr) -> *const std::ffi::c_void + Send + Sync>>,
    /// Always `None`: there is no GPU; egui is drawn in software.
    #[cfg(feature = "wgpu_no_default_features")]
    pub wgpu_render_state: Option<egui_wgpu::RenderState>,
}

impl CreationContext<'_> {
    #[doc(hidden)]
    pub fn _new_kittest(egui_ctx: egui::Context) -> Self {
        Self {
            egui_ctx,
            integration_info: IntegrationInfo::default(),
            storage: None,
            #[cfg(feature = "glow")]
            gl: None,
            #[cfg(feature = "glow")]
            get_proc_address: None,
            #[cfg(feature = "wgpu_no_default_features")]
            wgpu_render_state: None,
        }
    }
}

impl HasWindowHandle for CreationContext<'_> {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Err(HandleError::NotSupported)
    }
}

impl HasDisplayHandle for CreationContext<'_> {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Err(HandleError::NotSupported)
    }
}

/// Implement this trait to write apps that can be compiled for both web/wasm and desktop/native.
pub trait App {
    /// Called each frame before `ui`, outside any panel.
    fn logic(&mut self, ctx: &egui::Context, frame: &mut Frame) {
        _ = (ctx, frame);
    }

    /// Called each time the UI needs repainting.
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut Frame);

    /// Called on shutdown, and perhaps at regular intervals.
    fn save(&mut self, _storage: &mut dyn Storage) {}

    /// Called once on shutdown, after [`Self::save`].
    #[cfg(feature = "glow")]
    fn on_exit(&mut self, _gl: Option<&glow::Context>) {}

    /// Called once on shutdown, after [`Self::save`].
    #[cfg(not(feature = "glow"))]
    fn on_exit(&mut self) {}

    /// Time between automatic calls to [`Self::save`].
    fn auto_save_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(30)
    }

    /// Background colour values, premultiplied sRGBA in 0..1.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Color32::from_rgba_unmultiplied(12, 12, 12, 180).to_normalized_gamma_f32()
    }

    /// Whether egui's memory (window positions etc.) is saved.
    fn persist_egui_memory(&self) -> bool {
        true
    }

    /// A hook for manipulating or filtering raw input before egui sees it.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, _raw_input: &mut egui::RawInput) {}
}

/// Options controlling the behaviour of a native window.
pub struct NativeOptions {
    pub viewport: egui::ViewportBuilder,
    pub multisampling: u16,
    pub depth_buffer: u8,
    pub stencil_buffer: u8,
    #[cfg(any(feature = "glow", feature = "wgpu_no_default_features"))]
    pub renderer: Renderer,
    pub run_and_return: bool,
    pub window_builder: Option<WindowBuilderHook>,
    pub centered: bool,
    #[cfg(feature = "wgpu_no_default_features")]
    pub wgpu_options: egui_wgpu::WgpuConfiguration,
    pub persist_window: bool,
    /// Where the app's state is saved; overrides the default storage location.
    pub persistence_path: Option<std::path::PathBuf>,
    pub dithering: bool,
}

impl Clone for NativeOptions {
    fn clone(&self) -> Self {
        Self {
            viewport: self.viewport.clone(),
            window_builder: None, // hooks aren't clonable, as in eframe
            #[cfg(feature = "wgpu_no_default_features")]
            wgpu_options: self.wgpu_options.clone(),
            persistence_path: self.persistence_path.clone(),
            ..*self
        }
    }
}

impl Default for NativeOptions {
    fn default() -> Self {
        Self {
            viewport: Default::default(),
            multisampling: 0,
            depth_buffer: 0,
            stencil_buffer: 0,
            #[cfg(any(feature = "glow", feature = "wgpu_no_default_features"))]
            renderer: Renderer::default(),
            run_and_return: true,
            window_builder: None,
            centered: false,
            #[cfg(feature = "wgpu_no_default_features")]
            wgpu_options: egui_wgpu::WgpuConfiguration::default()
                .with_surface_config(egui_wgpu::SurfaceConfig::LOW_LATENCY),
            persist_window: true,
            persistence_path: None,
            dithering: true,
        }
    }
}

/// What rendering backend to use. Whichever is asked for, æther draws in software.
#[cfg(any(feature = "glow", feature = "wgpu_no_default_features"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "persistence", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "persistence", serde(rename_all = "snake_case"))]
pub enum Renderer {
    #[cfg(feature = "glow")]
    Glow,
    #[cfg(feature = "wgpu_no_default_features")]
    Wgpu,
}

#[cfg(any(feature = "glow", feature = "wgpu_no_default_features"))]
impl Default for Renderer {
    fn default() -> Self {
        #[cfg(feature = "wgpu_no_default_features")]
        return Self::Wgpu;
        #[cfg(not(feature = "wgpu_no_default_features"))]
        return Self::Glow;
    }
}

#[cfg(any(feature = "glow", feature = "wgpu_no_default_features"))]
impl std::fmt::Display for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "glow")]
            Self::Glow => "glow".fmt(f),
            #[cfg(feature = "wgpu_no_default_features")]
            Self::Wgpu => "wgpu".fmt(f),
        }
    }
}

#[cfg(any(feature = "glow", feature = "wgpu_no_default_features"))]
impl std::str::FromStr for Renderer {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, String> {
        match name.to_lowercase().as_str() {
            #[cfg(feature = "glow")]
            "glow" => Ok(Self::Glow),
            #[cfg(feature = "wgpu_no_default_features")]
            "wgpu" => Ok(Self::Wgpu),
            _ => Err(format!(
                "eframe renderer {name:?} is not available. Make sure that the corresponding eframe feature is enabled."
            )),
        }
    }
}

/// Your handle to the running app, passed to [`App::ui`] and [`App::logic`].
pub struct Frame {
    pub(crate) info: IntegrationInfo,
    pub(crate) storage: Option<Box<dyn Storage>>,
    #[cfg(feature = "glow")]
    pub(crate) gl: Option<std::sync::Arc<glow::Context>>,
    #[cfg(feature = "wgpu_no_default_features")]
    #[doc(hidden)]
    pub wgpu_render_state: Option<egui_wgpu::RenderState>,
}

impl HasWindowHandle for Frame {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Err(HandleError::NotSupported)
    }
}

impl HasDisplayHandle for Frame {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Err(HandleError::NotSupported)
    }
}

impl Frame {
    #[doc(hidden)]
    pub fn _new_kittest() -> Self {
        Self::new(None)
    }

    pub(crate) fn new(storage: Option<Box<dyn Storage>>) -> Self {
        Self {
            info: IntegrationInfo::default(),
            storage,
            #[cfg(feature = "glow")]
            gl: None,
            #[cfg(feature = "wgpu_no_default_features")]
            wgpu_render_state: None,
        }
    }

    pub fn is_web(&self) -> bool {
        false
    }

    pub fn info(&self) -> &IntegrationInfo {
        &self.info
    }

    pub fn storage(&self) -> Option<&dyn Storage> {
        self.storage.as_deref()
    }

    pub fn storage_mut(&mut self) -> Option<&mut (dyn Storage + 'static)> {
        self.storage.as_deref_mut()
    }

    /// Always `None`: æther windows are not winit windows. See [`crate::winit`].
    pub fn winit_window(&self) -> Option<&std::sync::Arc<crate::winit::window::Window>> {
        None
    }

    #[cfg(feature = "glow")]
    pub fn gl(&self) -> Option<&std::sync::Arc<glow::Context>> {
        self.gl.as_ref()
    }

    /// Always `None` on æther: there is no GPU device.
    #[cfg(feature = "wgpu_no_default_features")]
    pub fn wgpu_render_state(&self) -> Option<&egui_wgpu::RenderState> {
        self.wgpu_render_state.as_ref()
    }

    #[cfg(feature = "wgpu_no_default_features")]
    pub fn wgpu_surface_config(&self) -> Option<egui_wgpu::SurfaceConfig> {
        self.wgpu_render_state.as_ref().map(|s| s.surface_config)
    }

    #[cfg(feature = "wgpu_no_default_features")]
    pub fn set_wgpu_surface_config(&mut self, config: egui_wgpu::SurfaceConfig) {
        if let Some(state) = &mut self.wgpu_render_state {
            state.surface_config = config;
        }
    }
}

/// Information about the integration passed to the app.
#[derive(Clone, Debug, Default)]
pub struct IntegrationInfo {
    /// Seconds of CPU the last frame took (logic, tessellation and rasterizing).
    pub cpu_usage: Option<f32>,
}

/// A place where you can store custom data in a way that persists when you restart the app.
pub trait Storage {
    fn get_string(&self, key: &str) -> Option<String>;
    fn set_string(&mut self, key: &str, value: String);
    fn remove_string(&mut self, key: &str);
    /// Write-to-disk, if the data changed.
    fn flush(&mut self);
}

/// Get and deserialize the [RON](https://github.com/ron-rs/ron) stored at the given key.
#[cfg(feature = "persistence")]
pub fn get_value<T: serde::de::DeserializeOwned>(storage: &dyn Storage, key: &str) -> Option<T> {
    let value = storage.get_string(key)?;
    match ron::from_str(&value) {
        Ok(value) => Some(value),
        Err(err) => {
            log::debug!("Failed to decode RON: {err}");
            None
        }
    }
}

/// Serialize the given value as [RON](https://github.com/ron-rs/ron) and store with the given key.
#[cfg(feature = "persistence")]
pub fn set_value<T: serde::Serialize>(storage: &mut dyn Storage, key: &str, value: &T) {
    match ron::ser::to_string(value) {
        Ok(string) => storage.set_string(key, string),
        Err(err) => log::error!("eframe failed to encode data using ron: {err}"),
    }
}

/// [`Storage`] key used for app.
pub const APP_KEY: &str = "app";
