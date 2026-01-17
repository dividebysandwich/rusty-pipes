use crate::app::MainLoopAction;
use crate::app_state::AppState;
use crate::config::{OrganLibrary, OrganProfile, load_organ_library, save_organ_library, MidiEventSpec};
use eframe::egui;
use rust_i18n::t;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[allow(dead_code)]
pub struct OrganManagerUi {
    pub visible: bool,
    library: OrganLibrary,
    // Temporary fields for adding a new organ
    new_organ_path: Option<PathBuf>,

    // For MIDI learning
    learning_index: Option<usize>,
    last_learn_interaction: Instant,
}

impl OrganManagerUi {
    pub fn new() -> Self {
        let library = load_organ_library().unwrap_or_default();
        Self {
            visible: false,
            library,
            new_organ_path: None,
            learning_index: None,
            last_learn_interaction: Instant::now(),
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        exit_action: &Arc<Mutex<MainLoopAction>>,
        app_state: Arc<Mutex<AppState>>,
    ) {
        let mut open = self.visible;

        // Check if we learned something recently
        if self.learning_index.is_some() {
            let state = app_state.lock().unwrap();
            if let Some((event, time)) = &state.last_midi_event_received {
                if *time > self.last_learn_interaction {
                    // We found a trigger!
                    // We can't mutate self inside the lock, so we clone the event and drop lock
                    let event_clone = event.clone();
                    drop(state);
                    self.handle_learning(event_clone);
                }
            }
        }

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

                                    // Row 2: MIDI/SysEx Trigger info
                                    ui.horizontal(|ui| {
                                        let trigger_str = organ
                                            .activation_trigger
                                            .as_ref()
                                            .map(|t| t.to_string())
                                            .unwrap_or_else(|| "-".to_string());

                                        ui.label(format!("Trigger: {}", trigger_str));

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
                                                self.last_learn_interaction = Instant::now();
                                            }
                                        }

                                        if organ.activation_trigger.is_some() {
                                            if ui.button(t!("midi_learn.btn_clear")).clicked() {
                                                organ.activation_trigger = None;
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
                            activation_trigger: None,
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

    pub fn handle_learning(&mut self, event: MidiEventSpec) {
        if let Some(idx) = self.learning_index {
            if let Some(organ) = self.library.organs.get_mut(idx) {
                organ.activation_trigger = Some(event);
                let _ = save_organ_library(&self.library);
            }
            self.learning_index = None;
        }
    }

    pub fn find_organ_by_trigger(&self, event: &MidiEventSpec) -> Option<PathBuf> {
        self.library
            .organs
            .iter()
            .find(|o| o.activation_trigger.as_ref().map_or(false, |trig| trig == event))
            .map(|o| o.path.clone())
    }
}