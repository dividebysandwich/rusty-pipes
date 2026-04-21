use crate::app::{LOGO, PIPES};
use crate::audio::get_supported_sample_rates;
use crate::config::{AppSettings, ConfigState, RuntimeConfig};
use crate::gui_filepicker;
use crate::gui_midi::MidiMappingWindow;
use anyhow::Result;
use eframe::{App, Frame, egui};
use midir::MidiInput;
use rust_i18n::t;
use std::sync::{Arc, Mutex};

#[allow(dead_code)]
struct ConfigApp {
    state: ConfigState,
    midi_input_arc: Arc<Mutex<Option<MidiInput>>>,
    output: Arc<Mutex<Option<RuntimeConfig>>>,
    is_finished: Arc<Mutex<bool>>,
    selected_audio_device_index: Option<usize>,
    selected_ir_index: Option<usize>,
    midi_mapping_window: MidiMappingWindow,
    show_lcd_config: bool,
}

impl ConfigApp {
    fn new(
        settings: AppSettings,
        midi_input_arc: Arc<Mutex<Option<MidiInput>>>,
        output: Arc<Mutex<Option<RuntimeConfig>>>,
        is_finished: Arc<Mutex<bool>>,
    ) -> Self {
        let state =
            ConfigState::new(settings, &midi_input_arc).expect("Failed to create ConfigState");

        // Find the index of the pre-selected audio device, if any
        let selected_audio_device_index =
            state
                .selected_audio_device_name
                .as_ref()
                .and_then(|selected_name| {
                    state
                        .available_audio_devices
                        .iter()
                        .position(|name| name == selected_name)
                });

        // Find pre-selected IR index
        let selected_ir_index = state
            .settings
            .ir_file
            .as_ref()
            .and_then(|path| state.available_ir_files.iter().position(|(_, p)| p == path));

        Self {
            state,
            midi_input_arc,
            output,
            is_finished,
            selected_audio_device_index,
            selected_ir_index,
            midi_mapping_window: MidiMappingWindow::new(),
            show_lcd_config: false,
        }
    }

    // Helper to refresh rates when device changes
    fn refresh_sample_rates(&mut self) {
        let device_name = self
            .selected_audio_device_index
            .and_then(|idx| self.state.available_audio_devices.get(idx))
            .cloned();

        if let Ok(rates) = get_supported_sample_rates(device_name) {
            self.state.available_sample_rates = rates;
            // Ensure selected rate is valid, else reset to first available
            if !self
                .state
                .available_sample_rates
                .contains(&self.state.settings.sample_rate)
            {
                if let Some(&first) = self.state.available_sample_rates.first() {
                    self.state.settings.sample_rate = first;
                }
            }
        }
    }

    fn draw_lcd_config_modal(&mut self, ctx: &egui::Context) {
        let mut open = self.show_lcd_config;
        egui::Window::new(t!("config.lcd_title"))
            .open(&mut open)
            .show(ctx, |ui| {
                let mut to_remove = None;

                if ui.button(t!("config.lcd_add")).clicked() {
                    let next_id = self.state.settings.lcd_displays.len() as u8 + 1;
                    use crate::config::{LcdColor, LcdDisplayConfig, LcdLineType};
                    self.state.settings.lcd_displays.push(LcdDisplayConfig {
                        id: next_id,
                        line1: LcdLineType::OrganName,
                        line2: LcdLineType::SystemStatus,
                        background_color: LcdColor::White,
                    });
                }

                ui.separator();

                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .show(ui, |ui| {
                        for (i, display) in self.state.settings.lcd_displays.iter_mut().enumerate()
                        {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(t!("config.lcd_display_label", num = i + 1));
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.button("🗑").clicked() {
                                                to_remove = Some(i);
                                            }
                                        },
                                    );
                                });

                                use crate::config::{LcdColor, LcdLineType};

                                egui::Grid::new(format!("lcd_grid_{}", i))
                                    .num_columns(2)
                                    .show(ui, |ui| {
                                        ui.label(t!("config.lcd_id_label"));
                                        ui.add(
                                            egui::DragValue::new(&mut display.id).range(1..=127),
                                        );
                                        ui.end_row();

                                        ui.label(t!("config.lcd_background_label"));
                                        egui::ComboBox::from_id_salt(format!("lcd_color_{}", i))
                                            .selected_text(format!(
                                                "{:?}",
                                                display.background_color
                                            ))
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(
                                                    &mut display.background_color,
                                                    LcdColor::Off,
                                                    "Off",
                                                );
                                                ui.selectable_value(
                                                    &mut display.background_color,
                                                    LcdColor::White,
                                                    "White",
                                                );
                                                ui.selectable_value(
                                                    &mut display.background_color,
                                                    LcdColor::Red,
                                                    "Red",
                                                );
                                                ui.selectable_value(
                                                    &mut display.background_color,
                                                    LcdColor::Green,
                                                    "Green",
                                                );
                                                ui.selectable_value(
                                                    &mut display.background_color,
                                                    LcdColor::Yellow,
                                                    "Yellow",
                                                );
                                                ui.selectable_value(
                                                    &mut display.background_color,
                                                    LcdColor::Blue,
                                                    "Blue",
                                                );
                                                ui.selectable_value(
                                                    &mut display.background_color,
                                                    LcdColor::Magenta,
                                                    "Magenta",
                                                );
                                                ui.selectable_value(
                                                    &mut display.background_color,
                                                    LcdColor::Cyan,
                                                    "Cyan",
                                                );
                                            });
                                        ui.end_row();

                                        let line_options = [
                                            (LcdLineType::Empty, "Empty"),
                                            (LcdLineType::OrganName, "Organ Name"),
                                            (LcdLineType::SystemStatus, "System Status"),
                                            (LcdLineType::LastPreset, "Last Preset"),
                                            (LcdLineType::LastStopChange, "Last Stop Change"),
                                            (LcdLineType::MidiLog, "MIDI Log"),
                                            (LcdLineType::Gain, "Gain"),
                                            (LcdLineType::ReverbMix, "Reverb Mix"),
                                            (LcdLineType::MidiPlayerStatus, "MIDI Player Status"),
                                        ];

                                        ui.label(t!("config.lcd_line1_label"));
                                        egui::ComboBox::from_id_salt(format!("lcd_line1_{}", i))
                                            .selected_text(format!("{}", display.line1))
                                            .show_ui(ui, |ui| {
                                                for (val, label) in &line_options {
                                                    ui.selectable_value(
                                                        &mut display.line1,
                                                        val.clone(),
                                                        *label,
                                                    );
                                                }
                                            });
                                        ui.end_row();

                                        ui.label(t!("config.lcd_line2_label"));
                                        egui::ComboBox::from_id_salt(format!("lcd_line2_{}", i))
                                            .selected_text(format!("{}", display.line2))
                                            .show_ui(ui, |ui| {
                                                for (val, label) in &line_options {
                                                    ui.selectable_value(
                                                        &mut display.line2,
                                                        val.clone(),
                                                        *label,
                                                    );
                                                }
                                            });
                                        ui.end_row();
                                    });
                            });
                            ui.add_space(5.0);
                        }
                    });

                if let Some(idx) = to_remove {
                    self.state.settings.lcd_displays.remove(idx);
                }
            });
        self.show_lcd_config = open;
    }
}

impl App for ConfigApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut Frame) {
        // Modal MIDI Mapping Window
        self.midi_mapping_window
            .show(ctx, &mut self.state.settings.midi_devices);

        // Modal LCD Configuration Window
        if self.show_lcd_config {
            self.draw_lcd_config_modal(ctx);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
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
                            egui::RichText::new(t!("config.subtitle"))
                                .font(mono_font)
                                .color(orange),
                        );
                    });
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), 16.0),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                                    .color(egui::Color32::GRAY),
                            );
                        },
                    );
                    ui.add_space(10.0);

                    ui.heading(t!("config.window_title"));
                    ui.separator();
                    ui.add_space(10.0);

                    egui::Grid::new("config_grid")
                        .num_columns(2)
                        .spacing([40.0, 15.0]) // [col_spacing, row_spacing]
                        .min_col_width(180.0) // Minimum width for labels
                        .show(ui, |ui| {
                            ui.style_mut().spacing.slider_width = 300.0;

                            // --- Organ File ---
                            ui.label(t!("config.group_organ_file"))
                                .on_hover_text(t!("config.tooltip_organ"));
                            ui.horizontal(|ui| {
                                let organ_text = path_to_str_truncated(
                                    self.state.settings.organ_file.as_deref(),
                                );
                                let full_path =
                                    path_to_str_full(self.state.settings.organ_file.as_deref());
                                ui.label(organ_text).on_hover_text(full_path);

                                if ui.button(t!("config.btn_browse")).clicked() {
                                    if let Ok(Some(path)) = gui_filepicker::pick_file(
                                        &t!("config.picker_organ"),
                                        &[(
                                            "Organ Files",
                                            &["organ", "orgue", "Organ_Hauptwerk_xml", "xml"],
                                        )],
                                    ) {
                                        self.state.settings.organ_file = Some(path);
                                    }
                                }
                            });
                            ui.end_row();

                            // --- Audio Device ---
                            ui.label(t!("config.group_audio_device"))
                                .on_hover_text(t!("config.tooltip_audio_device"));
                            let selected_audio_text = self
                                .selected_audio_device_index
                                .and_then(|idx| self.state.available_audio_devices.get(idx))
                                .map_or(t!("config.status_default"), |name| {
                                    std::borrow::Cow::Borrowed(name.as_str())
                                });

                            ui.set_min_width(300.0);
                            egui::ComboBox::from_id_salt("audio_device_combo")
                                .selected_text(selected_audio_text)
                                .show_ui(ui, |ui| {
                                    // Track the user's intended action
                                    let mut selected_default = false;
                                    let mut selected_index = None;

                                    if ui
                                        .selectable_label(
                                            self.selected_audio_device_index.is_none(),
                                            t!("config.status_default"),
                                        )
                                        .clicked()
                                    {
                                        selected_default = true;
                                    }

                                    // Iterate list (Immutable borrow of self happens here)
                                    for (i, name) in
                                        self.state.available_audio_devices.iter().enumerate()
                                    {
                                        if ui
                                            .selectable_label(
                                                self.selected_audio_device_index == Some(i),
                                                name,
                                            )
                                            .clicked()
                                        {
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
                            ui.label(t!("config.group_sample_rate"))
                                .on_hover_text(t!("config.tooltip_sample_rate"));
                            egui::ComboBox::from_id_salt("sample_rate_combo")
                                .selected_text(format!("{} Hz", self.state.settings.sample_rate))
                                .show_ui(ui, |ui| {
                                    for &rate in &self.state.available_sample_rates {
                                        if ui
                                            .selectable_label(
                                                self.state.settings.sample_rate == rate,
                                                format!("{}", rate),
                                            )
                                            .clicked()
                                        {
                                            self.state.settings.sample_rate = rate;
                                        }
                                    }
                                });
                            ui.end_row();

                            // --- MIDI Device ---
                            ui.label(t!("config.group_midi_inputs"))
                                .on_hover_text(t!("config.tooltip_midi_inputs"));
                            ui.vertical(|ui| {
                                if self.state.system_midi_ports.is_empty() {
                                    ui.label(
                                        egui::RichText::new(t!("config.status_no_devices")).weak(),
                                    );
                                } else {
                                    for (_port, name) in &self.state.system_midi_ports {
                                        ui.horizontal(|ui| {
                                            // Find corresponding config entry
                                            if let Some(cfg_idx) = self
                                                .state
                                                .settings
                                                .midi_devices
                                                .iter()
                                                .position(|d| d.name == *name)
                                            {
                                                // Checkbox for Enable/Disable
                                                ui.checkbox(
                                                    &mut self.state.settings.midi_devices[cfg_idx]
                                                        .enabled,
                                                    "",
                                                );

                                                // Name Label
                                                ui.label(name);

                                                // Mapping Button
                                                if ui.button(t!("config.btn_map")).clicked() {
                                                    self.midi_mapping_window.device_index = cfg_idx;
                                                    self.midi_mapping_window.visible = true;
                                                }
                                            }
                                        });
                                    }
                                }
                            });
                            ui.end_row();

                            // --- IR File ---
                            ui.label(t!("config.group_ir_file"))
                                .on_hover_text(t!("config.tooltip_ir_file"));

                            let current_ir_name = self
                                .selected_ir_index
                                .and_then(|idx| self.state.available_ir_files.get(idx))
                                .map(|(name, _)| std::borrow::Cow::Borrowed(name.as_str()))
                                .unwrap_or(t!("config.status_no_reverb"));

                            ui.set_min_width(300.0);
                            egui::ComboBox::from_id_salt("ir_combo")
                                .selected_text(current_ir_name)
                                .show_ui(ui, |ui| {
                                    if ui
                                        .selectable_label(
                                            self.selected_ir_index.is_none(),
                                            t!("config.status_no_reverb"),
                                        )
                                        .clicked()
                                    {
                                        self.selected_ir_index = None;
                                        self.state.settings.ir_file = None;
                                    }

                                    for (i, (name, path)) in
                                        self.state.available_ir_files.iter().enumerate()
                                    {
                                        if ui
                                            .selectable_label(
                                                self.selected_ir_index == Some(i),
                                                name,
                                            )
                                            .clicked()
                                        {
                                            self.selected_ir_index = Some(i);
                                            self.state.settings.ir_file = Some(path.clone());
                                        }
                                    }
                                });

                            if ui
                                .button("📂")
                                .on_hover_text(t!("config.tooltip_ir_folder"))
                                .clicked()
                            {
                                if let Ok(dir) = crate::config::get_reverb_directory() {
                                    let _ = open::that(dir);
                                }
                            }
                            ui.end_row();

                            // --- Reverb Mix ---
                            ui.label(t!("config.group_reverb_mix"))
                                .on_hover_text(t!("config.tooltip_reverb_mix"));
                            ui.add(
                                egui::Slider::new(&mut self.state.settings.reverb_mix, 0.0..=1.0)
                                    .show_value(true)
                                    .min_decimals(2)
                                    .text(""),
                            );
                            ui.end_row();

                            // --- Gain ---
                            ui.label(t!("config.group_gain"))
                                .on_hover_text(t!("config.tooltip_gain"));
                            ui.add(
                                egui::Slider::new(&mut self.state.settings.gain, 0.0..=1.0)
                                    .show_value(true)
                                    .min_decimals(2)
                                    .text(""),
                            );
                            ui.end_row();

                            // --- Polyphony ---
                            ui.label(t!("config.group_polyphony"))
                                .on_hover_text(t!("config.tooltip_polyphony"));
                            ui.add(
                                egui::Slider::new(
                                    &mut self.state.settings.polyphony,
                                    1..=1024 * 16,
                                )
                                .show_value(true)
                                .min_decimals(0)
                                .logarithmic(true)
                                .text(""),
                            );
                            ui.end_row();

                            // --- Audio Buffer ---
                            ui.label(t!("config.group_buffer"))
                                .on_hover_text(t!("config.tooltip_buffer"));
                            ui.add(
                                egui::DragValue::new(&mut self.state.settings.audio_buffer_frames)
                                    .speed(32.0)
                                    .range(32..=4096),
                            );
                            ui.end_row();

                            // --- Preload Frames ---
                            ui.label(t!("config.group_preload"))
                                .on_hover_text(t!("config.tooltip_preload"));
                            ui.add_enabled(
                                !self.state.settings.precache,
                                egui::Slider::new(&mut self.state.settings.max_ram_gb, 0.0..=256.0)
                                    .show_value(true)
                                    .min_decimals(1)
                                    .step_by(0.1)
                                    .text(""),
                            );
                            ui.end_row();

                            // --- Boolean Options ---
                            ui.label(t!("config.group_options"));
                            ui.vertical(|ui| {
                                ui.checkbox(
                                    &mut self.state.settings.precache,
                                    t!("config.chk_precache"),
                                )
                                .on_hover_text(t!("config.tooltip_precache"));
                                ui.checkbox(
                                    &mut self.state.settings.convert_to_16bit,
                                    t!("config.chk_convert"),
                                )
                                .on_hover_text(t!("config.tooltip_convert"));
                                ui.checkbox(
                                    &mut self.state.settings.original_tuning,
                                    t!("config.chk_tuning"),
                                )
                                .on_hover_text(t!("config.tooltip_tuning"));
                            });
                            ui.end_row();

                            // --- LCD Configuration ---
                            ui.label(t!("config.lcd_title"));
                            if ui.button(t!("config.lcd_button")).clicked() {
                                self.show_lcd_config = true;
                            }
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
                        let quit_button_text = egui::RichText::new(t!("config.btn_quit"))
                            .text_style(egui::TextStyle::Heading);

                        let quit_button = ui.add_enabled(
                            self.state.settings.organ_file.is_some(),
                            egui::Button::new(quit_button_text),
                        );

                        if quit_button.clicked() {
                            *self.is_finished.lock().unwrap() = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }

                        let start_button_text = egui::RichText::new(t!("config.btn_start"))
                            .text_style(egui::TextStyle::Heading);

                        let start_button = ui.add_enabled(
                            self.state.settings.organ_file.is_some(),
                            egui::Button::new(start_button_text),
                        );

                        if start_button.clicked() {
                            // Collect enabled MIDI devices + ports
                            let mut active_devices = Vec::new();
                            for (port, name) in &self.state.system_midi_ports {
                                if let Some(cfg) = self
                                    .state
                                    .settings
                                    .midi_devices
                                    .iter()
                                    .find(|d| d.name == *name)
                                {
                                    if cfg.enabled {
                                        active_devices.push((port.clone(), cfg.clone()));
                                    }
                                }
                            }
                            let audio_device_name = self
                                .selected_audio_device_index
                                .and_then(|idx| self.state.available_audio_devices.get(idx))
                                .cloned();

                            let runtime_config = RuntimeConfig {
                                organ_file: self.state.settings.organ_file.clone().unwrap(),
                                ir_file: self.state.settings.ir_file.clone(),
                                reverb_mix: self.state.settings.reverb_mix,
                                audio_buffer_frames: self.state.settings.audio_buffer_frames,
                                max_ram_gb: self.state.settings.max_ram_gb,
                                precache: self.state.settings.precache,
                                convert_to_16bit: self.state.settings.convert_to_16bit,
                                original_tuning: self.state.settings.original_tuning,
                                midi_file: self.state.midi_file.clone(),
                                active_midi_devices: active_devices,
                                gain: self.state.settings.gain,
                                polyphony: self.state.settings.polyphony,
                                max_new_voices_per_block: self
                                    .state
                                    .settings
                                    .max_new_voices_per_block,
                                audio_device_name,
                                sample_rate: self.state.settings.sample_rate,
                                lcd_displays: self.state.settings.lcd_displays.clone(),
                            };

                            // Save config back to settings
                            // (midi_output_device is removed)

                            *self.output.lock().unwrap() = Some(runtime_config);
                            *self.is_finished.lock().unwrap() = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }

                        if self.state.settings.organ_file.is_none() {
                            ui.label(
                                egui::RichText::new(t!("config.warn_select_organ"))
                                    .color(egui::Color32::YELLOW),
                            );
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
        || t!("config.status_none").to_string(),
        |p| p.display().to_string(),
    )
}

/// Helper to get just the filename as a string
fn path_to_str_truncated(path: Option<&std::path::Path>) -> String {
    path.and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map_or(t!("config.status_none").to_string(), |s| s.to_string())
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
            .with_inner_size([700.0, 800.0])
            .with_resizable(true),
        ..Default::default()
    };

    let app = ConfigApp::new(
        settings,
        Arc::clone(&midi_input_arc),
        Arc::clone(&output),
        Arc::clone(&is_finished),
    );

    let win_title = t!("config.window_title").to_string();

    eframe::run_native(
        &win_title,
        native_options,
        Box::new(|_cc| Ok(Box::new(app))),
    )
    .map_err(|e| anyhow::anyhow!("Eframe error: {}", e))?;

    let final_config = output.lock().unwrap().clone();
    Ok(final_config)
}
