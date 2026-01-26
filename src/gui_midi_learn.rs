use crate::app_state::AppState;
use eframe::egui;
use rust_i18n::t;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, PartialEq, Debug)]
pub enum LearnTarget {
    Stop(usize),
    Tremulant(String),
    Preset(usize),
}

impl Default for LearnTarget {
    fn default() -> Self {
        LearnTarget::Stop(0)
    }
}

pub struct MidiLearnState {
    pub is_open: bool,
    pub target: LearnTarget,
    pub target_name: String,

    // If Some, we are waiting for a MIDI event to assign to (internal_channel, is_enable_slot)
    // For Tremulants, internal_channel is ignored (passed as 0)
    pub learning_slot: Option<(u8, bool)>,
    pub last_interaction: Instant,
}

impl Default for MidiLearnState {
    fn default() -> Self {
        Self {
            is_open: false,
            target: LearnTarget::default(),
            target_name: String::new(),
            learning_slot: None,
            last_interaction: Instant::now(),
        }
    }
}

pub fn draw_midi_learn_modal(
    ctx: &egui::Context,
    app_state: Arc<Mutex<AppState>>,
    learn_state: &mut MidiLearnState,
) {
    if !learn_state.is_open {
        return;
    }

    let mut is_open = learn_state.is_open;

    // We check for new MIDI events if we are in learning mode
    if let Some((target_internal, is_enable)) = learn_state.learning_slot {
        let mut state = app_state.lock().unwrap();
        if let Some((event, time)) = &state.last_midi_event_received {
            if *time > learn_state.last_interaction {
                // We caught a midi event!
                let event_clone = event.clone();

                match &learn_state.target {
                    LearnTarget::Stop(idx) => {
                        state.midi_control_map.learn_stop(
                            *idx,
                            target_internal,
                            event_clone.clone(),
                            is_enable,
                        );
                    }
                    LearnTarget::Tremulant(id) => {
                        state.midi_control_map.learn_tremulant(
                            id.clone(),
                            event_clone.clone(),
                            is_enable,
                        );
                    }
                    LearnTarget::Preset(slot) => {
                        // Presets only activate
                        if is_enable {
                            state
                                .midi_control_map
                                .learn_preset(*slot, event_clone.clone());
                        }
                    }
                }
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
                    t!(
                        "midi_learn.log_mapped_fmt",
                        event = event_clone,
                        action = action_text
                    )
                    .to_string(),
                );
            }
        }
    }

    let window_title = t!(
        "midi_learn.window_title_fmt",
        name = learn_state.target_name
    );

    egui::Window::new(window_title)
        .open(&mut is_open)
        .resizable(true)
        .default_width(600.0)
        .default_height(400.0)
        .show(ctx, |ui| {
            ui.label(t!("midi_learn.description_1"));

            // Clone to avoid borrow conflict
            let target = learn_state.target.clone();

            match target {
                LearnTarget::Stop(idx) => {
                    ui.label(t!("midi_learn.description_2"));
                    ui.add_space(10.0);

                    // Fetch current map for rendering
                    let control_map = {
                        let state = app_state.lock().unwrap();
                        state
                            .midi_control_map
                            .stops
                            .get(&idx)
                            .cloned()
                            .unwrap_or_default()
                    };
                    draw_stop_grid(ui, learn_state, &control_map, app_state.clone(), idx);
                }
                LearnTarget::Tremulant(id) => {
                    ui.add_space(10.0);
                    let control = {
                        let state = app_state.lock().unwrap();
                        state
                            .midi_control_map
                            .tremulants
                            .get(&id)
                            .cloned()
                            .unwrap_or_default()
                    };
                    draw_tremulant_row(ui, learn_state, &id, &control, app_state.clone());
                }
                LearnTarget::Preset(slot) => {
                    ui.add_space(10.0);
                    let trigger = {
                        let state = app_state.lock().unwrap();
                        state.midi_control_map.presets.get(&slot).cloned().flatten()
                    };
                    draw_preset_row(ui, learn_state, slot, trigger, app_state.clone());
                }
            }
        });

    learn_state.is_open = is_open;
}

fn draw_tremulant_row(
    ui: &mut egui::Ui,
    learn_state: &mut MidiLearnState,
    trem_id: &str,
    control: &crate::midi_control::StopChannelControl,
    app_state: Arc<Mutex<AppState>>,
) {
    egui::Grid::new("trem_learn_grid")
        .num_columns(3)
        .striped(true)
        .spacing([20.0, 8.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new(t!("midi_learn.col_enable_event")).strong());
            ui.label(egui::RichText::new(t!("midi_learn.col_disable_event")).strong());
            ui.label(egui::RichText::new(t!("midi_learn.col_actions")).strong());
            ui.end_row();

            // Enable
            let enable_txt = if learn_state.learning_slot == Some((0, true)) {
                t!("midi_learn.status_listening").to_string()
            } else if let Some(evt) = &control.enable_event {
                evt.to_string()
            } else {
                t!("midi_learn.btn_learn").to_string()
            };
            if ui
                .add(
                    egui::Button::new(enable_txt)
                        .selected(learn_state.learning_slot == Some((0, true))),
                )
                .clicked()
            {
                learn_state.last_interaction = Instant::now();
                learn_state.learning_slot = Some((0, true));
            }

            // Disable
            let disable_txt = if learn_state.learning_slot == Some((0, false)) {
                t!("midi_learn.status_listening").to_string()
            } else if let Some(evt) = &control.disable_event {
                evt.to_string()
            } else {
                t!("midi_learn.btn_learn").to_string()
            };
            if ui
                .add(
                    egui::Button::new(disable_txt)
                        .selected(learn_state.learning_slot == Some((0, false))),
                )
                .clicked()
            {
                learn_state.last_interaction = Instant::now();
                learn_state.learning_slot = Some((0, false));
            }

            // Clear
            if ui.button(t!("midi_learn.btn_clear")).clicked() {
                let mut state = app_state.lock().unwrap();
                state.midi_control_map.clear_tremulant(trem_id);
                let _ = state.midi_control_map.save(&state.organ.name);
            }
            ui.end_row();
        });
}

fn draw_preset_row(
    ui: &mut egui::Ui,
    learn_state: &mut MidiLearnState,
    slot: usize,
    trigger: Option<crate::config::MidiEventSpec>,
    app_state: Arc<Mutex<AppState>>,
) {
    egui::Grid::new("preset_learn_grid")
        .num_columns(2)
        .striped(true)
        .spacing([20.0, 8.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new(t!("midi_learn.col_enable_event")).strong());
            ui.label(egui::RichText::new(t!("midi_learn.col_actions")).strong());
            ui.end_row();

            // Trigger Button
            let txt = if learn_state.learning_slot == Some((0, true)) {
                t!("midi_learn.status_listening").to_string()
            } else if let Some(evt) = trigger {
                evt.to_string()
            } else {
                t!("midi_learn.btn_learn").to_string()
            };

            if ui
                .add(egui::Button::new(txt).selected(learn_state.learning_slot == Some((0, true))))
                .clicked()
            {
                learn_state.last_interaction = Instant::now();
                learn_state.learning_slot = Some((0, true)); // 0 = dummy internal channel, true = enable
            }

            // Clear Button
            if ui.button(t!("midi_learn.btn_clear")).clicked() {
                let mut state = app_state.lock().unwrap();
                state.midi_control_map.clear_preset(slot);
                let _ = state.midi_control_map.save(&state.organ.name);
            }
            ui.end_row();
        });
}

fn draw_stop_grid(
    ui: &mut egui::Ui,
    learn_state: &mut MidiLearnState,
    control_map: &std::collections::HashMap<u8, crate::midi_control::StopChannelControl>,
    app_state: Arc<Mutex<AppState>>,
    stop_idx: usize,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        egui::Grid::new("midi_learn_grid")
            .num_columns(4)
            .striped(true)
            .spacing([20.0, 8.0])
            .show(ui, |ui| {
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

                    // Enable
                    let enable_btn_text = if learn_state.learning_slot == Some((channel, true)) {
                        t!("midi_learn.status_listening").to_string()
                    } else if let Some(evt) = enable_evt {
                        evt.to_string()
                    } else {
                        t!("midi_learn.btn_learn").to_string()
                    };

                    if ui
                        .add(
                            egui::Button::new(enable_btn_text)
                                .selected(learn_state.learning_slot == Some((channel, true))),
                        )
                        .clicked()
                    {
                        learn_state.last_interaction = Instant::now();
                        learn_state.learning_slot = Some((channel, true));
                    }

                    // Disable
                    let disable_btn_text = if learn_state.learning_slot == Some((channel, false)) {
                        t!("midi_learn.status_listening").to_string()
                    } else if let Some(evt) = disable_evt {
                        evt.to_string()
                    } else {
                        t!("midi_learn.btn_learn").to_string()
                    };

                    if ui
                        .add(
                            egui::Button::new(disable_btn_text)
                                .selected(learn_state.learning_slot == Some((channel, false))),
                        )
                        .clicked()
                    {
                        learn_state.last_interaction = Instant::now();
                        learn_state.learning_slot = Some((channel, false));
                    }

                    // Clear
                    if ui.button(t!("midi_learn.btn_clear")).clicked() {
                        let mut state = app_state.lock().unwrap();
                        state.midi_control_map.clear_stop(stop_idx, channel);
                        let _ = state.midi_control_map.save(&state.organ.name);
                    }
                    ui.end_row();
                }
            });
    });
}
