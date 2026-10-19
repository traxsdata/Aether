//! GridCraft on æther: the same SheetApp the -web crate runs, with
//! æther's file picker for Open, Save and pictures. Built by craft-build.sh.
//!
//! deps: gridcraft-ui-egui = { path = "@REPO@/crates/ui-egui" }
//! deps: gridcraft-engine = { path = "@REPO@/crates/engine" }
//! deps: egui = "0.36"
//! deps: serde_json = "1"
//! deps: log = "0.4"
//! eframe: default_fonts, persistence

use gridcraft_engine::Session;
use gridcraft_ui_egui::{Inbox, Services, SheetApp};

struct Shell {
    app: SheetApp,
}

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.logic(ctx);
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.app.raw_input_hook(raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
    }
}

fn services(inbox: Inbox, ctx: egui::Context) -> Services {
    let open_inbox = inbox.clone();
    Services {
        open_async: Some(Box::new(move || {
            if let Some(file) = eframe::aether::pick_and_read() {
                open_inbox.lock().unwrap_or_else(|e| e.into_inner()).push(file);
                ctx.request_repaint();
            }
        })),
        pick_picture_async: Some(Box::new(|inbox: Inbox| {
            if let Some(file) = eframe::aether::pick_and_read() {
                inbox.lock().unwrap_or_else(|e| e.into_inner()).push(file);
            }
        })),
        download: Some(Box::new(|name: &str, bytes: &[u8]| {
            if eframe::aether::pick_and_write(name, bytes).is_none() {
                log::warn!("{name} was not saved");
            }
        })),
        inbox: Some(inbox),
        ..Default::default()
    }
}

fn main() -> eframe::Result {
    eframe::aether::init_logger();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("GridCraft").with_inner_size([1440.0, 900.0]),
        ..Default::default()
    };
    eframe::run_native(
        "GridCraft",
        options,
        Box::new(|cc| {
            let inbox = Inbox::default();
            let mut session = Session::new();
            let sample = std::env::var("CRAFT_SAMPLE").unwrap_or_else(|_| "sales".into());
            if !sample.is_empty() {
                let _ = session.execute("file.new", serde_json::json!({ "sample": sample }));
            }
            if session.documents().is_empty() {
                session.new_workbook();
            }
            let mut app = SheetApp::new(session, services(inbox, cc.egui_ctx.clone()));
            app.after_engine();
            SheetApp::setup_context_for_language(&cc.egui_ctx, app.ui.dark, app.ui.language);
            Ok(Box::new(Shell { app }))
        }),
    )
}
