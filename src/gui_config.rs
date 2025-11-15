use anyhow::Result;
use eframe::{egui, App, Frame};
use std::sync::{Arc, Mutex};
use midir::MidiInput;
use crate::config::{AppSettings, ConfigState, RuntimeConfig};
use crate::gui_filepicker;
use crate::app::{PIPES, LOGO};

#[allow(dead_code)]
struct ConfigApp {
    state: ConfigState,
    midi_input_arc: Arc<Mutex<Option<MidiInput>>>,
    // This holds the final config to be returned to main
    output: Arc<Mutex<Option<RuntimeConfig>>>,
    // This signals the main thread that we are done
    is_finished: Arc<Mutex<bool>>,
    // MIDI port selection needs to be stored as an index for the ComboBox
    selected_midi_port_index: Option<usize>,
}

impl ConfigApp {
fn new(
        settings: AppSettings,
        midi_input_arc: Arc<Mutex<Option<MidiInput>>>,
        output: Arc<Mutex<Option<RuntimeConfig>>>,
        is_finished: Arc<Mutex<bool>>,
    ) -> Self {
        Self {
            state: ConfigState::new(settings, &midi_input_arc).expect("Failed to create ConfigState"),
            midi_input_arc,
            output,
            is_finished,
            selected_midi_port_index: None,
        }
    }
}

impl App for ConfigApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                
                ui.vertical_centered(|ui| {
                    let font_size = 10.0;
                    let mono_font = egui::FontId::monospace(font_size);
                    let orange = egui::Color32::from_rgb(255, 165, 0);

                    ui.label(
                        egui::RichText::new(PIPES)
                            .font(mono_font.clone())
                            .color(egui::Color32::GRAY),
                    );
                    ui.label(
                        egui::RichText::new(LOGO)
                            .font(mono_font.clone())
                            .color(orange),
                    );
                    ui.label(
                        egui::RichText::new("Indicia MMXXV")
                            .font(mono_font)
                            .color(orange),
                    );
                });
                ui.add_space(10.0);

                ui.heading("Rusty Pipes Configuration");
                ui.separator();

                // --- File Paths ---
                ui.vertical_centered_justified(|ui| {
                    ui.horizontal(|ui| {
                        let organ_text = path_to_str(self.state.settings.organ_file.as_deref());
                        ui.label(format!("Organ File: {}", organ_text));
                        if ui.button("Browse...").clicked() {
                            if let Ok(Some(path)) = gui_filepicker::pick_file(
                                "Select Organ File",
                                &[("Organ Files", &["organ", "Organ_Hauptwerk_xml"])]
                            ) {
                                self.state.settings.organ_file = Some(path);
                            }
                        }
                    });

                    ui.horizontal(|ui| {
                        let midi_text = path_to_str(self.state.midi_file.as_deref());
                        ui.label(format!("MIDI File (Play): {}", midi_text));
                        if ui.button("Browse...").clicked() {
                             if let Ok(Some(path)) = gui_filepicker::pick_file(
                                "Select MIDI File (Optional)",
                                &[("MIDI Files", &["mid", "midi"])]
                            ) {
                                self.state.midi_file = Some(path);
                            }
                        }
                        if ui.button("Clear").clicked() {
                            self.state.midi_file = None;
                        }
                    });

                    ui.horizontal(|ui| {
                        let ir_text = path_to_str(self.state.settings.ir_file.as_deref());
                        ui.label(format!("IR File: {}", ir_text));
                        if ui.button("Browse...").clicked() {
                             if let Ok(Some(path)) = gui_filepicker::pick_file(
                                "Select IR File (Optional)",
                                &[("Audio Files", &["wav", "flac"])]
                            ) {
                                self.state.settings.ir_file = Some(path);
                            }
                        }
                         if ui.button("Clear").clicked() {
                            self.state.settings.ir_file = None;
                        }
                    });
                });
                
                ui.separator();

                // --- MIDI Device ---
                ui.horizontal(|ui| {
                    ui.label("MIDI Device:");
                    let selected_text = self.selected_midi_port_index
                        .and_then(|idx| self.state.available_ports.get(idx))
                        .map_or("None", |(_, name)| name.as_str());
                    
                    egui::ComboBox::from_id_salt("midi_device_combo")
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            // Option for None
                            if ui.selectable_label(self.selected_midi_port_index.is_none(), "None").clicked() {
                                self.selected_midi_port_index = None;
                            }
                            // Options for each port
                            for (i, (_, name)) in self.state.available_ports.iter().enumerate() {
                                if ui.selectable_label(self.selected_midi_port_index == Some(i), name).clicked() {
                                    self.selected_midi_port_index = Some(i);
                                }
                            }
                        });
                });

                ui.separator();

                // --- Other Settings ---
                ui.group(|ui| {
                    ui.add(egui::Slider::new(&mut self.state.settings.reverb_mix, 0.0..=1.0).text("Reverb Mix"));
                    ui.add(egui::DragValue::new(&mut self.state.settings.audio_buffer_frames).speed(1.0).range(32..=4096).prefix("Audio Buffer: ").suffix(" frames"));
                    ui.checkbox(&mut self.state.settings.precache, "Pre-cache Samples");
                    ui.checkbox(&mut self.state.settings.convert_to_16bit, "Convert to 16-bit");
                    ui.checkbox(&mut self.state.settings.original_tuning, "Use Original Tuning");
                });

                ui.separator();

                // --- Error Message ---
                if let Some(err) = &self.state.error_msg {
                    ui.label(egui::RichText::new(err).color(egui::Color32::RED));
                }

                // --- Start / Quit ---
                ui.horizontal(|ui| {
                    if ui.button("Quit").clicked() {
                        *self.is_finished.lock().unwrap() = true; // Signal main to exit
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    
                    let button_text = egui::RichText::new("Start Rusty Pipes")
                        .color(egui::Color32::GREEN)
                        .text_style(egui::TextStyle::Heading);
                        
                    // 2. Create the button using the RichText
                    let start_button = ui.add_enabled(
                        self.state.settings.organ_file.is_some(),
                        egui::Button::new(button_text) 
                    );
                    
                    if start_button.clicked() {
                        // --- Finalize Config and Exit ---
                        let (port, name) = self.selected_midi_port_index
                            .and_then(|idx| self.state.available_ports.get(idx))
                            .map_or((None, None), |(p, n)| (Some(p.clone()), Some(n.clone())));

                        let runtime_config = RuntimeConfig {
                            organ_file: self.state.settings.organ_file.clone().unwrap(), // Safe due to button enable
                            ir_file: self.state.settings.ir_file.clone(),
                            reverb_mix: self.state.settings.reverb_mix,
                            audio_buffer_frames: self.state.settings.audio_buffer_frames,
                            precache: self.state.settings.precache,
                            convert_to_16bit: self.state.settings.convert_to_16bit,
                            original_tuning: self.state.settings.original_tuning,
                            midi_file: self.state.midi_file.clone(),
                            midi_port: port,
                            midi_port_name: name,
                        };
                        
                        *self.output.lock().unwrap() = Some(runtime_config);
                        *self.is_finished.lock().unwrap() = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }

                    if self.state.settings.organ_file.is_none() {
                        ui.label(egui::RichText::new("Please select an Organ File to start.").color(egui::Color32::YELLOW));
                    }
                });
            }); // --- END SCROLL AREA ---
        });
    }
}

fn path_to_str(path: Option<&std::path::Path>) -> String {
    path.and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map_or("None".to_string(), |s| s.to_string())
}

/// Runs the GUI configuration loop.
pub fn run_config_ui(
    settings: AppSettings,
    midi_input_arc: Arc<Mutex<Option<MidiInput>>>,
) -> Result<Option<RuntimeConfig>> {
    let output = Arc::new(Mutex::new(None));
    let is_finished = Arc::new(Mutex::new(false));

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([600.0, 650.0]) // <-- INCREASED HEIGHT
            .with_resizable(true),         // <-- MADE RESIZABLE
        ..Default::default()
    };

    let app = ConfigApp::new(
        settings, 
        Arc::clone(&midi_input_arc),
        Arc::clone(&output), 
        Arc::clone(&is_finished)
    );

    eframe::run_native(
        "Rusty Pipes Configuration",
        native_options,
        Box::new(|_cc| Ok(Box::new(app))),
    )
    .map_err(|e| anyhow::anyhow!("Eframe error: {}", e))?;

    // After the loop exits, check the output Arc
    let final_config = output.lock().unwrap().clone();
    Ok(final_config)
}