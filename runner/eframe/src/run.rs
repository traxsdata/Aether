//! run.rs - `run_native`: one window, the egui frame loop, software paint.
//!
//! Each turn: wait for input or the next repaint deadline, gather every
//! pending event into a RawInput, run the app (`logic` then `ui`), apply the
//! texture deltas, tessellate, rasterize into our own buffer, copy it into
//! the window and present. Viewport commands we can honour (Close, Title,
//! Screenshot) are; the rest are ignored.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use egui::{ViewportCommand, ViewportId, ViewportInfo};

use crate::aether::with_backend;
use crate::input::InputState;
use crate::raster::{Rasterizer, Stats, Target};
use crate::{epi, App, AppCreator, Error, Frame, NativeOptions, Result};

/// Longest sleep. On æther another thread's `request_repaint` interrupts the
/// wait (SYS_EVENT_WAKE); headless, idle waits are cut into slices this long.
const POLL_MS: u64 = 100;
/// Most events folded into one frame.
const MAX_BATCH: usize = 512;

#[derive(Default)]
struct Timing {
    frames: u64,
    ui_ms: f64,
    tess_ms: f64,
    raster_ms: f64,
    present_ms: f64,
    stats: Stats,
}

fn now_ms() -> u64 {
    with_backend(|b| b.uptime_ms())
}

/// Wall time for profiling, independent of the (possibly virtual) backend clock.
fn wall() -> std::time::Instant {
    std::time::Instant::now()
}


fn env_num<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok()?.trim().parse().ok()
}

pub fn run(app_name: &str, mut options: NativeOptions, egui_ctx: Option<egui::Context>, app_creator: AppCreator<'_>) -> Result {
    with_backend(|_| ()); // open it now, from AETHER=1 or the headless environment
    if let Some(hook) = options.window_builder.take() {
        options.viewport = hook(options.viewport.clone());
    }
    let vp = &options.viewport;
    let title = vp.title.clone().unwrap_or_else(|| app_name.to_owned());
    let app_id = vp.app_id.clone().unwrap_or_else(|| app_name.to_owned());

    // æther has no display scaling yet; AETHER_PPP overrides for testing.
    let native_ppp: f32 = env_num("AETHER_PPP").filter(|p: &f32| *p > 0.25 && *p < 8.0).unwrap_or(1.0);
    let mut size = vp.inner_size.unwrap_or(egui::vec2(800.0, 600.0));
    if let Some(min) = vp.min_inner_size {
        size = size.max(min);
    }
    if let Some(max) = vp.max_inner_size {
        size = size.min(max);
    }
    // Clamped to the largest window that fits between the top bar and the dock.
    let display = with_backend(|b| b.display_info());
    let mut want_w = ((size.x * native_ppp).round() as u32).clamp(64, display.max_client_w.max(64));
    let mut want_h = ((size.y * native_ppp).round() as u32).clamp(48, display.max_client_h.max(48));
    // AETHER_WINDOW=WxH overrides the app's choice (benchmarks, screenshots).
    if let Some((w, h)) = std::env::var("AETHER_WINDOW").ok().and_then(|s| {
        let (w, h) = s.split_once('x')?;
        Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
    }) {
        (want_w, want_h) = (w, h);
    }
    let (win, mut width, mut height) = with_backend(|b| b.create_window(want_w, want_h, &title))
        .map_err(|e| Error::AppCreation(format!("can't create window: {e}").into()))?;
    log::debug!("window {win}: {width}x{height} ({title})");
    // Every egui layout reflows, so windows are resizable unless the app says otherwise.
    let min = vp.min_inner_size.map(|m| m * native_ppp).unwrap_or(egui::vec2(0.0, 0.0));
    let resizable = vp.resizable.unwrap_or(true) && !matches!((vp.min_inner_size, vp.max_inner_size), (Some(a), Some(b)) if a == b);
    let _ = with_backend(|b| b.set_resizable(win, resizable, min.x.max(0.0) as u32, min.y.max(0.0) as u32));

    let storage = create_storage(&options, &app_id);
    let ctx = egui_ctx.unwrap_or_default();
    ctx.set_embed_viewports(true); // one window: child viewports draw inside it
    ctx.options_mut(|o| o.max_passes = std::num::NonZeroUsize::new(2).unwrap_or(std::num::NonZeroUsize::MIN));
    #[cfg(feature = "persistence")]
    if let Some(mem) = storage.as_deref().and_then(|s| epi::get_value::<egui::Memory>(s, STORAGE_EGUI_MEMORY_KEY)) {
        ctx.memory_mut(|m| *m = mem);
    }

    // Repaint requests from other threads; ours are read from the frame output.
    let wake = Arc::new(AtomicBool::new(false));
    {
        let wake = wake.clone();
        // On æther the kernel can interrupt our wait (SYS_EVENT_WAKE); the
        // headless backend only polls.
        let on_aether = with_backend(|b| b.name() == "aether");
        ctx.set_request_repaint_callback(move |info| {
            if info.viewport_id == ViewportId::ROOT && info.delay < Duration::from_millis(POLL_MS) {
                if !wake.swap(true, Ordering::AcqRel) && on_aether {
                    let _ = aether_host::sys::event_wake();
                }
            }
        });
    }

    let mut input = InputState::new();
    let viewport_info = |w: u32, h: u32, focused: bool, title: &str, ppp: f32| ViewportInfo {
        title: Some(title.to_owned()),
        native_pixels_per_point: Some(native_ppp),
        monitor_size: Some(egui::vec2(display.screen_w as f32, display.screen_h as f32) / ppp),
        inner_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w as f32, h as f32) / ppp)),
        outer_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w as f32, h as f32) / ppp)),
        minimized: Some(false),
        maximized: Some(false),
        fullscreen: Some(false),
        focused: Some(focused),
        occluded: Some(false),
        ..Default::default()
    };
    // Let the app see its window size and scale while it is created.
    ctx.input_mut(|i| {
        i.raw.viewports.insert(ViewportId::ROOT, viewport_info(width, height, true, &title, native_ppp));
        i.pixels_per_point = native_ppp;
    });

    let mut frame = Frame::new(storage);
    let mut app: Box<dyn App + '_> = {
        let cc = epi::CreationContext {
            egui_ctx: ctx.clone(),
            integration_info: frame.info.clone(),
            storage: frame.storage(),
            #[cfg(feature = "glow")]
            gl: None,
            #[cfg(feature = "glow")]
            get_proc_address: None,
            #[cfg(feature = "wgpu_no_default_features")]
            wgpu_render_state: None,
        };
        app_creator(&cc).map_err(Error::AppCreation)?
    };

    let threads: usize = env_num("AETHER_RASTER_THREADS").unwrap_or(1).clamp(1, 16);
    let report = std::env::var_os("AETHER_STATS").is_some();
    let max_frames: Option<u64> = env_num("AETHER_MAX_FRAMES");
    let mut raster = Rasterizer::new();
    let mut canvas = vec![0u32; (width * height) as usize];
    let mut timing = Timing::default();
    let start = now_ms();
    let mut last_save = start;
    let mut repaint_at: Option<u64> = Some(start);
    let mut title_now = title.clone();
    let mut screenshots: Vec<egui::UserData> = Vec::new();
    let mut pending_events: Vec<egui::Event> = Vec::new();

    loop {
        // ---- Wait for input or the next repaint.
        let now = now_ms();
        let timeout = if wake.swap(false, Ordering::AcqRel) {
            Some(0)
        } else {
            match repaint_at {
                Some(t) => Some(t.saturating_sub(now).min(POLL_MS) as u32),
                None => Some(POLL_MS as u32),
            }
        };
        let mut got = 0;
        let mut next = with_backend(|b| b.wait_event(timeout));
        let ppp_now = ctx.zoom_factor() * native_ppp;
        while let Ok(Some(ev)) = next {
            input.on_event(ev, ppp_now);
            got += 1;
            if got >= MAX_BATCH {
                break;
            }
            next = with_backend(|b| b.wait_event(Some(0)));
        }
        if let Err(e) = next {
            log::error!("event wait failed: {e}; stopping");
            break;
        }
        if let Some((w, h)) = input.resized.take() {
            if (w, h) != (width, height) {
                (width, height) = (w, h);
                canvas = vec![0; (w * h) as usize];
            }
        }
        let now = now_ms();
        let due = repaint_at.is_some_and(|t| t <= now) || wake.swap(false, Ordering::AcqRel);
        if got == 0 && !due && !input.close_requested {
            continue;
        }

        // ---- One egui frame.
        let t0 = wall();
        let ppp = ctx.zoom_factor() * native_ppp;
        let mut raw = egui::RawInput {
            viewport_id: ViewportId::ROOT,
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width as f32, height as f32) / ppp)),
            max_texture_side: Some(8192),
            time: Some((now - start) as f64 / 1000.0),
            predicted_dt: 1.0 / 60.0,
            focused: input.focused,
            ..Default::default()
        };
        let mut info = viewport_info(width, height, input.focused, &title_now, ppp);
        if std::mem::take(&mut input.close_requested) {
            info.events.push(egui::ViewportEvent::Close);
        }
        raw.viewports.insert(ViewportId::ROOT, info);
        raw.events = std::mem::take(&mut pending_events);
        raw.events.append(&mut input.events);
        let close_requested = raw.viewport().close_requested();
        app.raw_input_hook(&ctx, &mut raw);

        let mut full = ctx.run_ui(raw, |ui| {
            app.logic(ui.ctx(), &mut frame);
            app.ui(ui, &mut frame);
        });
        let t1 = wall();

        // ---- Platform output.
        for cmd in &full.platform_output.commands {
            match cmd {
                egui::OutputCommand::CopyText(text) => input.clipboard = text.clone(),
                egui::OutputCommand::CopyImage(_) => log::debug!("image copy: no image clipboard on æther"),
                egui::OutputCommand::OpenUrl(u) => log::info!("open URL (no browser on æther): {}", u.url),
            }
        }
        let mut quit = false;
        let mut repaint_delay = Duration::MAX;
        if let Some(vo) = full.viewport_output.get(&ViewportId::ROOT) {
            repaint_delay = vo.repaint_delay;
            let cancelled = vo.commands.contains(&ViewportCommand::CancelClose);
            quit |= close_requested && !cancelled;
            for c in &vo.commands {
                match c {
                    ViewportCommand::Close => quit = true,
                    ViewportCommand::Title(t) if *t != title_now => {
                        title_now = t.clone();
                        let _ = with_backend(|b| b.set_title(win, t));
                    }
                    ViewportCommand::Screenshot(data) => screenshots.push(data.clone()),
                    _ => {}
                }
            }
        }

        // ---- Paint.
        // Taken out and cleared once applied: egui asserts (in debug builds)
        // that no TexturesDelta is dropped with deltas still in it.
        let mut textures = std::mem::take(&mut full.textures_delta);
        for (id, deltas) in &textures.set {
            for d in deltas {
                raster.set_texture(*id, d);
            }
        }
        let pixels_per_point = full.pixels_per_point;
        let prims = ctx.tessellate(full.shapes, pixels_per_point);
        let t2 = wall();
        let [r, g, b, _] = app.clear_color(&ctx.global_style().visuals);
        let to8 = |f: f32| (f.clamp(0.0, 1.0) * 255.0).round() as u32;
        canvas.fill((to8(r) << 16) | (to8(g) << 8) | to8(b));
        let stats = paint(&raster, &mut canvas, width as usize, height as usize, &prims, pixels_per_point, threads);
        for id in &textures.free {
            raster.free_texture(*id);
        }
        textures.clear();
        let t3 = wall();
        let presented = with_backend(|b| {
            if let Some(fb) = b.framebuffer(win) {
                let (w, h, pitch) = (width.min(fb.width) as usize, height.min(fb.height) as usize, fb.pitch as usize);
                for y in 0..h {
                    fb.pixels[y * pitch..y * pitch + w].copy_from_slice(&canvas[y * width as usize..y * width as usize + w]);
                }
            }
            b.present(win)
        });
        if let Err(e) = presented {
            log::warn!("present failed: {e}");
        }
        let t4 = wall();

        for user_data in screenshots.drain(..) {
            let pixels = canvas
                .iter()
                .map(|&p| egui::Color32::from_rgb((p >> 16) as u8, (p >> 8) as u8, p as u8))
                .collect();
            let image = egui::ColorImage::new([width as usize, height as usize], pixels);
            pending_events.push(egui::Event::Screenshot { viewport_id: ViewportId::ROOT, user_data, image: Arc::new(image) });
            repaint_at = Some(now);
        }

        // ---- Bookkeeping.
        let cpu = (t4 - t0).as_secs_f32();
        frame.info.cpu_usage = Some(cpu);
        timing.frames += 1;
        timing.ui_ms += (t1 - t0).as_secs_f64() * 1000.0;
        timing.tess_ms += (t2 - t1).as_secs_f64() * 1000.0;
        timing.raster_ms += (t3 - t2).as_secs_f64() * 1000.0;
        timing.present_ms += (t4 - t3).as_secs_f64() * 1000.0;
        timing.stats = stats;
        if report {
            eprintln!(
                "frame {}: ui {:.2} ms, tessellate {:.2} ms, raster {:.2} ms, present {:.2} ms ({} meshes, {} rects, {} triangles)",
                timing.frames,
                (t1 - t0).as_secs_f64() * 1000.0,
                (t2 - t1).as_secs_f64() * 1000.0,
                (t3 - t2).as_secs_f64() * 1000.0,
                (t4 - t3).as_secs_f64() * 1000.0,
                stats.meshes,
                stats.rects,
                stats.triangles
            );
        }
        if pending_events.is_empty() {
            repaint_at = if repaint_delay == Duration::MAX {
                None
            } else {
                Some(now + repaint_delay.as_millis().min(u64::MAX as u128) as u64)
            };
        }
        let now = now_ms();
        if now.saturating_sub(last_save) >= app.auto_save_interval().as_millis() as u64 {
            save(app.as_mut(), &mut frame, &ctx);
            last_save = now;
        }
        if quit || max_frames.is_some_and(|m| timing.frames >= m) {
            break;
        }
    }

    save(app.as_mut(), &mut frame, &ctx);
    #[cfg(feature = "glow")]
    app.on_exit(None);
    #[cfg(not(feature = "glow"))]
    app.on_exit();
    if report && timing.frames > 0 {
        let n = timing.frames as f64;
        eprintln!(
            "{} frames at {width}x{height}: ui {:.2} ms, tessellate {:.2} ms, raster {:.2} ms, present {:.2} ms per frame on average ({} threads)",
            timing.frames,
            timing.ui_ms / n,
            timing.tess_ms / n,
            timing.raster_ms / n,
            timing.present_ms / n,
            threads
        );
    }
    Ok(())
}

/// Rasterizes into `canvas`, split into horizontal bands when `threads` > 1.
fn paint(
    raster: &Rasterizer,
    canvas: &mut [u32],
    w: usize,
    h: usize,
    prims: &[egui::ClippedPrimitive],
    ppp: f32,
    threads: usize,
) -> Stats {
    if threads <= 1 || h < threads * 16 {
        return raster.paint(&mut Target { px: canvas, width: w, height: h, y0: 0 }, prims, ppp);
    }
    let rows = h.div_ceil(threads);
    std::thread::scope(|s| {
        let jobs: Vec<_> = canvas
            .chunks_mut(rows * w)
            .enumerate()
            .map(|(i, band)| {
                let height = band.len() / w;
                s.spawn(move || raster.paint(&mut Target { px: band, width: w, height, y0: (i * rows) as i32 }, prims, ppp))
            })
            .collect();
        let mut total = Stats::default();
        for j in jobs {
            if let Ok(st) = j.join() {
                total.meshes = total.meshes.max(st.meshes);
                total.rects += st.rects;
                total.triangles += st.triangles;
                total.callbacks += st.callbacks;
            }
        }
        total
    })
}

#[cfg(feature = "persistence")]
const STORAGE_EGUI_MEMORY_KEY: &str = "egui";

fn create_storage(options: &NativeOptions, app_id: &str) -> Option<Box<dyn epi::Storage>> {
    #[cfg(feature = "persistence")]
    {
        if let Some(path) = &options.persistence_path {
            return Some(Box::new(crate::storage::FileStorage::from_path(path.clone())));
        }
        if let Some(s) = crate::storage::FileStorage::from_app_id(app_id) {
            return Some(Box::new(s));
        }
    }
    let _ = (options, app_id);
    None
}

fn save(app: &mut dyn App, frame: &mut Frame, ctx: &egui::Context) {
    let Some(storage) = frame.storage_mut() else { return };
    #[cfg(feature = "persistence")]
    if app.persist_egui_memory() {
        ctx.memory(|mem| epi::set_value(storage, STORAGE_EGUI_MEMORY_KEY, mem));
    }
    let _ = ctx;
    app.save(storage);
    storage.flush();
}
