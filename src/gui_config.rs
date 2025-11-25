use anyhow::Result;
use eframe::{egui, App, Frame};
use std::sync::{Arc, Mutex};
use midir::MidiInput; 
use crate::config::{AppSettings, ConfigState, RuntimeConfig};
use crate::gui_filepicker;
use crate::app::{PIPES, LOGO};
use crate::audio::get_supported_sample_rates;

#[allow(dead_code)]
struct ConfigApp {
    state: ConfigState,
    midi_input_arc: Arc<Mutex<Option<MidiInput>>>, 
    output: Arc<Mutex<Option<RuntimeConfig>>>, 
    is_finished: Arc<Mutex<bool>>,
    selected_midi_port_index: Option<usize>,
    selected_audio_device_index: Option<usize>,
}

impl ConfigApp {
    fn new(
        settings: AppSettings,
        midi_input_arc: Arc<Mutex<Option<MidiInput>>>, 
        output: Arc<Mutex<Option<RuntimeConfig>>>, 
        is_finished: Arc<Mutex<bool>>,
    ) -> Self {
        let state = ConfigState::new(settings, &midi_input_arc).expect("Failed to create ConfigState");

        // Find the index of the pre-selected port, if any
        let selected_midi_port_index = state.selected_midi_port.as_ref()
            .and_then(|(selected_port, _)| {
                state.available_ports.iter().position(|(port, _)| port == selected_port)
            });
        
        // Find the index of the pre-selected audio device, if any
        let selected_audio_device_index = state.selected_audio_device_name.as_ref()
            .and_then(|selected_name| {
                state.available_audio_devices.iter().position(|name| name == selected_name)
            });

        Self {
            state,
            midi_input_arc,
            output,
            is_finished,
            selected_midi_port_index,
            selected_audio_device_index,
        }
    }

    // Helper to refresh rates when device changes
    fn refresh_sample_rates(&mut self) {
         let device_name = self.selected_audio_device_index
            .and_then(|idx| self.state.available_audio_devices.get(idx))
            .cloned();

        if let Ok(rates) = get_supported_sample_rates(device_name) {
            self.state.available_sample_rates = rates;
            // Ensure selected rate is valid, else reset to first available
            if !self.state.available_sample_rates.contains(&self.state.settings.sample_rate) {
                if let Some(&first) = self.state.available_sample_rates.first() {
                     self.state.settings.sample_rate = first;
                }
            }
        }
    }
}

impl App for ConfigApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                
                // --- LOGO ---
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
                ui.add_space(10.0);

                egui::Grid::new("config_grid")
                    .num_columns(2)
                    .spacing([40.0, 15.0]) // [col_spacing, row_spacing]
                    .min_col_width(180.0)  // Minimum width for labels
                    .show(ui, |ui| {
                        
                        // --- Organ File ---
                        ui.label("Organ File:");
                        ui.horizontal(|ui| {
                            let organ_text = path_to_str_truncated(self.state.settings.organ_file.as_deref());
                            let full_path = path_to_str_full(self.state.settings.organ_file.as_deref());
                            ui.label(organ_text).on_hover_text(full_path);
                            
                            if ui.button("Browse...").clicked() {
                                if let Ok(Some(path)) = gui_filepicker::pick_file(
                                    "Select Organ File",
                                    &[("Organ Files", &["organ", "Organ_Hauptwerk_xml"])]
                                ) {
                                    self.state.settings.organ_file = Some(path);
                                }
                            }
                        });
                        ui.end_row();

                        // --- Audio Device ---
                        ui.label("Audio Device:");
                        let selected_audio_text = self.selected_audio_device_index
                            .and_then(|idx| self.state.available_audio_devices.get(idx))
                            .map_or("Default", |name| name.as_str());
                    
                        ui.set_min_width(300.0); 
                        egui::ComboBox::from_id_salt("audio_device_combo")
                            .selected_text(selected_audio_text)
                            .show_ui(ui, |ui| {
                                // Track the user's intended action
                                let mut selected_default = false;
                                let mut selected_index = None;

                                // Check "Default" option
                                if ui.selectable_label(self.selected_audio_device_index.is_none(), "[ Default ]").clicked() {
                                    selected_default = true;
                                }

                                // Iterate list (Immutable borrow of self happens here)
                                for (i, name) in self.state.available_audio_devices.iter().enumerate() {
                                    if ui.selectable_label(self.selected_audio_device_index == Some(i), name).clicked() {
                                        selected_index = Some(i);
                                    }
                                }

                                // Apply changes (Immutable borrow is dropped, so we can now mutate self)
                                if selected_default {
                                    self.selected_audio_device_index = None;
                                    self.refresh_sample_rates();
                                } else if let Some(i) = selected_index {
                                    self.selected_audio_device_index = Some(i);
                                    self.refresh_sample_rates();
                                }
                            });
                        ui.end_row();

                        // --- Sample Rate ---
                        ui.label("Sample Rate:");
                        egui::ComboBox::from_id_salt("sample_rate_combo")
                            .selected_text(format!("{} Hz", self.state.settings.sample_rate))
                            .show_ui(ui, |ui| {
                                for &rate in &self.state.available_sample_rates {
                                    if ui.selectable_label(self.state.settings.sample_rate == rate, format!("{}", rate)).clicked() {
                                        self.state.settings.sample_rate = rate;
                                    }
                                }
                            });
                        ui.end_row();

                        // --- MIDI Device ---
                        ui.label("MIDI Device:");
                        let selected_text = self.selected_midi_port_index
                            .and_then(|idx| self.state.available_ports.get(idx))
                            .map_or("None", |(_, name)| name.as_str());
                        
                        // Give the ComboBox a minimum width
                        ui.set_min_width(300.0); 
                        egui::ComboBox::from_id_salt("midi_device_combo")
                            .selected_text(selected_text)
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(self.selected_midi_port_index.is_none(), "None").clicked() {
                                    self.selected_midi_port_index = None;
                                }
                                for (i, (_, name)) in self.state.available_ports.iter().enumerate() {
                                    if ui.selectable_label(self.selected_midi_port_index == Some(i), name).clicked() {
                                        self.selected_midi_port_index = Some(i);
                                    }
                                }
                            });
                        ui.end_row();

                        // --- MIDI File ---
                        ui.label("MIDI File (Play):");
                        ui.horizontal(|ui| {
                             let midi_text = path_to_str_truncated(self.state.midi_file.as_deref());
                             let full_path = path_to_str_full(self.state.midi_file.as_deref());
                             ui.label(midi_text).on_hover_text(full_path);

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
                        ui.end_row();

                        // --- IR File ---
                        ui.label("Reverb Impulse Response WAV:");
                        ui.horizontal(|ui| {
                            let ir_text = path_to_str_truncated(self.state.settings.ir_file.as_deref());
                            let full_path = path_to_str_full(self.state.settings.ir_file.as_deref());
                            ui.label(ir_text).on_hover_text(full_path);

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
                        ui.end_row();

                        // --- Reverb Mix ---
                        ui.label("Reverb Mix:");
                        // Make slider fill available width
                        ui.add(egui::Slider::new(&mut self.state.settings.reverb_mix, 0.0..=1.0)
                            .show_value(true)
                            .min_decimals(2)
                            .text(""));
                        ui.end_row();

                        // --- Gain ---
                        ui.label("Gain:");
                        // Make slider fill available width
                        ui.add(egui::Slider::new(&mut self.state.settings.gain, 0.0..=1.0)
                            .show_value(true)
                            .min_decimals(2)
                            .text(""));
                        ui.end_row();

                        // --- Polyphony ---
                        ui.label("Polyphony:");
                        // Make slider fill available width
                        ui.add(egui::Slider::new(&mut self.state.settings.polyphony, 1..=1024)
                            .show_value(true)
                            .min_decimals(0)
                            .text(""));
                        ui.end_row();

                        // --- Audio Buffer ---
                        ui.label("Audio Buffer (frames):");
                        ui.add(egui::DragValue::new(&mut self.state.settings.audio_buffer_frames).speed(32.0).range(32..=4096));
                        ui.end_row();

                        // --- Boolean Options ---
                        ui.label("Options:");
                        ui.vertical(|ui| {
                            ui.checkbox(&mut self.state.settings.precache, "Pre-cache Samples");
                            ui.checkbox(&mut self.state.settings.convert_to_16bit, "Convert to 16-bit");
                            ui.checkbox(&mut self.state.settings.original_tuning, "Use Original Tuning");
                        });
                        ui.end_row();
                    });
                // --- END GRID LAYOUT ---

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);

                // --- Error Message ---
                if let Some(err) = &self.state.error_msg {
                    ui.label(egui::RichText::new(err).color(egui::Color32::RED));
                }

                // --- Start / Quit ---
                ui.horizontal(|ui| {

                    let quit_button_text = egui::RichText::new("Quit")
                        .color(egui::Color32::RED)
                        .text_style(egui::TextStyle::Heading); 
                        
                    let quit_button = ui.add_enabled(
                        self.state.settings.organ_file.is_some(),
                        egui::Button::new(quit_button_text) 
                    );

                    if quit_button.clicked() {
                        *self.is_finished.lock().unwrap() = true; 
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }

                    let start_button_text = egui::RichText::new("Start Rusty Pipes")
                        .color(egui::Color32::GREEN)
                        .text_style(egui::TextStyle::Heading); 
                        
                    let start_button = ui.add_enabled(
                        self.state.settings.organ_file.is_some(),
                        egui::Button::new(start_button_text) 
                    );
                    
                    if start_button.clicked() {
                        let (port, name) = self.selected_midi_port_index
                            .and_then(|idx| self.state.available_ports.get(idx))
                            .map_or((None, None), |(p, n)| (Some(p.clone()), Some(n.clone())));

                        let audio_device_name = self.selected_audio_device_index
                            .and_then(|idx| self.state.available_audio_devices.get(idx))
                            .cloned();

                        let runtime_config = RuntimeConfig {
                            organ_file: self.state.settings.organ_file.clone().unwrap(),
                            ir_file: self.state.settings.ir_file.clone(),
                            reverb_mix: self.state.settings.reverb_mix,
                            audio_buffer_frames: self.state.settings.audio_buffer_frames,
                            precache: self.state.settings.precache,
                            convert_to_16bit: self.state.settings.convert_to_16bit,
                            original_tuning: self.state.settings.original_tuning,
                            midi_file: self.state.midi_file.clone(),
                            midi_port: port,
                            midi_port_name: name,
                            gain: self.state.settings.gain,
                            polyphony: self.state.settings.polyphony,
                            audio_device_name,
                            sample_rate: self.state.settings.sample_rate,
                        };
                        
                        *self.output.lock().unwrap() = Some(runtime_config); 
                        *self.is_finished.lock().unwrap() = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }

                    if self.state.settings.organ_file.is_none() {
                        ui.label(egui::RichText::new("Please select an Organ File.").color(egui::Color32::YELLOW));
                    }
                });
            }); 
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        *self.is_finished.lock().unwrap() = true;
    }
}

/// Helper to get the full path as a string
fn path_to_str_full(path: Option<&std::path::Path>) -> String {
    path.map_or_else(
        || "None".to_string(), 
        |p| p.display().to_string()
    )
}

/// Helper to get just the filename as a string
fn path_to_str_truncated(path: Option<&std::path::Path>) -> String {
    path.and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map_or("None".to_string(), |s| s.to_string())
}

/// Runs the GUI configuration loop.
pub fn run_config_ui(
    settings: AppSettings,
    midi_input_arc: Arc<Mutex<Option<MidiInput>>>
) -> Result<Option<RuntimeConfig>> {
    let output = Arc::new(Mutex::new(None));
    let is_finished = Arc::new(Mutex::new(false));

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([700.0, 700.0])
            .with_resizable(true),         
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

    let final_config = output.lock().unwrap().clone();
    Ok(final_config)
}