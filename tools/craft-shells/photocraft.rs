//! PhotoCraft on æther: the PhotocraftApp the -web crate runs, with import,
//! export and æther's file picker in place of the browser's. Built by
//! craft-build.sh. The canvas composites on the CPU (no wgpu render state).
//!
//! deps: photocraft-ui-egui = { path = "@REPO@/crates/ui-egui" }
//! deps: photocraft-engine = { path = "@REPO@/crates/engine" }
//! deps: photocraft-io = { path = "@REPO@/crates/io" }
//! deps: photocraft-doc = { path = "@REPO@/crates/doc" }
//! deps: egui = "0.36"
//! deps: log = "0.4"
//! eframe: default_fonts, wgpu, accesskit, persistence

use std::sync::Arc;

use photocraft_doc::Document;
use photocraft_engine::Session;
use photocraft_ui_egui::theme::ThemeKind;
use photocraft_ui_egui::{FileDialogAnswer, FileDialogRequest, Inbox, PhotocraftApp, Services};

fn services(inbox: Inbox, place_inbox: Inbox) -> Services {
    Services {
        import: Some(Box::new(|name: &str, bytes: &[u8], max_svg_group_depth: usize| {
            photocraft_io::import_with_svg_group_depth(name, bytes, max_svg_group_depth)
                .map(|r| (r.document, r.warnings))
                .map_err(|e| e.to_string())
        })),
        export: Some(Box::new(|doc: &Document, path: &str, settings: &photocraft_ui_egui::ExportSettings| {
            photocraft_io::export(doc, path, &settings.io_options()).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string())
        })),
        // æther's picker is modal: it answers before returning.
        file_dialog: Some(Box::new(|request, _frame, reply| match request {
            FileDialogRequest::Open { .. } => {
                reply.send(eframe::aether::pick_and_read().map(|(name, bytes)| FileDialogAnswer::Contents(name, bytes)))
            }
            FileDialogRequest::Folder { .. } => reply.send(None),
            // As on the web: the name now, the bytes through `write` (which asks where).
            FileDialogRequest::Save { suggested } => {
                let name = std::path::Path::new(&suggested)
                    .file_name()
                    .map_or_else(|| suggested.clone(), |n| n.to_string_lossy().to_string());
                reply.send(Some(FileDialogAnswer::SaveTo(name)));
            }
        })),
        write: Some(Box::new(|path: &str, bytes: &[u8]| {
            eframe::aether::pick_and_write(path, bytes).map(|_| ()).ok_or_else(|| format!("{path} was not saved"))
        })),
        inbox: Some(inbox),
        place_inbox: Some(place_inbox),
        ..Default::default()
    }
}

fn main() -> eframe::Result {
    eframe::aether::init_logger();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("PhotoCraft").with_inner_size([1440.0, 900.0]),
        ..Default::default()
    };
    eframe::run_native(
        "PhotoCraft",
        options,
        Box::new(|cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, ThemeKind::Pro);
            let inbox: Inbox = Arc::default();
            let place_inbox: Inbox = Arc::default();
            let mut app = PhotocraftApp::new(Session::new(), services(inbox, place_inbox));
            app.set_theme(&cc.egui_ctx, ThemeKind::Pro);
            Ok(Box::new(app))
        }),
    )
}
