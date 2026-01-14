use crate::app::MainLoopAction;
use crate::app_state::AppState;
use crate::config::{OrganLibrary, OrganProfile, load_organ_library, save_organ_library};
use eframe::egui;
use rust_i18n::t;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[allow(dead_code)]
pub struct OrganManagerUi {
    pub visible: bool,
    library: OrganLibrary,
    // Temporary fields for adding a new organ
    new_organ_path: Option<PathBuf>,

    // For MIDI learning
    learning_index: Option<usize>,
}

impl OrganManagerUi {
    pub fn new() -> Self {
        let library = load_organ_library().unwrap_or_default();
        Self {
            visible: false,
            library,
            new_organ_path: None,
            learning_index: None,
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        exit_action: &Arc<Mutex<MainLoopAction>>,
        _app_state: Arc<Mutex<AppState>>,
    ) {
        let mut open = self.visible;

        egui::Window::new(t!("organ_manager.title"))
            .open(&mut open)
            .resize(|r| r.fixed_size([600.0, 400.0]))
            .show(ctx, |ui| {
                ui.label(t!("organ_manager.description"));
                ui.add_space(10.0);

                // --- List of Organs ---
                let mut remove_index = None;
                let mut dirty = false;

                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        egui::Grid::new("organ_list_grid")
                            .striped(true)
                            .num_columns(3)
                            .show(ui, |ui| {
                                for (i, organ) in self.library.organs.iter_mut().enumerate() {
                                    ui.label(&organ.name);
                                    ui.label(
                                        organ
                                            .path
                                            .file_name()
                                            .unwrap_or_default()
                                            .to_string_lossy(),
                                    );

                                    ui.horizontal(|ui| {
                                        // Load/Switch Button
                                        if ui.button(t!("organ_manager.load")).clicked() {
                                            // Trigger Switch
                                            *exit_action.lock().unwrap() =
                                                MainLoopAction::ReloadOrgan {
                                                    file: organ.path.clone(),
                                                };
                                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                        }

                                        // Remove Button
                                        if ui.button("❌").clicked() {
                                            remove_index = Some(i);
                                        }
                                    });
                                    ui.end_row();

                                    // Row 2: MIDI SysEx info
                                    ui.horizontal(|ui| {
                                        let sysex_str = organ
                                            .sysex_id
                                            .as_ref()
                                            .map(|bytes| {
                                                let hex: Vec<String> = bytes
                                                    .iter()
                                                    .map(|b| format!("{:02X}", b))
                                                    .collect();
                                                hex.join(" ")
                                            })
                                            .unwrap_or_else(|| "-".to_string());

                                        ui.label(format!("SysEx: {}", sysex_str));

                                        if let Some(learn_idx) = self.learning_index {
                                            if learn_idx == i {
                                                ui.add(
                                                    egui::Button::new(t!("midi_learn.status_listening"))
                                                        .fill(egui::Color32::ORANGE),
                                                );
                                            } else {
                                                ui.add_enabled(false, egui::Button::new(t!("midi_learn.btn_learn")));
                                            }
                                        } else {
                                            if ui.button(t!("midi_learn.btn_learn")).clicked() {
                                                self.learning_index = Some(i);
                                            }
                                        }

                                        if organ.sysex_id.is_some() {
                                            if ui.button(t!("midi_learn.btn_clear")).clicked() {
                                                organ.sysex_id = None;
                                                dirty = true;
                                            }
                                        }
                                    });
                                    ui.end_row();
                                }
                            });
                    });

                if let Some(i) = remove_index {
                    self.library.organs.remove(i);
                    dirty = true;
                }

                if dirty {
                    if let Err(e) = save_organ_library(&self.library) {
                        log::error!("Failed to save organ library: {}", e);
                    }
                }

                ui.separator();

                // --- Add New Organ ---
                if ui.button(t!("organ_manager.add_organ")).clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter(
                            "Organ Definitions",
                            &["organ", "json", "xml", "Organ_Hauptwerk_xml"],
                        )
                        .pick_file()
                    {
                        // Guess a name
                        let name = path
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| "Unknown Organ".to_string());

                        self.library.organs.push(OrganProfile {
                            name,
                            path,
                            sysex_id: None,
                        });
                        if let Err(e) = save_organ_library(&self.library) {
                            log::error!("Failed to save organ library: {}", e);
                        }
                    }
                }
            });

        self.visible = open;
    }

    pub fn is_learning(&self) -> bool {
        self.learning_index.is_some()
    }

    pub fn handle_learning(&mut self, sysex: Vec<u8>) {
        if let Some(idx) = self.learning_index {
            if let Some(organ) = self.library.organs.get_mut(idx) {
                organ.sysex_id = Some(sysex);
                let _ = save_organ_library(&self.library);
            }
            self.learning_index = None;
        }
    }

    pub fn find_organ_by_sysex(&self, sysex: &[u8]) -> Option<PathBuf> {
        self.library
            .organs
            .iter()
            .find(|o| o.sysex_id.as_ref().map_or(false, |id| id == sysex))
            .map(|o| o.path.clone())
    }
}
