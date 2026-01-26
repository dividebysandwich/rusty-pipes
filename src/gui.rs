use anyhow::Result;
use eframe::{egui, App, Frame};
use egui::{Stroke, UiBuilder};
use midir::MidiInputConnection;
use rust_i18n::t;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    path::PathBuf,
    sync::{mpsc::Sender, Arc, Mutex},
};

use crate::{
    app::MainLoopAction,
    app::{AppMessage, TuiMessage},
    app_state::{AppState, Preset},
    config::MidiEventSpec, // Import the new Enum
    gui_midi_learn::{draw_midi_learn_modal, LearnTarget, MidiLearnState},
    gui_organ_manager::OrganManagerUi,
    input::MusicCommand,
    organ::Organ,
};

#[allow(dead_code)]
pub struct EguiApp {
    app_state: Arc<Mutex<AppState>>,
    audio_tx: Sender<AppMessage>,
    tui_tx: Sender<TuiMessage>,

    // Need to hold the connection to keep it alive
    _midi_connection: Vec<MidiInputConnection<()>>,

    // --- GUI-specific state ---
    selected_stop_index: Option<usize>,
    stop_list_scroll_offset: f32,
    selection_changed_by_key: bool,

    last_mouse_move_repaint: Instant,

    show_preset_save_modal: bool,
    preset_save_slot: usize,
    preset_save_name: String,
    reverb_files: Vec<(String, PathBuf)>,
    selected_reverb_index: Option<usize>,
    midi_learn_state: MidiLearnState,
    show_lcd_config_modal: bool,

    // Organ Manager
    organ_manager: OrganManagerUi,
    exit_action: Arc<Mutex<MainLoopAction>>,
    gui_is_running: Arc<AtomicBool>,
}

/// Runs the main GUI loop.
pub fn run_gui_loop(
    audio_tx: Sender<AppMessage>,
    tui_tx: Sender<TuiMessage>,
    app_state: Arc<Mutex<AppState>>,
    organ: Arc<Organ>,
    midi_connection: Vec<MidiInputConnection<()>>,
    gui_ctx_tx: Sender<egui::Context>,
    reverb_files: Vec<(String, PathBuf)>,
    initial_ir_file: Option<PathBuf>,
    initial_mix: f32,
    gui_is_running: Arc<AtomicBool>,
    exit_action: Arc<Mutex<MainLoopAction>>,
) -> Result<MainLoopAction> {
    let selected_stop_index = if !organ.stops.is_empty() {
        Some(0)
    } else {
        None
    };

    let selected_reverb_index = initial_ir_file
        .as_ref()
        .and_then(|path| reverb_files.iter().position(|(_, p)| p == path));

    // Limit scope to reduce lock time
    {
        let mut state = app_state.lock().unwrap();
        state.reverb_mix = initial_mix;
        state.selected_reverb_index = selected_reverb_index;
    }

    let egui_app = EguiApp {
        app_state,
        audio_tx,
        tui_tx,
        _midi_connection: midi_connection, // Store the connection
        selected_stop_index,
        stop_list_scroll_offset: 0.0,
        selection_changed_by_key: false,
        last_mouse_move_repaint: Instant::now(),
        show_preset_save_modal: false,
        preset_save_slot: 0,
        preset_save_name: String::new(),
        reverb_files,
        selected_reverb_index,
        midi_learn_state: MidiLearnState::default(),
        show_lcd_config_modal: false,
        organ_manager: OrganManagerUi::new(),
        exit_action: exit_action.clone(),
        gui_is_running,
    };

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 600.0]),
        ..Default::default()
    };

    let window_title = t!("gui.app_title_fmt", name = organ.name);

    eframe::run_native(
        &window_title,
        native_options,
        Box::new(move |cc| {
            // Extract the Context and send it back to main logic thread
            let _ = gui_ctx_tx.send(cc.egui_ctx.clone());

            Ok::<Box<dyn App>, Box<dyn std::error::Error + Send + Sync>>(Box::new(egui_app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("Eframe error: {}", e))?;

    let action = exit_action.lock().unwrap().clone();
    Ok(action)
}

const MOUSE_DEBOUNCE_DELAY: Duration = Duration::from_millis(100);

impl App for EguiApp {
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn update(&mut self, ctx: &egui::Context, _frame: &mut Frame) {
        // Mouse Debouncing Logic
        let input = ctx.input(|i| i.clone());
        // Check if the pointer exists (hovering) AND if it moved (delta is non-zero)
        let mouse_moved =
            input.pointer.hover_pos().is_some() && input.pointer.delta() != egui::Vec2::ZERO;

        if mouse_moved {
            let now = Instant::now();
            if now.duration_since(self.last_mouse_move_repaint) >= MOUSE_DEBOUNCE_DELAY {
                // Repaint immediately if debounce time has passed
                ctx.request_repaint();
                self.last_mouse_move_repaint = now;
            } else {
                // If debounce time hasn't passed, ask to repaint after the remaining time
                let remaining_time = MOUSE_DEBOUNCE_DELAY
                    .saturating_sub(now.duration_since(self.last_mouse_move_repaint));
                ctx.request_repaint_after(remaining_time);
            }
        }

        let (organ, stop_channels, midi_log, presets, active_notes) = {
            let app_state = self.app_state.lock().unwrap();
            (
                app_state.organ.clone(),
                app_state.stop_channels.clone(),
                app_state.midi_log.clone(),
                app_state.presets.clone(),
                app_state.active_midi_notes.clone(),
            )
        };

        let mut active_notes_display: HashMap<u8, Vec<u8>> = HashMap::new();

        // The key is now a tuple: (_key_channel, note)
        // We ignore the key_channel here because we can get it from played_note.channel,
        // or just use the one from the tuple.
        for ((_key_channel, note), played_note) in &active_notes {
            active_notes_display
                .entry(*note)
                .or_default()
                .push(played_note.channel);
        }

        // Sort channels to ensure consistent color stacking (e.g. Channel 1 always above Channel 3)
        for channels in active_notes_display.values_mut() {
            channels.sort();
        }

        if !self.show_preset_save_modal {
            let input = ctx.input(|i| i.clone());

            // --- Keyboard Music Input---
            for event in &input.events {
                if let egui::Event::Key {
                    key,
                    pressed,
                    repeat,
                    ..
                } = event
                {
                    if *repeat {
                        continue;
                    } // Ignore key repeats for music

                    // Map the key
                    let command = {
                        let state = self.app_state.lock().unwrap();
                        state.keyboard_layout.map_egui(*key)
                    };

                    // Execute
                    match command {
                        MusicCommand::OctaveUp if *pressed => {
                            self.app_state.lock().unwrap().octave_offset += 1;
                        }
                        MusicCommand::OctaveDown if *pressed => {
                            self.app_state.lock().unwrap().octave_offset -= 1;
                        }
                        MusicCommand::PlayNote(semitone) => {
                            let mut state = self.app_state.lock().unwrap();
                            let note = state.get_keyboard_midi_note(semitone);
                            let velocity = if *pressed { 100 } else { 0 };

                            state.handle_keyboard_note(note, velocity, &self.audio_tx);
                        }
                        _ => {} // Ignore None or non-music keys
                    }
                }
            }

            // --- Function Keys ---

            let function_keys = [
                egui::Key::F1,
                egui::Key::F2,
                egui::Key::F3,
                egui::Key::F4,
                egui::Key::F5,
                egui::Key::F6,
                egui::Key::F7,
                egui::Key::F8,
                egui::Key::F9,
                egui::Key::F10,
                egui::Key::F11,
                egui::Key::F12,
            ];
            for (i, &key) in function_keys.iter().enumerate() {
                if input.key_pressed(key) {
                    if input.modifiers.shift {
                        self.preset_save_slot = i;
                        self.preset_save_name = presets[i].as_ref().map_or_else(
                            || t!("gui.default_preset_name_fmt", num = i + 1).to_string(),
                            |p| p.name.clone(),
                        );
                        self.show_preset_save_modal = true;
                    } else {
                        let mut app_state = self.app_state.lock().unwrap();
                        if let Err(e) = app_state.recall_preset(i, &self.audio_tx) {
                            app_state
                                .add_midi_log(t!("errors.recall_preset_fail", err = e).to_string());
                        }
                    }
                }
            }
            // Gain: + / -
            if input.key_pressed(egui::Key::Plus) || input.key_pressed(egui::Key::Equals) {
                self.app_state
                    .lock()
                    .unwrap()
                    .modify_gain(0.05, &self.audio_tx);
            }
            if input.key_pressed(egui::Key::Minus) {
                self.app_state
                    .lock()
                    .unwrap()
                    .modify_gain(-0.05, &self.audio_tx);
            }

            // Polyphony: [ / ]
            if input.key_pressed(egui::Key::OpenBracket) {
                self.app_state
                    .lock()
                    .unwrap()
                    .modify_polyphony(-16, &self.audio_tx);
            }
            if input.key_pressed(egui::Key::CloseBracket) {
                self.app_state
                    .lock()
                    .unwrap()
                    .modify_polyphony(16, &self.audio_tx);
            }
            // Panic key: P
            if input.key_pressed(egui::Key::P) {
                self.audio_tx
                    .send(AppMessage::AllNotesOff)
                    .unwrap_or_else(|e| {
                        log::error!("ERROR sending AllNotesOff: {}", e);
                    });
            }

            // Arrow key navigation
            if !organ.stops.is_empty() {
                let num_stops = organ.stops.len();
                // Default to 0 if nothing is selected, to allow keys to "start" selection
                let mut current_idx = self.selected_stop_index.unwrap_or(0);
                let mut changed = false;

                let num_cols = 3;
                let items_per_column = (num_stops + num_cols - 1) / num_cols;

                if input.key_pressed(egui::Key::ArrowDown) {
                    current_idx = (current_idx + 1) % num_stops;
                    changed = true;
                } else if input.key_pressed(egui::Key::ArrowUp) {
                    current_idx = (current_idx + num_stops - 1) % num_stops;
                    changed = true;
                } else if input.key_pressed(egui::Key::ArrowLeft) {
                    current_idx = current_idx.saturating_sub(items_per_column);
                    changed = true;
                } else if input.key_pressed(egui::Key::ArrowRight) {
                    // Move to the next column (clamps to last item)
                    let new_idx = current_idx + items_per_column;
                    current_idx = new_idx.min(num_stops - 1);
                    changed = true;
                }

                if changed {
                    self.selected_stop_index = Some(current_idx);
                    self.selection_changed_by_key = true; // Flag for scroll view
                }
            }

            // Number key toggling (for selected stop)
            if let Some(stop_idx) = self.selected_stop_index {
                let mut channel_to_toggle: Option<u8> = None;

                if input.key_pressed(egui::Key::Num1) {
                    channel_to_toggle = Some(0);
                }
                if input.key_pressed(egui::Key::Num2) {
                    channel_to_toggle = Some(1);
                }
                if input.key_pressed(egui::Key::Num3) {
                    channel_to_toggle = Some(2);
                }
                if input.key_pressed(egui::Key::Num4) {
                    channel_to_toggle = Some(3);
                }
                if input.key_pressed(egui::Key::Num5) {
                    channel_to_toggle = Some(4);
                }
                if input.key_pressed(egui::Key::Num6) {
                    channel_to_toggle = Some(5);
                }
                if input.key_pressed(egui::Key::Num7) {
                    channel_to_toggle = Some(6);
                }
                if input.key_pressed(egui::Key::Num8) {
                    channel_to_toggle = Some(7);
                }
                if input.key_pressed(egui::Key::Num9) {
                    channel_to_toggle = Some(8);
                }
                if input.key_pressed(egui::Key::Num0) {
                    channel_to_toggle = Some(9);
                }
                if let Some(channel) = channel_to_toggle {
                    // Replicate the toggle logic from the button click
                    let mut app_state = self.app_state.lock().unwrap();
                    if let Err(e) = app_state.toggle_stop_channel(stop_idx, channel, &self.audio_tx)
                    {
                        app_state.add_midi_log(format!("ERROR: {}", e));
                    }
                }
            }
        }

        // Draw the UI (no more mode switching)
        self.draw_main_app_ui(
            ctx,
            &midi_log,
            &active_notes_display,
            organ.clone(),
            stop_channels.clone(),
            &presets,
        );

        // Draw modals if needed
        self.draw_preset_save_modal(ctx);
        draw_midi_learn_modal(ctx, self.app_state.clone(), &mut self.midi_learn_state);

        self.organ_manager
            .show(ctx, &self.exit_action, self.app_state.clone());

        // Organ Switching via Trigger
        if !self.organ_manager.visible && !self.midi_learn_state.is_open {
            // Check for any recent MIDI event (SysEx or Note) that matches a known organ trigger
            let event_opt = {
                let mut state = self.app_state.lock().unwrap();
                let evt = state.last_midi_event_received.take();
                // Also check for legacy SysEx field just in case
                let legacy_sysex = state.last_sysex.take();
                (evt, legacy_sysex)
            };

            let current_name = &self.app_state.lock().unwrap().organ.name;

            if let (Some((event, _time)), _) = event_opt {
                if self.organ_manager.is_learning() {
                    // We are learning inside the organ manager UI
                } else {
                    // Not learning, check if this triggers an organ load
                    if let Some(path) = self
                        .organ_manager
                        .find_organ_by_trigger(&event, current_name)
                    {
                        *self.exit_action.lock().unwrap() =
                            MainLoopAction::ReloadOrgan { file: path };
                        self.gui_is_running.store(false, Ordering::SeqCst);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            } else if let (_, Some(sysex)) = event_opt {
                // Fallback for SysEx if it came through the old path (unlikely with new code, but safe)
                let event = MidiEventSpec::SysEx(sysex);
                if let Some(path) = self
                    .organ_manager
                    .find_organ_by_trigger(&event, current_name)
                {
                    *self.exit_action.lock().unwrap() = MainLoopAction::ReloadOrgan { file: path };
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    // Handle quit request
    fn on_exit(&mut self, _glow_ctx: Option<&eframe::glow::Context>) {
        self.gui_is_running.store(false, Ordering::SeqCst);
        if let Err(e) = self.audio_tx.send(AppMessage::Quit) {
            eprintln!("Failed to send Quit message on close: {}", e);
        }
    }
}

// --- UI Drawing Implementations ---

impl EguiApp {
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn draw_main_app_ui(
        &mut self,
        ctx: &egui::Context,
        midi_log: &VecDeque<std::string::String>,
        active_notes: &HashMap<u8, Vec<u8>>,
        organ: Arc<Organ>,
        stop_channels: HashMap<usize, BTreeSet<u8>>,
        presets: &[std::option::Option<Preset>; 12],
    ) {
        self.draw_footer(ctx);
        self.draw_preset_panel(ctx, presets, organ.clone());
        self.draw_log_and_midi_indicator_panel(ctx, midi_log, active_notes);

        // Determine background based on theme
        let is_dark = ctx.style().visuals.dark_mode;
        let panel_fill = if is_dark {
            egui::Color32::from_rgb(30, 30, 30) // Dark gray
        } else {
            ctx.style().visuals.panel_fill // Standard light mode background
        };

        let panel_frame = egui::Frame {
            fill: panel_fill,
            inner_margin: egui::Margin::same(10),
            ..Default::default()
        };

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                ui.heading(egui::RichText::new(organ.name.clone()).heading().strong());
                ui.separator();

                self.draw_stop_controls(ui, organ.clone());

                ui.separator();

                let scroll = egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .vertical_scroll_offset(self.stop_list_scroll_offset);

                let scroll_out = scroll.show(ui, |ui| {
                    self.draw_stop_list_columns(ui, organ.clone(), stop_channels.clone());
                });
                self.stop_list_scroll_offset = scroll_out.state.offset.y;
            });

        // LCD Modal
        if self.show_lcd_config_modal {
            self.draw_lcd_config_modal(ctx);
        }
    }

    fn draw_lcd_config_modal(&mut self, ctx: &egui::Context) {
        let mut open = self.show_lcd_config_modal;
        egui::Window::new("LCD Configuration")
            .open(&mut open)
            .show(ctx, |ui| {
                let mut state = self.app_state.lock().unwrap();
                let mut to_remove = None;

                if ui.button("Add Display").clicked() {
                    let next_id = state.lcd_displays.len() as u8 + 1;
                    use crate::config::{LcdColor, LcdDisplayConfig, LcdLineType};
                    state.lcd_displays.push(LcdDisplayConfig {
                        id: next_id,
                        line1: LcdLineType::OrganName,
                        line2: LcdLineType::SystemStatus,
                        background_color: LcdColor::White,
                    });
                    state.persist_settings();
                    state.refresh_lcds();
                }

                ui.separator();

                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .show(ui, |ui| {
                        for (i, display) in state.lcd_displays.iter_mut().enumerate() {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(format!("Display #{}", i + 1));
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
                                        ui.label("ID (Sysex ID):");
                                        ui.add(
                                            egui::DragValue::new(&mut display.id).range(1..=127),
                                        );
                                        ui.end_row();

                                        ui.label("Background:");
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

                                        ui.label("Line 1:");
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

                                        ui.label("Line 2:");
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
                    state.lcd_displays.remove(idx);
                    state.persist_settings();
                    state.refresh_lcds();
                }

                // Refresh on close or change is implicit if we persisted settings.
                // We persisted above on add/remove.
                // For direct field modification, we should probably persist on close or change.
                if ui.input(|i| i.pointer.any_released()) {
                    state.persist_settings();
                    state.refresh_lcds();
                }
            });
        self.show_lcd_config_modal = open;
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn draw_footer(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Theme Toggle Button
                let is_dark = ui.visuals().dark_mode;
                let theme_icon = if is_dark { "☀" } else { "🌙" };

                if ui.button(theme_icon).clicked() {
                    if is_dark {
                        ctx.set_visuals(egui::Visuals::light());
                    } else {
                        ctx.set_visuals(egui::Visuals::dark());
                    }
                }

                ui.separator();
                ui.separator();
                ui.label(t!("gui.footer_tip"));

                ui.separator();
                if ui.button(t!("organ_manager.button")).clicked() {
                    self.organ_manager.visible = true;
                }

                // Right-aligned controls
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(t!("gui.btn_lcd_config")).clicked() {
                        self.show_lcd_config_modal = true;
                    }
                    ui.add_space(5.0);
                    ui.separator();
                    ui.add_space(5.0);

                    let (is_rec_midi, is_rec_audio) = {
                        let state = self.app_state.lock().unwrap();
                        (state.is_recording_midi, state.is_recording_audio)
                    };

                    // Use theme's default widget background instead of fixed gray(60)
                    let default_btn_color = ui.visuals().widgets.inactive.bg_fill;

                    // Audio Record Button
                    let audio_btn_text = if is_rec_audio {
                        t!("gui.rec_wav_stop")
                    } else {
                        t!("gui.rec_wav_start")
                    };
                    let audio_btn = egui::Button::new(audio_btn_text).fill(if is_rec_audio {
                        egui::Color32::RED
                    } else {
                        default_btn_color
                    });

                    if ui.add(audio_btn).clicked() {
                        let new_state = !is_rec_audio;
                        self.app_state.lock().unwrap().is_recording_audio = new_state;
                        let _ = self.audio_tx.send(if new_state {
                            AppMessage::StartAudioRecording
                        } else {
                            AppMessage::StopAudioRecording
                        });
                    }

                    ui.add_space(5.0);

                    // MIDI Record Button
                    let midi_btn_text = if is_rec_midi {
                        t!("gui.rec_midi_stop")
                    } else {
                        t!("gui.rec_midi_start")
                    };
                    let midi_btn = egui::Button::new(midi_btn_text).fill(if is_rec_midi {
                        egui::Color32::RED
                    } else {
                        default_btn_color
                    });

                    if ui.add(midi_btn).clicked() {
                        let new_state = !is_rec_midi;
                        self.app_state.lock().unwrap().is_recording_midi = new_state;
                        let _ = self.audio_tx.send(if new_state {
                            AppMessage::StartMidiRecording
                        } else {
                            AppMessage::StopMidiRecording
                        });
                    }

                    if is_rec_midi || is_rec_audio {
                        ui.add_space(5.0);
                        ui.label(
                            egui::RichText::new(t!("gui.recording_active"))
                                .color(egui::Color32::RED)
                                .strong(),
                        );
                    }
                });
            });
        });
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn draw_preset_panel(
        &mut self,
        ctx: &egui::Context,
        presets: &[std::option::Option<Preset>; 12],
        organ: Arc<Organ>,
    ) {
        egui::SidePanel::right("preset_panel")
            .default_width(250.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("sidebar_main_scroll") // Unique ID helps egui track scroll position
                    .show(ui, |ui| {
                        ui.heading(t!("gui.presets_heading"));
                        ui.add_space(5.0);

                        let spacing = ui.spacing().item_spacing.x;
                        let btn_size = egui::vec2(
                            (ui.available_width() - spacing) / 2.0, 
                            20.0
                        );

                        ui.label(t!("gui.recall_label"));

                        egui::Grid::new("preset_recall_grid")
                            .num_columns(2)
                            .spacing([spacing, 5.0])
                            .show(ui, |ui| {
                                for i in 0..12 {
                                    let (text, is_loaded) = presets[i].as_ref().map_or_else(
                                        || (format!("F{}", i + 1), false),
                                        |p| (format!("F{}: {}", i + 1, p.name.clone()), true),
                                    );

                                    let btn = ui.add_sized(btn_size, egui::Button::new(text));
                                    
                                    // Left Click: Recall
                                    if btn.clicked() {
                                        if is_loaded {
                                            let mut app_state = self.app_state.lock().unwrap();
                                            if let Err(e) = app_state.recall_preset(i, &self.audio_tx) {
                                                app_state.add_midi_log(
                                                    t!("errors.recall_preset_fail", err = e).to_string(),
                                                );
                                            }
                                        }
                                    }
                                    
                                    // Right Click: Learn
                                    if btn.secondary_clicked() {
                                        self.midi_learn_state.is_open = true;
                                        self.midi_learn_state.target = LearnTarget::Preset(i);
                                        self.midi_learn_state.target_name = format!("Preset F{}", i + 1);
                                        self.midi_learn_state.learning_slot = None;
                                    }
                                    
                                    if (i + 1) % 2 == 0 {
                                        ui.end_row();
                                    }
                                }
                            });
                
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(10.0);

                        ui.label(t!("gui.save_label"));
                        egui::Grid::new("preset_save_grid")
                            .num_columns(2)
                            .spacing([spacing, 5.0])
                            .show(ui, |ui| {
                                for i in 0..12 {
                                    let text = format!("F{}", i + 1);
                                    if ui.add_sized(btn_size, egui::Button::new(text)).clicked() {
                                        self.preset_save_slot = i;
                                        self.preset_save_name = presets[i].as_ref().map_or_else(
                                            || t!("gui.default_preset_name_fmt", num = i + 1).to_string(),
                                            |p| p.name.clone(),
                                        );
                                        self.show_preset_save_modal = true;
                                    }
                                    if (i + 1) % 2 == 0 {
                                        ui.end_row();
                                    }
                                }
                            });

                        ui.separator();
                        ui.heading(t!("gui.tremulants_heading"));
                        ui.add_space(5.0);

                        let mut trem_ids: Vec<_> = organ.tremulants.keys().collect();
                        trem_ids.sort();

                        if trem_ids.is_empty() {
                            ui.label(egui::RichText::new(t!("gui.no_tremulants")).weak());
                        } else {
                            egui::Grid::new("tremulant_grid")
                                .num_columns(2)
                                .spacing([spacing, 5.0])
                                .show(ui, |ui| {
                                    for (i, trem_id) in trem_ids.iter().enumerate() {
                                        let trem = &organ.tremulants[*trem_id];
                                        let is_active = self
                                            .app_state
                                            .lock()
                                            .unwrap()
                                            .active_tremulants
                                            .contains(*trem_id);

                                        let button_text = if is_active {
                                            egui::RichText::new(&trem.name).color(egui::Color32::GREEN)
                                        } else {
                                            egui::RichText::new(&trem.name)
                                        };

                                        let btn = ui.add_sized(btn_size, egui::Button::new(button_text));
                                        
                                        // Left Click: Toggle
                                        if btn.clicked() {
                                            let mut state = self.app_state.lock().unwrap();
                                            state.set_tremulant_active(
                                                trem_id.to_string(),
                                                !is_active,
                                                &self.audio_tx,
                                            );
                                        }
                                        
                                        // Right Click: Learn
                                        if btn.secondary_clicked() {
                                            self.midi_learn_state.is_open = true;
                                            self.midi_learn_state.target = LearnTarget::Tremulant(trem_id.to_string());
                                            self.midi_learn_state.target_name = trem.name.clone();
                                            self.midi_learn_state.learning_slot = None;
                                        }
                                        
                                        if (i + 1) % 2 == 0 {
                                            ui.end_row();
                                        }
                                    }
                                });
                        }
                    
                        ui.separator();
                        ui.heading(t!("gui.audio_settings_heading"));
                        ui.add_space(5.0);

                        // Get current values
                        let (mut gain, polyphony, selected_reverb_index, mut reverb_mix) = {
                            let state = self.app_state.lock().unwrap();
                        (state.gain, state.polyphony, state.selected_reverb_index, state.reverb_mix)
                        };

                        ui.label(t!("gui.reverb_label"));
                        let current_name: String = selected_reverb_index
                            .and_then(|i| self.reverb_files.get(i))
                            .map(|(n, _)| n.clone())
                            .unwrap_or_else(|| t!("gui.no_reverb").to_string());

                        egui::ComboBox::from_id_salt("runtime_reverb_combo")
                            .width(ui.available_width()) 
                            .selected_text(current_name)
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(selected_reverb_index.is_none(), t!("gui.no_reverb")).clicked() {
                                    let _ = self.audio_tx.send(AppMessage::SetReverbWetDry(0.0));
                                    let mut state = self.app_state.lock().unwrap();
                                    state.selected_reverb_index = None;
                                    state.reverb_mix = 0.0;
                                    state.persist_settings();
                                }

                                for (i, (name, path)) in self.reverb_files.iter().enumerate() {
                                    if ui.selectable_label(selected_reverb_index == Some(i), name).clicked() {
                                        let _ = self.audio_tx.send(AppMessage::SetReverbIr(path.clone()));
                                        let mut state = self.app_state.lock().unwrap();
                                        state.selected_reverb_index = Some(i);
                                        state.persist_settings();
                                    }
                                }
                            });

                        ui.add_space(10.0);

                        // Update the style locally for this frame.
                        // Subtract ~50px to leave room for the value text (e.g. "1.00").
                        ui.spacing_mut().slider_width = ui.available_width() - 50.0;

                        ui.label(t!("gui.reverb_mix_label"));
                        if ui.add(egui::Slider::new(&mut reverb_mix, 0.0..=1.0).show_value(true)).changed() {
                            let mut state = self.app_state.lock().unwrap();
                            state.reverb_mix = reverb_mix;
                            let _ = self.audio_tx.send(AppMessage::SetReverbWetDry(reverb_mix));
                            state.persist_settings();
                        }

                        ui.add_space(10.0);

                        ui.label(t!("gui.master_gain_label"));
                        if ui.add(egui::Slider::new(&mut gain, 0.0..=2.0).show_value(true)).changed() {
                            let mut state = self.app_state.lock().unwrap();
                            state.gain = gain;
                            let _ = self.audio_tx.send(AppMessage::SetGain(gain));
                            state.persist_settings();
                        }

                        ui.label(egui::RichText::new(t!("gui.gain_keys_hint")).small().weak());

                        ui.add_space(10.0);

                        // --- Polyphony Control ---
                        ui.horizontal(|ui| {
                            // Label on the left, with toolip
                            ui.label(t!("gui.polyphony_label"))
                                .on_hover_text(t!("gui.polyphony_keys_hint"));

                            // Push everything else to the right
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let poly_btn_size = egui::vec2(25.0, 20.0);
                                if ui.add_sized(poly_btn_size, egui::Button::new("+")).clicked() {
                                    self.app_state.lock().unwrap().modify_polyphony(16, &self.audio_tx);
                                }
                                ui.label(egui::RichText::new(format!("{}", polyphony)).strong());
                                if ui.add_sized(poly_btn_size, egui::Button::new("-")).clicked() {
                                    self.app_state.lock().unwrap().modify_polyphony(-16, &self.audio_tx);
                                }
                            });
                        });

                        ui.separator();
                
                        // --- MIDI File Player Section ---
                        ui.heading(t!("gui.midi_player_label"));
                        ui.add_space(5.0);

                        let (is_playing, has_file, filename_display, progress, curr_sec, total_sec) = {
                            let state = self.app_state.lock().unwrap();
                            let raw_name = state.midi_file_path.as_ref()
                                .and_then(|p| p.file_name())
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| t!("gui.midi_player_nofile").to_string());
                            
                            let display = if raw_name.len() > 38 {
                                format!("{}...{}", &raw_name[0..12], &raw_name[raw_name.len()-14..])
                            } else {
                                raw_name
                            };
                            
                            (state.is_midi_file_playing, state.midi_file_path.is_some(), display, state.midi_playback_progress, state.midi_current_time_secs, state.midi_total_time_secs)
                        };

                        // Button row
                        ui.horizontal(|ui| {
                            let spacing = ui.spacing().item_spacing.x;
                            let num_buttons = 4.0;
                            // Calculate precise width for even buttons
                            let btn_width = (ui.available_width() - ((num_buttons - 1.0) * spacing)) / num_buttons; 
                            let ctrl_btn_size = egui::vec2(btn_width, 25.0); 

                            // Open file browser
                            if ui.add_sized(ctrl_btn_size, egui::Button::new("📂")).on_hover_text(t!("config.picker_midi")).clicked() {
                                if let Some(path) = rfd::FileDialog::new().add_filter("MIDI", &["mid", "midi"]).pick_file() {
                                    let mut state = self.app_state.lock().unwrap();
                                    state.midi_file_path = Some(path);
                                    if state.is_midi_file_playing {
                                        state.midi_file_stop_signal.store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                    state.midi_playback_progress = 0.0;
                                    state.midi_current_time_secs = 0;
                                    state.midi_total_time_secs = 0;
                                }
                            }

                            // Rewind button
                            if ui.add_enabled_ui(is_playing, |ui| {
                                ui.add_sized(ctrl_btn_size, egui::Button::new("⏪"))
                            }).inner.on_hover_text(t!("gui.midi_player_rewind")).clicked() {
                                let state = self.app_state.lock().unwrap();
                                let _ = self.audio_tx.send(AppMessage::AllNotesOff);
                                if let Some(tx) = &state.midi_seek_tx {
                                    let _ = tx.send(-15);
                                }
                            }

                            // Play / Stop button
                            if is_playing {
                                if ui.add_sized(ctrl_btn_size, egui::Button::new("⏹").fill(egui::Color32::RED)).on_hover_text(t!("gui.midi_player_stop")).clicked() {
                                    let mut state = self.app_state.lock().unwrap();

                                    state.midi_file_stop_signal.store(true, std::sync::atomic::Ordering::Relaxed);
                                    state.is_midi_file_playing = false;
                                    state.handle_tui_all_notes_off();
                                    state.channel_active_notes.clear();
                                    let _ = self.audio_tx.send(AppMessage::AllNotesOff);
                                }
                            } else {
                                // We use add_enabled so the button looks disabled if no file is selected
                                ui.add_enabled_ui(has_file, |ui| {
                                    if ui.add_sized(ctrl_btn_size, egui::Button::new("▶")).on_hover_text(t!("gui.midi_player_play")).clicked() {
                                        let mut state = self.app_state.lock().unwrap();
                                        if let Some(path) = state.midi_file_path.clone() {
                                            state.is_midi_file_playing = true;
                                            state.midi_file_stop_signal.store(false, std::sync::atomic::Ordering::Relaxed);
                                            let stop_sig = state.midi_file_stop_signal.clone();
                                            let tui_tx_clone = self.tui_tx.clone();
                                            if let Err(e) = crate::midi::play_midi_file(path, tui_tx_clone, stop_sig) {
                                                state.add_midi_log(format!("Error playing MIDI: {}", e));
                                                state.is_midi_file_playing = false;
                                            }
                                        }
                                    }
                                });
                            }

                            // Seek foward button
                            if ui.add_enabled_ui(is_playing, |ui| {
                                ui.add_sized(ctrl_btn_size, egui::Button::new("⏩"))
                            }).inner.on_hover_text(t!("gui.midi_player_fastforward")).clicked() {
                                let state = self.app_state.lock().unwrap();
                                let _ = self.audio_tx.send(AppMessage::AllNotesOff);
                                if let Some(tx) = &state.midi_seek_tx {
                                    let _ = tx.send(15);
                                }
                            }
                        });

                        ui.add_space(5.0);
                        
                        // Progress bar
                        let progress_bar = egui::ProgressBar::new(progress)
                            .animate(false) // Animate if playing
                            .corner_radius(0.0)
                            .desired_height(15.0); // Slightly taller target

                        let response = ui.add(progress_bar);
                        
                        // Add interaction to the progress bar rect
                        let interact = ui.interact(response.rect, response.id, egui::Sense::click());
                        
                        // Change cursor to pointer to indicate clickability
                        if interact.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }

                        if interact.clicked() {
                            // Calculate click percentage
                            if let Some(hover_pos) = interact.hover_pos() {
                                let rect = response.rect;
                                // Clamp ensure we don't go out of bounds if clicked on the very edge
                                let ratio = ((hover_pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
                                
                                // Calculate target time in seconds
                                let target_time = ratio as f64 * total_sec as f64;
                                let current_time = curr_sec as f64;
                                
                                // Calculate delta (seconds to skip) needed by midi.rs
                                let delta = (target_time - current_time) as i32;

                                // Send command
                                let state = self.app_state.lock().unwrap();
                                // Silence notes before jumping
                                let _ = self.audio_tx.send(AppMessage::AllNotesOff);
                                
                                if let Some(tx) = &state.midi_seek_tx {
                                    // midi.rs handles the math: new_time = current + delta
                                    let _ = tx.send(delta);
                                }
                            }
                        }

                        // Time & Filename Display
                        let format_time = |s: u32| format!("{:02}:{:02}", s / 60, s % 60);
                        ui.add_space(2.0);
                        ui.vertical_centered(|ui| {
                            // Time
                            ui.label(egui::RichText::new(format!("{} / {}", format_time(curr_sec), format_time(total_sec)))
                            .small().strong());
                            // Filename
                            ui.label(egui::RichText::new(filename_display).weak().italics());
                        });

                        ui.separator();


                        // --- Underrun Indicator ---
                        let is_underrun = {
                            let state = self.app_state.lock().unwrap();
                            if let Some(last) = state.last_underrun {
                            // Light up if underrun happened in the last 200ms
                                last.elapsed() < Duration::from_millis(200)
                            } else {
                                false
                            }
                        };

                        let (active_voice_count, polyphony, cpu_load) = {
                            let state = self.app_state.lock().unwrap();
                            (state.active_voice_count, state.polyphony, state.cpu_load)
                        };

                        let status_btn_size = egui::vec2(ui.available_width(), 30.0);

                        if is_underrun {
                            ui.add_sized(status_btn_size, egui::Button::new(
                            egui::RichText::new(t!("gui.underrun_alert")).color(egui::Color32::WHITE).strong(),
                            ).fill(egui::Color32::RED));
                        } else {
                            // Use the theme's faint background or noninteractive fill
                            let bg = ui.visuals().widgets.noninteractive.bg_fill;
                            ui.add_sized(status_btn_size, egui::Button::new(
                                egui::RichText::new(t!("gui.voices_fmt", voices = active_voice_count, poly = polyphony)).strong(),
                            ).fill(bg).frame(true)); 
                        }

                        ui.add_space(5.0);
                        ui.separator();

                        // --- CPU Load Bar ---
                        ui.label(t!("gui.cpu_load_fmt", load = format!("{:.1}", cpu_load * 100.0)));

                        let load_color = if cpu_load < 0.5 {
                            egui::Color32::GREEN
                        } else if cpu_load < 0.9 {
                            egui::Color32::YELLOW
                        } else {
                            egui::Color32::RED
                        };

                        ui.add(egui::ProgressBar::new(cpu_load).fill(load_color).animate(false));
                    }
                );
            });
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn draw_stop_controls(&mut self, ui: &mut egui::Ui, organ: Arc<Organ>) {
        ui.horizontal(|ui| {
            ui.label(t!("gui.selected_stop_label"));
            if let Some(idx) = self.selected_stop_index {
                let stop = &organ.stops[idx];
                ui.label(egui::RichText::new(&stop.name).strong());

                if ui.button(t!("gui.btn_all_channels")).clicked() {
                    let mut app_state = self.app_state.lock().unwrap();
                    app_state.select_all_channels_for_stop(idx);
                }
                if ui.button(t!("gui.btn_no_channels")).clicked() {
                    let mut app_state = self.app_state.lock().unwrap();
                    if let Err(e) = app_state.select_none_channels_for_stop(idx, &self.audio_tx) {
                        app_state.add_midi_log(format!("ERROR: {}", e));
                    }
                }
            } else {
                ui.label(egui::RichText::new(t!("gui.no_selection")).italics());
            }

            ui.separator();

            if ui
                .button(t!("gui.btn_panic"))
                .on_hover_text(t!("gui.panic_tooltip"))
                .clicked()
            {
                let mut app_state = self.app_state.lock().unwrap();
                if let Err(e) = self.audio_tx.send(AppMessage::AllNotesOff) {
                    app_state.add_midi_log(format!("ERROR: {}", e));
                }
            }
        });
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn draw_stop_list_columns(
        &mut self,
        ui: &mut egui::Ui,
        organ: Arc<Organ>,
        stop_channels: HashMap<usize, BTreeSet<u8>>,
    ) {
        let num_cols = 3;
        let stops: Vec<_> = organ.stops.clone();
        let stops_count = stops.len();
        if stops_count == 0 {
            ui.label(t!("gui.no_stops_loaded"));
            return;
        }

        // Theme-aware colors for text and backgrounds
        let visuals = ui.visuals().clone();
        let active_text_color = if visuals.dark_mode {
            egui::Color32::from_rgb(100, 255, 100) // Bright green for dark mode
        } else {
            egui::Color32::from_rgb(0, 120, 0) // Dark forest green for light mode
        };

        // Calculate how many stops go in each column
        let items_per_column = (stops_count + num_cols - 1) / num_cols;

        // Create 3 resizable layout columns
        ui.columns(num_cols, |cols| {
            for (col_idx, ui) in cols.iter_mut().enumerate() {
                // Calculate the range of stops for this specific column
                let start_idx = col_idx * items_per_column;
                let end_idx = (start_idx + items_per_column).min(stops_count);

                if start_idx >= end_idx {
                    continue;
                }

                // Render all stops for this column
                for i in start_idx..end_idx {
                    let is_selected = self.selected_stop_index == Some(i);
                    let active_channels = stop_channels.get(&i).cloned().unwrap_or_default();
                    let is_active = !active_channels.is_empty();
                    let stop = &stops[i];

                    ui.vertical(|ui| {
                        // --- Stop Name ---
                        let mut label_text = egui::RichText::new(&stop.name)
                            .size(18.0)
                            .heading()
                            .strong();

                        if is_active && !is_selected {
                            label_text = label_text.color(active_text_color);
                        }

                        let response = ui.selectable_label(is_selected, label_text);
                        // ... (interaction logic remains the same)
                        if response.clicked() {
                            self.selected_stop_index = Some(i);
                            // User clicked, don't auto-scroll
                            self.selection_changed_by_key = false;
                            self.midi_learn_state.is_open = true;
                            self.midi_learn_state.target = LearnTarget::Stop(i); // Use Enum
                            self.midi_learn_state.target_name = stop.name.clone(); // Use target_name
                            self.midi_learn_state.learning_slot = None;
                        }

                        // Auto-scroll if selection changed by key
                        if is_selected && self.selection_changed_by_key {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }

                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            let available_width = ui.available_width();
                            let spacing = ui.spacing().item_spacing.x;

                            // We want to fit 16 buttons.
                            // Let's assume a minimum comfortable button width (e.g., 20px) plus spacing.
                            // 16 buttons * 20px + 15 spaces * 4px ≈ 380px.
                            // If we have less than that, we wrap to 2 rows.

                            // Calculate required width for 16 buttons in one row
                            let min_btn_width = 20.0;
                            let required_width_single_row =
                                (16.0 * min_btn_width) + (18.0 * spacing);
                            let is_wide = available_width > (required_width_single_row + 10.0);
                            let buttons_per_row = if is_wide { 16.0 } else { 8.0 };
                            let btn_width = (available_width - ((buttons_per_row - 1.0) * spacing))
                                / buttons_per_row;
                            let btn_size = egui::vec2(btn_width, 20.0);

                            let draw_btn = |ui: &mut egui::Ui, chan: u8| {
                                let is_on = active_channels.contains(&chan);
                                let text = egui::RichText::new((chan + 1).to_string()).strong();

                                // Button will automatically use theme primary colors when .selected(true)
                                if ui
                                    .add_sized(btn_size, egui::Button::new(text).selected(is_on))
                                    .clicked()
                                {
                                    let mut app_state = self.app_state.lock().unwrap();
                                    if let Err(e) =
                                        app_state.toggle_stop_channel(i, chan, &self.audio_tx)
                                    {
                                        app_state.add_midi_log(format!("ERROR: {}", e));
                                    }
                                }
                            };

                            if is_wide {
                                // Wide screen: One single row of 16 buttons (No gap in middle)
                                ui.horizontal(|ui| {
                                    for c in 0..16 {
                                        draw_btn(ui, c);
                                    }
                                });
                            } else {
                                // Narrow screen: Two stacked rows of 8 buttons
                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        for c in 0..8 {
                                            draw_btn(ui, c);
                                        }
                                    });
                                    ui.horizontal(|ui| {
                                        for c in 8..16 {
                                            draw_btn(ui, c);
                                        }
                                    });
                                });
                            }
                        });
                    });
                    ui.add_space(4.0);
                }
            }
        });

        // Reset the key-scroll flag after drawing
        self.selection_changed_by_key = false;
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn draw_log_and_midi_indicator_panel(
        &mut self,
        ctx: &egui::Context,
        midi_log: &std::collections::VecDeque<String>,
        active_notes: &HashMap<u8, Vec<u8>>,
    ) {
        const LOG_WIDTH: f32 = 300.0;
        let visuals = ctx.style().visuals.clone();

        egui::TopBottomPanel::bottom("bottom_panel")
            .resizable(true)
            .default_height(100.0)
            .show(ctx, |ui| {
                let full_rect = ui.available_rect_before_wrap();
                let split_x = (full_rect.left() + LOG_WIDTH).min(full_rect.right());
                let (log_rect, indicator_rect) = full_rect.split_left_right_at_x(split_x);

                // Column 0: Log
                ui.scope_builder(
                    UiBuilder {
                        max_rect: Some(log_rect),
                        ..Default::default()
                    },
                    |ui| {
                        // Use the theme's 'extreme' background for the log area to make it look like a terminal/text box
                        egui::Frame::canvas(ui.style())
                            .fill(visuals.panel_fill)
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .stick_to_bottom(true)
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        for msg in midi_log {
                                            ui.label(egui::RichText::new(msg));
                                        }
                                    });
                            });
                    },
                );

                // MIDI Activity Indicator Area
                ui.scope_builder(
                    UiBuilder {
                        max_rect: Some(indicator_rect),
                        ..Default::default()
                    },
                    |ui| {
                        ui.vertical(|ui| {
                            ui.heading(t!("gui.midi_activity_heading"));
                            self.draw_midi_indicator(ui, active_notes);
                        });
                    },
                );
            });
    }

    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn draw_midi_indicator(&self, ui: &mut egui::Ui, active_notes: &HashMap<u8, Vec<u8>>) {
        const PIANO_LOW_NOTE: u8 = 21; // A0
        const PIANO_HIGH_NOTE: u8 = 108; // C8
        const BLACK_KEY_MODS: [u8; 5] = [1, 3, 6, 8, 10]; // C#, D#, F#, G#, A#

        let desired_size = egui::vec2(ui.available_width(), 65.0);
        let (response, painter) = ui.allocate_painter(desired_size, egui::Sense::hover());
        let rect = response.rect;

        // Define the area strictly for keys (top 50px)
        let keys_area = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 50.0));

        let note_range = (PIANO_HIGH_NOTE - PIANO_LOW_NOTE + 1) as f32;
        let key_width = keys_area.width() / note_range;

        for note in PIANO_LOW_NOTE..=PIANO_HIGH_NOTE {
            let note_mod = note % 12;
            let is_black_key = BLACK_KEY_MODS.contains(&note_mod);

            let x_start = egui::remap(
                note as f64,
                PIANO_LOW_NOTE as f64..=(PIANO_HIGH_NOTE + 1) as f64,
                keys_area.left() as f64..=keys_area.right() as f64,
            ) as f32;

            let key_rect =
                egui::Rect::from_x_y_ranges(x_start..=(x_start + key_width), keys_area.y_range());

            // Draw Background
            let base_color = if is_black_key {
                egui::Color32::from_gray(50)
            } else {
                egui::Color32::from_gray(100)
            };

            painter.add(egui::Shape::Rect(egui::epaint::RectShape::new(
                key_rect,
                egui::CornerRadius::ZERO,
                base_color,
                Stroke::new(1.0, egui::Color32::BLACK),
                egui::StrokeKind::Middle,
            )));

            // Draw Active Slices & Channel Numbers
            if let Some(channels) = active_notes.get(&note) {
                let count = channels.len();
                if count > 0 {
                    let slice_height = key_rect.height() / count as f32;

                    for (i, &channel) in channels.iter().enumerate() {
                        let active_color = Self::get_channel_color(channel);

                        let y_start = key_rect.top() + (i as f32 * slice_height);
                        let slice_rect = egui::Rect::from_min_size(
                            egui::pos2(key_rect.left(), y_start),
                            egui::vec2(key_rect.width(), slice_height),
                        );

                        // Draw Color Slice
                        painter.add(egui::Shape::Rect(egui::epaint::RectShape::new(
                            slice_rect,
                            egui::CornerRadius::ZERO,
                            active_color,
                            Stroke::new(0.5, egui::Color32::BLACK),
                            egui::StrokeKind::Middle,
                        )));

                        // Only draw channel number if the slice is tall enough to be readable (> 10px)
                        if slice_height > 10.0 {
                            // Calculate contrast color: if background is bright, use black text
                            let brightness = (active_color.r() as u32
                                + active_color.g() as u32
                                + active_color.b() as u32)
                                / 3;
                            let text_color = if brightness > 128 {
                                egui::Color32::BLACK
                            } else {
                                egui::Color32::WHITE
                            };

                            painter.text(
                                slice_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                (channel + 1).to_string(),
                                egui::FontId::proportional(10.0), // Small font to fit key width
                                text_color,
                            );
                        }
                    }
                }
            }

            // Draw Octave Labels below the keys
            if !is_black_key && note_mod == 0 {
                let octave = (note / 12) - 1;
                let note_label = format!("C{}", octave);

                // Position text below the key rect
                let pos = egui::pos2(key_rect.center().x, keys_area.bottom() + 8.0);

                painter.text(
                    pos,
                    egui::Align2::CENTER_CENTER,
                    note_label,
                    egui::FontId::proportional(10.0),
                    egui::Color32::from_gray(180), // Softer gray for labels
                );
            }
        }
    }

    /// Renders a modal window for saving a preset.
    fn draw_preset_save_modal(&mut self, ctx: &egui::Context) {
        if !self.show_preset_save_modal {
            return;
        }

        let mut is_open = self.show_preset_save_modal;
        let slot_display = self.preset_save_slot + 1;

        egui::Window::new(t!("gui.save_preset_title_fmt", num = slot_display))
            .open(&mut is_open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(t!("gui.enter_name_prompt"));

                    let text_edit =
                        egui::TextEdit::singleline(&mut self.preset_save_name).desired_width(250.0);
                    let response = ui.add(text_edit);

                    // Only request focus if the name is empty (fresh open)
                    // (Prevents focus stealing that makes buttons unclickable)
                    if self.preset_save_name.is_empty() && !response.has_focus() {
                        response.request_focus();
                    }

                    // Handle Keys
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.show_preset_save_modal = false;
                    }

                    ui.add_space(10.0);

                    ui.horizontal(|ui| {
                        if ui.button(t!("gui.btn_cancel")).clicked() {
                            self.show_preset_save_modal = false;
                        }

                        // Check for 'Enter' key OR 'Save' click
                        let enter_pressed = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let save_clicked = ui.button(t!("gui.btn_save")).clicked();

                        if save_clicked || enter_pressed {
                            if !self.preset_save_name.is_empty() {
                                self.app_state.lock().unwrap().save_preset(
                                    self.preset_save_slot,
                                    self.preset_save_name.clone(),
                                );
                                self.show_preset_save_modal = false;
                            }
                        }
                    });
                });
            });

        // Sync the internal state if the user clicked the window's 'X' button
        if !is_open {
            self.show_preset_save_modal = false;
        }
    }

    /// helper function inside impl EguiApp or as a standalone function
    fn get_channel_color(channel: u8) -> egui::Color32 {
        match channel {
            0 => egui::Color32::from_rgb(255, 0, 0),     // Red
            1 => egui::Color32::from_rgb(255, 165, 0),   // Orange
            2 => egui::Color32::from_rgb(255, 255, 0),   // Yellow
            3 => egui::Color32::from_rgb(0, 255, 0),     // Lime Green
            4 => egui::Color32::from_rgb(0, 255, 255),   // Cyan
            5 => egui::Color32::from_rgb(0, 0, 255),     // Blue
            6 => egui::Color32::from_rgb(128, 0, 128),   // Purple
            7 => egui::Color32::from_rgb(255, 0, 255),   // Magenta
            8 => egui::Color32::from_rgb(255, 192, 203), // Pink
            9 => egui::Color32::from_rgb(139, 69, 19),   // Brown
            10..=15 => egui::Color32::from_gray(180),    // Light Gray for higher channels
            _ => egui::Color32::WHITE,                   // Fallback
        }
    }
}
