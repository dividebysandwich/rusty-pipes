use eframe::{egui, App, Frame};
use rust_i18n::t;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};

struct LoadingApp {
    progress_rx: mpsc::Receiver<(f32, String)>,
    is_finished: Arc<AtomicBool>,
    progress: f32,
    status_text: String,
}

impl App for LoadingApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut Frame) {
        // Check for progress updates
        while let Ok((progress, status)) = self.progress_rx.try_recv() {
            self.progress = progress;
            self.status_text = status;
        }

        // Check if the loading thread is finished
        if self.is_finished.load(Ordering::SeqCst) {
            // Signal eframe to close this window
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(&self.status_text)
                        .heading()
                        .color(egui::Color32::WHITE),
                );
                ui.add_space(10.0);

                // The progress bar
                let progress_bar = egui::ProgressBar::new(self.progress)
                    .show_percentage()
                    .animate(true);
                ui.add(progress_bar);
                ui.add_space(20.0);
            });
        });

        // Tell eframe to keep redrawing this window to poll for updates
        ctx.request_repaint();
    }
}

/// Runs the dedicated eframe loading window.
/// This will block the thread it's called on (the main thread)
/// until the `is_finished` atomic is set to true.
pub fn run_loading_ui(
    progress_rx: mpsc::Receiver<(f32, String)>,
    is_finished: Arc<AtomicBool>,
) -> Result<(), eframe::Error> {
    let win_title = t!("loading.window_title").to_string();
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 120.0])
            .with_resizable(false)
            .with_decorations(true) // Re-enable for the loading window
            .with_title(&win_title),
        ..Default::default()
    };

    let app = LoadingApp {
        progress_rx,
        is_finished,
        progress: 0.0,
        status_text: t!("loading.status_init").to_string(),
    };

    let app_name = t!("loading.app_name").to_string();

    eframe::run_native(&app_name, native_options, Box::new(|_cc| Ok(Box::new(app))))
}
