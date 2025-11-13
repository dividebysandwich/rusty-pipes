use anyhow::Result;
use eframe::{egui, App, Frame};
use egui::{Stroke, UiBuilder};
use midir::MidiInputConnection;
use std::{
    path::PathBuf,
    sync::{
        mpsc::Sender,
        Arc, Mutex
    },
    time::{Duration, Instant},
};

use crate::{
    app::{AppMessage, TuiMessage, PIPES, LOGO},
    app_state::{connect_to_midi, AppState},
    organ::Organ,
};

enum GuiMode {
    MidiSelection,
    MainApp,
}

pub struct EguiApp {
    app_state: Arc<Mutex<AppState>>,
    audio_tx: Sender<AppMessage>,
    tui_tx: Sender<TuiMessage>,
    mode: GuiMode,

    // Need to hold the connection to keep it alive
    midi_connection: Option<MidiInputConnection<()>>,

    // --- GUI-specific state ---
    selected_midi_port_index: Option<usize>,
    selected_stop_index: Option<usize>,
    stop_list_scroll_offset: f32,

    show_preset_save_modal: bool,
    preset_save_slot: usize,
    preset_save_name: String,
}

/// Runs the main GUI loop.
pub fn run_gui_loop(
    audio_tx: Sender<AppMessage>,
    app_state: Arc<Mutex<AppState>>,
    tui_tx: Sender<TuiMessage>,
    organ: Arc<Organ>,
    ir_file_path: Option<PathBuf>,
    reverb_mix: f32,
    is_file_playback: bool,
    preselected_device_name: Option<String>,
) -> Result<()> {

    let mut gui_mode = GuiMode::MidiSelection;
    let mut connection: Option<MidiInputConnection<()>> = None;
    let mut selected_midi_port_index: Option<usize> = None;

    // Scope to limit the lock duration
    {
        let mut app_state_locked = app_state.lock().unwrap();
        // Handle preselected MIDI device (same logic as TUI)
        if !is_file_playback {
            if let Some(device_name) = preselected_device_name {
                let found_port = app_state_locked
                    .available_ports
                    .iter()
                    .find(|(_, name)| *name == device_name)
                    .map(|(port, _)| port.clone());

                if let Some(port) = found_port {
                    if let Some(midi_input) = app_state_locked.midi_input.take() {
                        let conn = connect_to_midi(midi_input, &port, &device_name, &tui_tx)?;
                        connection = Some(conn);
                        gui_mode = GuiMode::MainApp;
                        app_state_locked.add_midi_log(format!("Connected to: {}", device_name));
                        app_state_locked.available_ports.clear();
                    }
                } else {
                    app_state_locked.error_msg =
                        Some(format!("ERROR: MIDI device not found: '{}'", device_name));
                }
            }
            
            if !app_state_locked.available_ports.is_empty() {
                selected_midi_port_index = Some(0);
            }

        } else {
            gui_mode = GuiMode::MainApp; // Skip selection if playing file
        }

        // Handle IR file (same logic as TUI)
        if let Some(path) = ir_file_path {
            if path.exists() {
                let log_msg = format!("Loading IR file: {:?}", path.file_name().unwrap());
                app_state_locked.add_midi_log(log_msg);
                audio_tx.send(AppMessage::SetReverbIr(path))?;
                audio_tx.send(AppMessage::SetReverbWetDry(reverb_mix))?;
            } else {
                let log_msg = format!("ERROR: IR file not found: {}", path.display());
                app_state_locked.add_midi_log(log_msg);
            }
        }
    } // Scope ends, unlock app_state

    let selected_stop_index = if !organ.stops.is_empty() { Some(0) } else { None };

    let egui_app = EguiApp {
        app_state,
        audio_tx,
        tui_tx,
        mode: gui_mode,
        midi_connection: connection,
        selected_midi_port_index,
        selected_stop_index,
        stop_list_scroll_offset: 0.0,
        show_preset_save_modal: false,
        preset_save_slot: 0,
        preset_save_name: String::new(),
    };

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 600.0]),
        ..Default::default()
    };
    
    eframe::run_native(
        &format!("Rusty Pipes - {}", organ.name),
        native_options,
        Box::new(|_cc| Ok::<Box<dyn App>, Box<dyn std::error::Error + Send + Sync>>(Box::new(egui_app))),
    )
    .map_err(|e| anyhow::anyhow!("Eframe error: {}", e))?;

    Ok(())
}

impl App for EguiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut Frame) {

        if !self.show_preset_save_modal { // Don't process keys if modal is open
        let input = ctx.input(|i| i.clone()); // Get a snapshot of the input state

            let function_keys = [
                egui::Key::F1, egui::Key::F2, egui::Key::F3, egui::Key::F4,
                egui::Key::F5, egui::Key::F6, egui::Key::F7, egui::Key::F8,
                egui::Key::F9, egui::Key::F10, egui::Key::F11, egui::Key::F12,
            ];
            for (i, &key) in function_keys.iter().enumerate() {
                if input.key_pressed(key) {
                    if input.modifiers.shift {
                        self.preset_save_slot = i;
                        // Pre-fill name if it exists, otherwise "Preset F{}"
                        self.preset_save_name = self.app_state.lock().unwrap().presets[i]
                            .as_ref()
                            .map_or_else(
                                || format!("Preset F{}", i + 1), // Default name
                                |p| p.name.clone() // Current name
                            );
                        self.show_preset_save_modal = true;
                    } else {
                        let mut app_state = self.app_state.lock().unwrap();
                        // Recall logic
                        if let Err(e) = app_state.recall_preset(i, &self.audio_tx) {
                            app_state.add_midi_log(format!("ERROR recalling preset: {}", e));
                        }
                    }
                }
            }
            if input.key_pressed(egui::Key::P) {
                self.audio_tx.send(AppMessage::AllNotesOff).unwrap_or_else(|e| {
                    log::error!("ERROR sending AllNotesOff: {}", e);
                });
            }
        }

        // Update internal state (e.g., piano roll)
        if matches!(self.mode, GuiMode::MainApp) {
            self.app_state.lock().unwrap().update_piano_roll_state();
            // Request continuous repaints for the piano roll
            ctx.request_repaint_after(Duration::from_millis(30));
        }

        // Draw the UI based on mode
        match self.mode {
            GuiMode::MidiSelection => {
                self.draw_midi_selection_ui(ctx);
            }
            GuiMode::MainApp => {
                self.draw_main_app_ui(ctx);
            }
        }

        // Draw preset save modal if needed
        self.draw_preset_save_modal(ctx);
    }

    // Handle quit request (e.g., pressing 'X' on window)
    fn on_exit(&mut self, _glow_ctx: Option<&eframe::glow::Context>) {
        if let Err(e) = self.audio_tx.send(AppMessage::Quit) {
            // Log to terminal, as GUI is closing
            eprintln!("Failed to send Quit message on close: {}", e);
        }
    }
}

// --- UI Drawing Implementations ---

impl EguiApp {
    fn draw_midi_selection_ui(&mut self, ctx: &egui::Context) {
        let panel_frame = egui::Frame::default();
        egui::CentralPanel::default().frame(panel_frame).show(ctx, |ui| {
            ui.vertical_centered(|ui| {

                // Set a font size for the ASCII art
                let font_size = 10.0; 
                let mono_font = egui::FontId::monospace(font_size);
                let orange = egui::Color32::from_rgb(255, 165, 0);

                // Draw the pipes (gray)
                ui.label(
                    egui::RichText::new(PIPES)
                        .font(mono_font.clone())
                        .color(egui::Color32::GRAY),
                );
                // Draw the main logo (orange)
                ui.label(
                    egui::RichText::new(LOGO)
                        .font(mono_font.clone())
                        .color(orange),
                );
                            // Draw the tagline from the TUI (orange)
                ui.label(
                    egui::RichText::new("Indicia MMXXV")
                        .font(mono_font) // No clone needed on last use
                        .color(orange),
                );
                ui.add_space(10.0); // Space between logo and text
                
                ui.heading("Rusty Pipes");
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.label(env!("CARGO_PKG_DESCRIPTION"));
                ui.add_space(30.0);

                // --- Lock Scope for reading state ---
                {
                    let mut app_state = self.app_state.lock().unwrap();
                    if let Some(err) = &app_state.error_msg {
                        ui.label(egui::RichText::new(err).color(egui::Color32::RED));
                    }

                    if app_state.available_ports.is_empty() {
                        ui.label("No MIDI Input Devices Found!");
                    } else {
                        ui.label("Select a MIDI Input Device:");
                    }

                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.group(|ui| {
                            for (i, (_, name)) in app_state.available_ports.iter().enumerate() {
                                if ui.selectable_label(self.selected_midi_port_index == Some(i), name).clicked() {
                                    self.selected_midi_port_index = Some(i);
                                }
                            }
                        });
                    });
                    
                    ui.add_space(10.0);

                    let connect_clicked = ui.button("Connect").clicked();

                    if connect_clicked {
                        if let Some(selected_idx) = self.selected_midi_port_index {
                            if let Some((port_to_connect, port_name)) = app_state
                                .available_ports
                                .get(selected_idx)
                                .map(|(p, n)| (p.clone(), n.clone()))
                            {
                                app_state
                                    .add_midi_log(format!("Connecting to: {}", port_name));

                                if let Some(midi_input) = app_state.midi_input.take() {
                                    match connect_to_midi(
                                        midi_input,
                                        &port_to_connect,
                                        &port_name,
                                        &self.tui_tx,
                                    ) {
                                        Ok(conn) => {
                                            self.midi_connection = Some(conn);
                                            self.mode = GuiMode::MainApp;
                                            app_state.available_ports.clear();
                                        }
                                        Err(e) => {
                                            app_state.error_msg = Some(format!("Failed to connect: {}", e));
                                            // Put the midi_input back if it failed
                                            // Note: This is tricky as midir::Input consumes itself.
                                            // A better way would be to re-initialize it.
                                            // For now, we'll just show the error.
                                        }
                                    }
                                }
                            }
                        }
                    }
                } // End lock scope
            });
        });
    }

    fn draw_main_app_ui(&mut self, ctx: &egui::Context) {
        self.draw_footer(ctx);
        self.draw_preset_panel(ctx);
        self.draw_log_and_piano_roll_panel(ctx);

        let panel_frame = egui::Frame {
            fill: egui::Color32::from_rgb(30, 30, 30),
            ..Default::default()
        };

        egui::CentralPanel::default().frame(panel_frame).show(ctx, |ui| {
            let organ_name = self.app_state.lock().unwrap().organ.name.clone();
            ui.heading(organ_name);
            ui.separator();
            
            self.draw_stop_controls(ui);

            ui.separator();
            
            let scroll = egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .vertical_scroll_offset(self.stop_list_scroll_offset);
                
            let scroll_out = scroll.show(ui, |ui| {
                self.draw_stop_list_columns(ui);
            });
            self.stop_list_scroll_offset = scroll_out.state.offset.y;
        });
    }

    fn draw_footer(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            let mut app_state = self.app_state.lock().unwrap();
            if let Some(err) = &app_state.error_msg {
                ui.label(egui::RichText::new(err).color(egui::Color32::RED));
            } else {
                ui.label("Tip: F1-F12 to Recall, Shift+F1-F12 to Save, 'P' for Panic");
            }
            // Clear error after showing it
            app_state.error_msg = None;
        });
    }

    fn draw_preset_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("preset_panel").show(ctx, |ui| {
            ui.heading("Presets");

            let mut app_state = self.app_state.lock().unwrap();
            
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.label("Recall (F1-F12):");
                egui::Grid::new("preset_recall_grid").num_columns(2).show(ui, |ui| {
                    for i in 0..12 {
                        // Get name and state from Preset struct
                        let (text, is_loaded) = app_state.presets[i]
                            .as_ref()
                            .map_or_else(
                                || (format!("F{}", i + 1), false), // No preset
                                |p| (format!("F{}: {}", i + 1, p.name.clone()), true) // Preset loaded
                            );

                        if ui.add_enabled(is_loaded, egui::Button::new(text)).clicked() {
                            if let Err(e) = app_state.recall_preset(i, &self.audio_tx) {
                                app_state.add_midi_log(format!("ERROR recalling preset: {}", e));
                            }
                        }
                        if (i + 1) % 2 == 0 { ui.end_row(); }
                    }
                });
                
                ui.separator();
                
                ui.label("Save (Shift+F1-F12):");
                egui::Grid::new("preset_save_grid").num_columns(2).show(ui, |ui| {
                    for i in 0..12 {
                        let text = format!("F{}", i + 1);
                        if ui.button(text).clicked() {
                            self.preset_save_slot = i;
                            self.preset_save_name = app_state.presets[i]
                                .as_ref()
                                .map_or_else(
                                    || format!("Preset F{}", i + 1), // Default name
                                    |p| p.name.clone() // Current name
                                );
                            self.show_preset_save_modal = true;
                        }
                        if (i + 1) % 2 == 0 { ui.end_row(); }
                    }
                });
            });
        });
    }

    fn draw_stop_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Selected Stop:");
            let mut app_state = self.app_state.lock().unwrap();
            if let Some(idx) = self.selected_stop_index {
                let stop = &app_state.organ.stops[idx];
                ui.label(egui::RichText::new(&stop.name).strong());

                if ui.button("All Channels").clicked() {
                    app_state.select_all_channels_for_stop(idx);
                }
                if ui.button("No Channels").clicked() {
                    if let Err(e) = app_state.select_none_channels_for_stop(idx, &self.audio_tx) {
                        app_state.add_midi_log(format!("ERROR: {}", e));
                    }
                }
            } else {
                 ui.label(egui::RichText::new("None").italics());
            }
            
            ui.separator();
            
            if ui.button("PANIC (All Notes Off)").on_hover_text("Stops all sounding notes").clicked() {
                if let Err(e) = self.audio_tx.send(AppMessage::AllNotesOff) {
                     app_state.add_midi_log(format!("ERROR: {}", e));
                }
            }
        });
    }

    fn draw_stop_list_columns(&mut self, ui: &mut egui::Ui) {
        let mut app_state = self.app_state.lock().unwrap();
        let num_cols = 3;
        let stops: Vec<_> = app_state.organ.stops.clone();
        let stops_count = stops.len();
        let stop_channels = app_state.stop_channels.clone();
        if stops_count == 0 {
            ui.label("No stops loaded.");
            return;
        }

        // Calculate how many stops go in each column
        let items_per_column = (stops_count + num_cols - 1) / num_cols;

        // Create 3 resizable layout columns
        ui.columns(num_cols, |cols| {
            for (col_idx, ui) in cols.iter_mut().enumerate() {
                // Calculate the range of stops for this specific column
                let start_idx = col_idx * items_per_column;
                let end_idx = (start_idx + items_per_column).min(stops_count);

                if start_idx >= end_idx { continue; } // Skip if this column is empty

                // Render all stops for this column
                for i in start_idx..end_idx {
                    let is_selected = self.selected_stop_index == Some(i);
                    let active_channels = stop_channels
                        .get(&i)
                        .cloned()
                        .unwrap_or_default();
                    let is_active = !active_channels.is_empty();
                    let stop = &stops[i];

                    ui.vertical(|ui| { 
                            
                        // Stop Name (on top)
                        let label_text = egui::RichText::new(&stop.name);
                        let label_text = if is_active {
                            label_text.color(egui::Color32::from_rgb(100, 255, 100))
                        } else {
                            label_text
                        };

                        // The .selectable_label will wrap text automatically
                        if ui.selectable_label(is_selected, label_text).clicked() {
                            self.selected_stop_index = Some(i);
                        }
                        
                        // Toggles (below)
                        ui.group(|ui| {
                            // Use horizontal_wrapped to allow toggles to "scale"
                            ui.horizontal_wrapped(|ui| {
                                for chan in 0..10u8 {
                                    let is_on = active_channels.contains(&chan);
                                    let display_char = if chan == 9 { '0' } else { (b'1' + chan) as char };
                                    
                                    if ui.selectable_label(is_on, display_char.to_string()).clicked() {
                                        if let Err(e) = app_state.toggle_stop_channel(i, chan, &self.audio_tx) {
                                            app_state.add_midi_log(format!("ERROR: {}", e));
                                        }
                                    }
                                }
                            });
                        }); // End toggle group
                    });

                    ui.add_space(2.0); // Add a small gap between stops
                }
            }
        });
    }


    fn draw_log_and_piano_roll_panel(&mut self, ctx: &egui::Context) {
        const LOG_WIDTH: f32 = 300.0;

        egui::TopBottomPanel::bottom("bottom_panel")
        .resizable(true)
        .default_height(250.0)
        .min_height(250.0)
        .show(ctx, |ui| {
            
            let full_rect = ui.available_rect_before_wrap();
            let split_x = (full_rect.left() + LOG_WIDTH).min(full_rect.right());
            let (log_rect, piano_rect) = full_rect.split_left_right_at_x(split_x);

            // --- Column 0: MIDI Log (Fixed Width) ---
            ui.scope_builder( 
                UiBuilder{ 
                    max_rect: Some(log_rect),
                    layout: Some(egui::Layout::top_down(egui::Align::LEFT)),
                    ..Default::default()
                }, |ui| {
                ui.heading("MIDI Log");
                
                egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                    // 4. (Optional but recommended) Tell labels to wrap
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap); 
                    let app_state = self.app_state.lock().unwrap();
                    for msg in &app_state.midi_log {
                        ui.label(msg); // This will now wrap
                    }
                });
            });

            // --- Column 1: Piano Roll (Remaining Width) ---
            ui.scope_builder( UiBuilder{ max_rect: Some(piano_rect), layout: Some(egui::Layout::top_down(egui::Align::LEFT)), ..Default::default()}, |ui| {
                ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                    ui.heading("Piano Roll");
                    self.draw_piano_roll(ui);
                });
            });
        });
    }

    fn draw_piano_roll(&self, ui: &mut egui::Ui) {
        let app_state = self.app_state.lock().unwrap();

        const PIANO_LOW_NOTE: u8 = 21;  // A0
        const PIANO_HIGH_NOTE: u8 = 108; // C8
        const BLACK_KEY_MODS: [u8; 5] = [1, 3, 6, 8, 10]; // C#, D#, F#, G#, A#

        let desired_size = ui.available_size_before_wrap();

        let (response, painter) = ui.allocate_painter(
            desired_size,
            egui::Sense::hover(),
        );
        let rect = response.rect;

        let now = Instant::now();
        let display_start_time = now.checked_sub(app_state.piano_roll_display_duration)
            .unwrap_or(Instant::now());
        let total_duration_f64 = app_state.piano_roll_display_duration.as_secs_f64();

        // Draw Background (Keyboard)
        let note_range = (PIANO_HIGH_NOTE - PIANO_LOW_NOTE + 1) as f32;
        let key_width = rect.width() / note_range;

        for note in PIANO_LOW_NOTE..=PIANO_HIGH_NOTE {
            let is_black_key = BLACK_KEY_MODS.contains(&(note % 12));
            let color = if is_black_key {
                egui::Color32::from_gray(50)
            } else {
                egui::Color32::from_gray(100)
            };
            
            let x_start = egui::remap(
                note as f64, 
                PIANO_LOW_NOTE as f64..=(PIANO_HIGH_NOTE + 1) as f64, 
                rect.left() as f64..=rect.right() as f64
            ) as f32;

            painter.add(egui::Shape::Rect(egui::epaint::RectShape::new(
                egui::Rect::from_x_y_ranges(x_start..=(x_start + key_width), rect.y_range()),
                egui::CornerRadius::ZERO,
                color, // fill
                Stroke::new(1.0, color), // border
                egui::StrokeKind::Inside,
            )));
        }
        
        // Helper to map time to Y-coord
        let map_time_to_y = |time: Instant| -> f32 {
            let time_since_start = time.duration_since(display_start_time).as_secs_f64();
            // Remap 0.0 -> total_duration to rect.bottom() -> rect.top() (inverted)
            egui::remap(
                time_since_start, 
                0.0..=total_duration_f64, 
                rect.bottom() as f64..=rect.top() as f64
            ) as f32
        };
        
        // Helper to map note to X-coord
        let map_note_to_x_range = |note: u8| -> (f32, f32) {
            let x_start = egui::remap(
                note as f64, 
                PIANO_LOW_NOTE as f64..=(PIANO_HIGH_NOTE + 1) as f64, 
                rect.left() as f64..=rect.right() as f64
            ) as f32;
            (x_start, x_start + key_width)
        };
        
        // Draw Finished Notes
        for note in &app_state.finished_notes_display {
            let (x1, x2) = map_note_to_x_range(note.note);
            let y1 = map_time_to_y(note.start_time);
            let y2 = map_time_to_y(note.end_time.unwrap_or(now));
            
            painter.add(egui::Shape::Rect(egui::epaint::RectShape::new(
                egui::Rect::from_x_y_ranges(x1..=x2, y2..=y1), // y2 is "higher" (older)
                egui::CornerRadius::ZERO,
                egui::Color32::from_rgb(255, 0, 255), // Magenta
                egui::Stroke::NONE,
                egui::StrokeKind::Inside,
            )));
        }
        
        // Draw Active Notes
        for note in app_state.currently_playing_notes.values() {
            let (x1, x2) = map_note_to_x_range(note.note);
            let y1 = map_time_to_y(note.start_time);
            let y2 = map_time_to_y(now); // End at the "now" line
            
            painter.add(egui::Shape::Rect(egui::epaint::RectShape::new(
                egui::Rect::from_x_y_ranges(x1..=x2, y2..=y1),
                egui::CornerRadius::ZERO,
                egui::Color32::from_rgb(0, 255, 0), // Green
                egui::Stroke::NONE,
                egui::StrokeKind::Inside,
            )));
        }
    }

    /// Renders a modal window for saving a preset.
    fn draw_preset_save_modal(&mut self, ctx: &egui::Context) {
        if !self.show_preset_save_modal {
            return;
        }

        let mut is_open = self.show_preset_save_modal;
        let slot_display = self.preset_save_slot + 1;

        egui::Window::new(format!("Save Preset F{}", slot_display))
            .open(&mut is_open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label("Enter a name for the preset:");
                    
                    let text_edit = egui::TextEdit::singleline(&mut self.preset_save_name)
                        .desired_width(250.0);
                    let response = ui.add(text_edit);

                    // Auto-focus the text input when the window opens
                    if !response.has_focus() {
                        response.request_focus();
                    }
                    
                    ui.add_space(10.0);
                    
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.show_preset_save_modal = false;
                        }
                        
                        // Check for 'Enter' key or button click
                        let save_triggered = ui.button("Save").clicked() || 
                                             (response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));

                        if save_triggered {
                            if !self.preset_save_name.is_empty() {
                                self.app_state.lock().unwrap().save_preset(
                                    self.preset_save_slot, 
                                    self.preset_save_name.clone()
                                );
                                self.show_preset_save_modal = false;
                            }
                        }
                    });

                    // If 'Enter' was pressed, close the modal
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                         self.show_preset_save_modal = false;
                    }

                });
            });
        
        // If the user clicked the 'X' button on the window
        if !is_open {
            self.show_preset_save_modal = false;
        }
    }
}
