use eframe::egui;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use crate::app_state::AppState;
use rust_i18n::t;

pub struct MidiLearnState {
    pub is_open: bool,
    pub target_stop_index: usize,
    pub target_stop_name: String,
    
    // If Some, we are waiting for a MIDI event to assign to (internal_channel, is_enable_slot)
    pub learning_slot: Option<(u8, bool)>, 
    pub last_interaction: Instant,
}

impl Default for MidiLearnState {
    fn default() -> Self {
        Self {
            is_open: false,
            target_stop_index: 0,
            target_stop_name: String::new(),
            learning_slot: None,
            last_interaction: Instant::now(),
        }
    }
}

pub fn draw_midi_learn_modal(
    ctx: &egui::Context,
    app_state: Arc<Mutex<AppState>>,
    learn_state: &mut MidiLearnState
) {
    if !learn_state.is_open {
        return;
    }

    let mut is_open = learn_state.is_open;
    
    // We check for new MIDI events if we are in learning mode
    if let Some((target_internal, is_enable)) = learn_state.learning_slot {
        let mut state = app_state.lock().unwrap();
        // Check if a new MIDI event arrived since we opened the dialog/clicked learn
        if let Some((event, time)) = &state.last_midi_event_received {
            if *time > learn_state.last_interaction {
                // We caught a midi event!
                let event_clone = event.clone();
                state.midi_control_map.learn(
                    learn_state.target_stop_index, 
                    target_internal, 
                    event_clone.clone(), 
                    is_enable
                );
                // Save immediately
                let _ = state.midi_control_map.save(&state.organ.name);
                
                // Reset learning state
                learn_state.learning_slot = None;
                let action_text = if is_enable { 
                    t!("midi_learn.action_enable") 
                } else { 
                    t!("midi_learn.action_disable") 
                };
                
                state.add_midi_log(
                    t!("midi_learn.log_mapped_fmt", event = event_clone, action = action_text).to_string()
                );
            }
        }
    }

    let window_title = t!("midi_learn.window_title_fmt", name = learn_state.target_stop_name);

    egui::Window::new(window_title)
        .open(&mut is_open)
        .resizable(true)
        .default_width(600.0)
        .default_height(500.0)
        .show(ctx, |ui| {
            ui.label(t!("midi_learn.description_1"));
            ui.label(t!("midi_learn.description_2"));
            ui.add_space(10.0);

            // Fetch current map for rendering
            let control_map = {
                let state = app_state.lock().unwrap();
                state.midi_control_map.stops.get(&learn_state.target_stop_index).cloned().unwrap_or_default()
            };

            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("midi_learn_grid")
                    .num_columns(4)
                    .striped(true)
                    .spacing([20.0, 8.0])
                    .show(ui, |ui| {
                        
                        // Header
                        ui.label(egui::RichText::new(t!("midi_learn.col_internal_channel")).strong());
                        ui.label(egui::RichText::new(t!("midi_learn.col_enable_event")).strong());
                        ui.label(egui::RichText::new(t!("midi_learn.col_disable_event")).strong());
                        ui.label(egui::RichText::new(t!("midi_learn.col_actions")).strong());
                        ui.end_row();

                        for channel in 0..16u8 {
                            ui.label(t!("midi_learn.channel_fmt", num = channel + 1));

                            let config = control_map.get(&channel);
                            let enable_evt = config.and_then(|c| c.enable_event.as_ref());
                            let disable_evt = config.and_then(|c| c.disable_event.as_ref());

                            // --- Enable Column ---
                            let enable_btn_text = if learn_state.learning_slot == Some((channel, true)) {
                                t!("midi_learn.status_listening").to_string()
                            } else if let Some(evt) = enable_evt {
                                evt.to_string()
                            } else {
                                t!("midi_learn.btn_learn").to_string()
                            };

                            let btn = egui::Button::new(enable_btn_text)
                                .selected(learn_state.learning_slot == Some((channel, true)));

                            if ui.add(btn).clicked() {
                                learn_state.last_interaction = Instant::now();
                                learn_state.learning_slot = Some((channel, true));
                            }

                            // --- Disable Column ---
                            let disable_btn_text = if learn_state.learning_slot == Some((channel, false)) {
                                t!("midi_learn.status_listening").to_string()
                            } else if let Some(evt) = disable_evt {
                                evt.to_string()
                            } else {
                                t!("midi_learn.btn_learn").to_string()
                            };

                            let btn = egui::Button::new(disable_btn_text)
                                .selected(learn_state.learning_slot == Some((channel, false)));

                            if ui.add(btn).clicked() {
                                learn_state.last_interaction = Instant::now();
                                learn_state.learning_slot = Some((channel, false));
                            }

                            // --- Clear Column ---
                            if ui.button(t!("midi_learn.btn_clear")).clicked() {
                                let mut state = app_state.lock().unwrap();
                                state.midi_control_map.clear(learn_state.target_stop_index, channel);
                                let _ = state.midi_control_map.save(&state.organ.name);
                            }

                            ui.end_row();
                        }
                    });
            });
        });

    learn_state.is_open = is_open;
}