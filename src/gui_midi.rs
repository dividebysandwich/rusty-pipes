use eframe::egui;
use rust_i18n::t;

use crate::config::{MidiDeviceConfig, MidiMappingMode};

/// Manages the state and visibility of the MIDI channel mapping configuration window.
pub struct MidiMappingWindow {
    /// Whether the window is currently open.
    pub visible: bool,
    /// The index of the device in the `AppSettings::midi_devices` vector being edited.
    pub device_index: usize,
}

impl MidiMappingWindow {
    pub fn new() -> Self {
        Self {
            visible: false,
            device_index: 0,
        }
    }

    /// Renders the mapping window if it is visible.
    pub fn show(&mut self, ctx: &egui::Context, devices: &mut Vec<MidiDeviceConfig>) {
        if !self.visible {
            return;
        }

        // Safety check: if the device list changed and index is invalid, close window.
        if self.device_index >= devices.len() {
            self.visible = false;
            return;
        }

        let device = &mut devices[self.device_index];

        // We use a fixed sized window that can be resized by the user
        let window_title = t!("midi_config.window_title_fmt", name = device.name);

        let mut is_open = self.visible;
        
        let mut close_requested = false;

        egui::Window::new(window_title.to_string())
            .open(&mut is_open) // Borrow local var, not self.visible
            .resizable(true)
            .collapsible(false)
            .default_width(400.0)
            .default_height(500.0)
            .show(ctx, |ui| {
                ui.heading(t!("midi_config.heading"));
                ui.label(t!("midi_config.description"));
                ui.add_space(10.0);

                // --- Mode Selection ---
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(t!("midi_config.mode_label")).strong());
                    ui.radio_value(&mut device.mapping_mode, MidiMappingMode::Simple, t!("midi_config.mode_simple"));
                    ui.radio_value(&mut device.mapping_mode, MidiMappingMode::Complex, t!("midi_config.mode_complex"));
                });
                
                ui.separator();
                ui.add_space(5.0);

                // --- Mapping Content ---
                // We call these as associated functions (Self::) or just make them static.
                match device.mapping_mode {
                    MidiMappingMode::Simple => {
                        Self::render_simple_mode(ui, device);
                    }
                    MidiMappingMode::Complex => {
                        Self::render_complex_mode(ui, device);
                    }
                }

                ui.add_space(15.0);
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button(t!("midi_config.btn_done")).clicked() {
                        close_requested = true;
                    }
                });
            });

        // 3. Sync state back to self.visible
        // Window closes if X is clicked (is_open becomes false) OR Done is clicked.
        self.visible = is_open && !close_requested;
    }

    fn render_simple_mode(ui: &mut egui::Ui, device: &mut MidiDeviceConfig) {
        ui.vertical_centered(|ui| {
            ui.add_space(10.0);
            ui.label(t!("midi_config.simple_desc_1"));
            ui.label(t!("midi_config.simple_desc_2"));
            ui.add_space(10.0);

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(t!("midi_config.target_channel_label")).strong());
                
                // ComboBox for 1-16 (stored as 0-15)
                let current_text = t!("midi_config.channel_fmt", num = device.simple_target_channel + 1);
                egui::ComboBox::from_id_salt("simple_target_selector")
                    .selected_text(current_text)
                    .show_ui(ui, |ui| {
                        for ch in 0..16 {
                            ui.selectable_value(
                                &mut device.simple_target_channel, 
                                ch, 
                                t!("midi_config.channel_fmt", num = ch + 1).to_string()
                            );
                        }
                    });
            });
            ui.add_space(10.0);
            ui.label(egui::RichText::new("Use this for single-keyboard controllers.").italics().weak());
        });
    }

    fn render_complex_mode(ui: &mut egui::Ui, device: &mut MidiDeviceConfig) {
        ui.label(t!("midi_config.complex_desc"));
        ui.add_space(5.0);

        egui::ScrollArea::vertical()
            .max_height(350.0)
            .show(ui, |ui| {
                egui::Grid::new("complex_map_grid")
                    .striped(true)
                    .min_col_width(100.0)
                    .spacing([40.0, 10.0])
                    .show(ui, |ui| {
                        // Header
                        ui.label(egui::RichText::new(t!("midi_config.col_input")).strong().underline());
                        ui.label(egui::RichText::new(t!("midi_config.col_target")).strong().underline());
                        ui.end_row();

                        // Rows 1-16
                        for i in 0..16 {
                            ui.label(t!("midi_config.input_channel_fmt", num = i + 1));
                            
                            let unique_id = format!("complex_ch_combo_{}", i);
                            egui::ComboBox::from_id_salt(unique_id)
                                .selected_text(format!("{}", device.complex_mapping[i] + 1))
                                .show_ui(ui, |ui| {
                                    for target in 0..16 {
                                        ui.selectable_value(
                                            &mut device.complex_mapping[i], 
                                            target, 
                                            format!("{}", target + 1)
                                        );
                                    }
                                });
                            ui.end_row();
                        }
                    });
            });
    }
}