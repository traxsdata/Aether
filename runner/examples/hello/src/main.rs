//! hello - a small eframe app for the æther runner: a label, a button, a
//! slider, a text field and a painted plot. It is plain eframe code; the
//! workspace's [patch.crates-io] makes it draw through æther.

use eframe::egui;

struct Hello {
    name: String,
    clicks: u32,
    freq: f32,
}

impl Default for Hello {
    fn default() -> Self {
        Hello { name: "æther".into(), clicks: 0, freq: 2.0 }
    }
}

impl eframe::App for Hello {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Hello from eframe on æther");
            ui.label(format!("Hello, {}! The button was clicked {} times.", self.name, self.clicks));
            ui.horizontal(|ui| {
                ui.label("Your name:");
                ui.text_edit_singleline(&mut self.name);
            });
            if ui.button("Click me").clicked() {
                self.clicks += 1;
            }
            ui.add(egui::Slider::new(&mut self.freq, 0.5..=8.0).text("frequency"));
            ui.separator();
            plot(ui, self.freq);
        });
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &(self.name.clone(), self.clicks));
    }
}

/// A sine wave and a few bars, painted directly.
fn plot(ui: &mut egui::Ui, freq: f32) {
    let size = egui::vec2(ui.available_width(), 220.0_f32.min(ui.available_height()));
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 6.0, ui.visuals().extreme_bg_color);
    for i in 0..8 {
        let x = rect.left() + 20.0 + i as f32 * 30.0;
        let h = 20.0 + 15.0 * i as f32;
        let bar = egui::Rect::from_min_max(egui::pos2(x, rect.bottom() - h), egui::pos2(x + 20.0, rect.bottom() - 4.0));
        p.rect_filled(bar, 2.0, egui::Color32::from_rgba_unmultiplied(80, 160, 255, 140));
    }
    let pts: Vec<egui::Pos2> = (0..=200)
        .map(|i| {
            let t = i as f32 / 200.0;
            egui::pos2(rect.left() + t * rect.width(), rect.center().y - (t * freq * std::f32::consts::TAU).sin() * rect.height() * 0.4)
        })
        .collect();
    p.add(egui::Shape::line(pts, egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 180, 60))));
    p.circle_filled(rect.center(), 10.0, egui::Color32::from_rgba_unmultiplied(255, 60, 90, 200));
    p.text(rect.left_top() + egui::vec2(8.0, 6.0), egui::Align2::LEFT_TOP, format!("sin({freq:.1}·2πt)"), egui::FontId::monospace(14.0), ui.visuals().text_color());
}

fn main() -> eframe::Result {
    eframe::aether::init_logger();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([640.0, 480.0]).with_title("Hello"),
        ..Default::default()
    };
    eframe::run_native(
        "hello",
        options,
        Box::new(|cc| {
            let mut app = Hello::default();
            if let Some((name, clicks)) = cc.storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)) {
                (app.name, app.clicks) = (name, clicks);
            }
            Ok(Box::new(app))
        }),
    )
}
