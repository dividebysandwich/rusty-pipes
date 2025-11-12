
use anyhow::Result;
use eframe::{egui, App, Frame};
use egui::Stroke;
use midir::MidiInputConnection;
use std::{
    path::PathBuf,
    sync::{
        mpsc::{Receiver, Sender},
        Arc,
    },
    time::{Duration, Instant},
};

use crate::{
    app::{AppMessage, TuiMessage},
    app_state::{connect_to_midi, AppState},
    organ::Organ,
};

enum GuiMode {
    MidiSelection,
    MainApp,
}

pub struct EguiApp {
    shared_state: AppState,
    audio_tx: Sender<AppMessage>,
    tui_rx: Receiver<TuiMessage>, // Receives messages from MIDI
    tui_tx: Sender<TuiMessage>,
    mode: GuiMode,

    // Need to hold the connection to keep it alive
    midi_connection: Option<MidiInputConnection<()>>,

    // --- GUI-specific state ---
    selected_midi_port_index: Option<usize>,
    selected_stop_index: Option<usize>,
    stop_list_scroll_offset: f32,
}

/// Runs the main GUI loop.
pub fn run_gui_loop(
    audio_tx: Sender<AppMessage>,
    tui_rx: Receiver<TuiMessage>,
    tui_tx: Sender<TuiMessage>,
    organ: Arc<Organ>,
    ir_file_path: Option<PathBuf>,
    reverb_mix: f32,
    is_file_playback: bool,
    preselected_device_name: Option<String>,
) -> Result<()> {
    let mut app_state = AppState::new(organ.clone(), is_file_playback)?;

    let mut gui_mode = GuiMode::MidiSelection;
    let mut connection: Option<MidiInputConnection<()>> = None;
    let mut selected_midi_port_index: Option<usize> = None;

    // Handle preselected MIDI device (same logic as TUI)
    if !is_file_playback {
        if let Some(device_name) = preselected_device_name {
            let found_port = app_state
                .available_ports
                .iter()
                .find(|(_, name)| *name == device_name)
                .map(|(port, _)| port.clone());

            if let Some(port) = found_port {
                if let Some(midi_input) = app_state.midi_input.take() {
                    let conn = connect_to_midi(midi_input, &port, &device_name, &tui_tx)?;
                    connection = Some(conn);
                    gui_mode = GuiMode::MainApp;
                    app_state.add_midi_log(format!("Connected to: {}", device_name));
                    app_state.available_ports.clear();
                }
            } else {
                app_state.error_msg =
                    Some(format!("ERROR: MIDI device not found: '{}'", device_name));
            }
        }
        
        if !app_state.available_ports.is_empty() {
             selected_midi_port_index = Some(0);
        }

    } else {
        gui_mode = GuiMode::MainApp; // Skip selection if playing file
    }

    // Handle IR file (same logic as TUI)
    if let Some(path) = ir_file_path {
        if path.exists() {
            let log_msg = format!("Loading IR file: {:?}", path.file_name().unwrap());
            app_state.add_midi_log(log_msg);
            audio_tx.send(AppMessage::SetReverbIr(path))?;
            audio_tx.send(AppMessage::SetReverbWetDry(reverb_mix))?;
        } else {
            let log_msg = format!("ERROR: IR file not found: {}", path.display());
            app_state.add_midi_log(log_msg);
        }
    }
    
    let selected_stop_index = if !organ.stops.is_empty() { Some(0) } else { None };

    let egui_app = EguiApp {
        shared_state: app_state,
        audio_tx,
        tui_rx,
        tui_tx,
        mode: gui_mode,
        midi_connection: connection,
        selected_midi_port_index,
        selected_stop_index,
        stop_list_scroll_offset: 0.0,
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
        // Process all pending TuiMessages
        while let Ok(msg) = self.tui_rx.try_recv() {
            if let Err(e) = self.shared_state.handle_tui_message(msg, &self.audio_tx) {
                self.shared_state
                    .add_midi_log(format!("Error handling message: {}", e));
            }
        }

        // Update internal state (e.g., piano roll)
        if matches!(self.mode, GuiMode::MainApp) {
            self.shared_state.update_piano_roll_state();
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
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.2);
                ui.heading("Rusty Pipes");
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.label(env!("CARGO_PKG_DESCRIPTION"));
                ui.add_space(30.0);

                if let Some(err) = &self.shared_state.error_msg {
                    ui.label(egui::RichText::new(err).color(egui::Color32::RED));
                }

                if self.shared_state.available_ports.is_empty() {
                    ui.label("No MIDI Input Devices Found!");
                } else {
                    ui.label("Select a MIDI Input Device:");
                }

                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.group(|ui| {
                        for (i, (_, name)) in self.shared_state.available_ports.iter().enumerate() {
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
                        if let Some((port_to_connect, port_name)) = self
                            .shared_state
                            .available_ports
                            .get(selected_idx)
                            .map(|(p, n)| (p.clone(), n.clone()))
                        {
                            self.shared_state
                                .add_midi_log(format!("Connecting to: {}", port_name));

                            if let Some(midi_input) = self.shared_state.midi_input.take() {
                                match connect_to_midi(
                                    midi_input,
                                    &port_to_connect,
                                    &port_name,
                                    &self.tui_tx,
                                ) {
                                    Ok(conn) => {
                                        self.midi_connection = Some(conn);
                                        self.mode = GuiMode::MainApp;
                                        self.shared_state.available_ports.clear();
                                    }
                                    Err(e) => {
                                        self.shared_state.error_msg = Some(format!("Failed to connect: {}", e));
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
            });
        });
    }

    fn draw_main_app_ui(&mut self, ctx: &egui::Context) {
        self.draw_footer(ctx);
        self.draw_preset_panel(ctx);
        self.draw_log_and_piano_roll_panel(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(&self.shared_state.organ.name);
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
        egui::TopBottomPanel::bottom("footer")
            .show(ctx, |ui| {
                 if let Some(err) = &self.shared_state.error_msg {
                    ui.label(egui::RichText::new(err).color(egui::Color32::RED));
                } else {
                    ui.label("Tip: Use Ctrl+Scroll to zoom piano roll. (Not implemented, just a placeholder)");
                }
            });
    }

    fn draw_preset_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("preset_panel").show(ctx, |ui| {
            ui.heading("Presets");
            
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.label("Recall (F1-F12):");
                egui::Grid::new("preset_recall_grid").num_columns(2).show(ui, |ui| {
                    for i in 0..12 {
                        let text = format!("F{}", i + 1);
                        let is_loaded = self.shared_state.presets[i].is_some();
                        if ui.add_enabled(is_loaded, egui::Button::new(text)).clicked() {
                            if let Err(e) = self.shared_state.recall_preset(i, &self.audio_tx) {
                                self.shared_state.add_midi_log(format!("ERROR recalling preset: {}", e));
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
                            self.shared_state.save_preset(i);
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
            if let Some(idx) = self.selected_stop_index {
                let stop = &self.shared_state.organ.stops[idx];
                ui.label(egui::RichText::new(&stop.name).strong());

                if ui.button("All Channels").clicked() {
                    self.shared_state.select_all_channels_for_stop(idx);
                }
                if ui.button("No Channels").clicked() {
                    if let Err(e) = self.shared_state.select_none_channels_for_stop(idx, &self.audio_tx) {
                        self.shared_state.add_midi_log(format!("ERROR: {}", e));
                    }
                }
            } else {
                 ui.label(egui::RichText::new("None").italics());
            }
            
            ui.separator();
            
            if ui.button("PANIC (All Notes Off)").on_hover_text("Stops all sounding notes").clicked() {
                if let Err(e) = self.audio_tx.send(AppMessage::AllNotesOff) {
                     self.shared_state.add_midi_log(format!("ERROR: {}", e));
                }
            }
        });
    }

    fn draw_stop_list_columns(&mut self, ui: &mut egui::Ui) {
        let num_cols = 3;
        let stops_count = self.shared_state.organ.stops.len();
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
                    let active_channels = self
                        .shared_state
                        .stop_channels
                        .get(&i)
                        .cloned()
                        .unwrap_or_default();
                    let is_active = !active_channels.is_empty();

                    // We use a horizontal layout, just like the TUI
                    ui.horizontal(|ui| {
                        // Channel Toggles
                        // We group them so they have a faint background
                        ui.group(|ui| {
                            ui.horizontal(|ui| { // Use horizontal, NOT wrapped
                                for chan in 0..10u8 {
                                    let is_on = active_channels.contains(&chan);
                                    let display_char = if chan == 9 { '0' } else { (b'1' + chan) as char };
                                
                                    // Use a SelectableLabel for a compact toggle "button"
                                    if ui.selectable_label(is_on, display_char.to_string()).clicked() {
                                        if let Err(e) = self.shared_state.toggle_stop_channel(i, chan, &self.audio_tx) {
                                            self.shared_state.add_midi_log(format!("ERROR: {}", e));
                                        }
                                    }
                                }
                            });
                        }); // End toggle group

                        // Stop Name
                        let stop = &self.shared_state.organ.stops[i];
                        let label_text = egui::RichText::new(&stop.name);
                        let label_text = if is_active {
                            label_text.color(egui::Color32::from_rgb(100, 255, 100)) // Green
                        } else {
                            label_text
                        };

                        // Make the name selectable to set the "Selected Stop"
                        if ui.selectable_label(is_selected, label_text).clicked() {
                            self.selected_stop_index = Some(i);
                        }
                    }); // End horizontal layout for one stop
                
                    ui.add_space(2.0); // Add a small gap between stops
                }
            }
        });
    }

    fn draw_log_and_piano_roll_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("bottom_panel")
            .resizable(true)
            .default_height(250.0)
            .show(ctx, |ui| {
                ui.columns(2, |cols| {
                    // --- Column 0: MIDI Log ---
                    cols[0].heading("MIDI Log");
                    egui::ScrollArea::vertical().stick_to_bottom(true).show(&mut cols[0], |ui| {
                        for msg in &self.shared_state.midi_log {
                            ui.label(msg);
                        }
                    });
                    
                    // --- Column 1: Piano Roll ---
                    cols[1].heading("Piano Roll");
                    self.draw_piano_roll(&mut cols[1]);
                });
            });
    }

    fn draw_piano_roll(&self, ui: &mut egui::Ui) {
        const PIANO_LOW_NOTE: u8 = 21;  // A0
        const PIANO_HIGH_NOTE: u8 = 108; // C8
        const BLACK_KEY_MODS: [u8; 5] = [1, 3, 6, 8, 10]; // C#, D#, F#, G#, A#

        let (response, painter) = ui.allocate_painter(
            ui.available_size_before_wrap(),
            egui::Sense::hover(),
        );
        let rect = response.rect;

        let now = Instant::now();
        let display_start_time = now.checked_sub(self.shared_state.piano_roll_display_duration)
            .unwrap_or(Instant::now());
        let total_duration_f64 = self.shared_state.piano_roll_display_duration.as_secs_f64();

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
        
        // --- Helper to map time to Y-coord ---
        let map_time_to_y = |time: Instant| -> f32 {
            let time_since_start = time.duration_since(display_start_time).as_secs_f64();
            // Remap 0.0 -> total_duration to rect.bottom() -> rect.top() (inverted)
            egui::remap(
                time_since_start, 
                0.0..=total_duration_f64, 
                rect.bottom() as f64..=rect.top() as f64
            ) as f32
        };
        
        // --- Helper to map note to X-coord ---
        let map_note_to_x_range = |note: u8| -> (f32, f32) {
            let x_start = egui::remap(
                note as f64, 
                PIANO_LOW_NOTE as f64..=(PIANO_HIGH_NOTE + 1) as f64, 
                rect.left() as f64..=rect.right() as f64
            ) as f32;
            (x_start, x_start + key_width)
        };
        
        // Draw Finished Notes
        for note in &self.shared_state.finished_notes_display {
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
        for note in self.shared_state.currently_playing_notes.values() {
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
}
