//! Translations exposed to the web UI.
//!
//! Builds a flat dictionary of stable string keys → translated strings using
//! the active locale set on `rust_i18n`. The web client fetches this via
//! `/i18n` and uses it to populate every user-visible string. Keys are
//! sourced from existing namespaces wherever a suitable translation already
//! exists in the YAML files (so we leverage all the work that's been done
//! for the desktop UI), and from the `web:` namespace for strings only the
//! web UI needs.

use rust_i18n::t;
use serde_json::{Value, json};

/// (locale code, native display name, flag emoji)
///
/// Constructed languages without a country (Esperanto, Latin, Klingon) get a
/// thematic glyph instead of a country flag. The locale codes here must match
/// the YAML filenames in `locales/`.
pub const SUPPORTED_LANGUAGES: &[(&str, &str, &str)] = &[
    ("ca", "Català", "🇪🇸"),
    ("cs", "Čeština", "🇨🇿"),
    ("da", "Dansk", "🇩🇰"),
    ("de", "Deutsch", "🇩🇪"),
    ("en", "English", "🇬🇧"),
    ("eo", "Esperanto", "⭐"),
    ("es", "Español", "🇪🇸"),
    ("fi", "Suomi", "🇫🇮"),
    ("fr", "Français", "🇫🇷"),
    ("ga", "Gaeilge", "🇮🇪"),
    ("gd", "Gàidhlig", "🏴󠁧󠁢󠁳󠁣󠁴󠁿"),
    ("hu", "Magyar", "🇭🇺"),
    ("id", "Bahasa Indonesia", "🇮🇩"),
    ("it", "Italiano", "🇮🇹"),
    ("ja", "日本語", "🇯🇵"),
    ("ko", "한국어", "🇰🇷"),
    ("la", "Latina", "🏛️"),
    ("nb", "Norsk Bokmål", "🇳🇴"),
    ("nl", "Nederlands", "🇳🇱"),
    ("nl-BE", "Vlaams", "🇧🇪"),
    ("pl", "Polski", "🇵🇱"),
    ("pt", "Português", "🇵🇹"),
    ("ro", "Română", "🇷🇴"),
    ("ru", "Русский", "🇷🇺"),
    ("sv", "Svenska", "🇸🇪"),
    ("tlh", "tlhIngan Hol", "🛸"),
    ("uk", "Українська", "🇺🇦"),
    ("zh-CN", "简体中文", "🇨🇳"),
    ("zh-TW", "繁體中文", "🇹🇼"),
];

/// Returns the list of supported languages as JSON for inclusion in the
/// `/i18n` response.
fn languages_json() -> Value {
    let arr: Vec<Value> = SUPPORTED_LANGUAGES
        .iter()
        .map(|(code, name, flag)| {
            json!({
                "code": code,
                "native_name": name,
                "flag": flag,
            })
        })
        .collect();
    Value::Array(arr)
}

/// Build a JSON document containing the active locale code and a flat map
/// of stable string keys to their translated values.
pub fn web_translations() -> Value {
    // Helper: forward a `t!()` lookup under a stable key. Each entry is a
    // (web_key, translated_value) tuple. `t!()` falls back to the default
    // locale automatically when a key is missing in the active one.
    let mut s = serde_json::Map::new();

    // Topbar
    s.insert("topbar_config".into(), t!("web.topbar_config").into());
    s.insert("topbar_panic".into(), t!("web.topbar_panic").into());
    s.insert("status_connecting".into(), t!("web.status_connecting").into());
    s.insert("status_connected".into(), t!("web.status_connected").into());

    // Tabs
    s.insert("tab_presets".into(), t!("web.tab_presets").into());
    s.insert("tab_stops".into(), t!("web.tab_stops").into());
    s.insert("tab_tremulants".into(), t!("web.tab_tremulants").into());
    s.insert("tab_audio".into(), t!("web.tab_audio").into());
    s.insert("tab_recording".into(), t!("web.tab_recording").into());
    s.insert("tab_organs".into(), t!("web.tab_organs").into());
    s.insert("config_tab_organ".into(), t!("web.config_tab_organ").into());
    s.insert("config_tab_audio".into(), t!("web.config_tab_audio").into());
    s.insert("config_tab_midi".into(), t!("web.config_tab_midi").into());
    s.insert(
        "config_tab_advanced".into(),
        t!("web.config_tab_advanced").into(),
    );

    // Play view
    s.insert("presets_hint".into(), t!("web.presets_hint").into());
    s.insert(
        "stops_channel_label".into(),
        t!("web.stops_channel_label").into(),
    );
    s.insert(
        "stops_channel_hint".into(),
        t!("web.stops_channel_hint").into(),
    );
    s.insert(
        "default_division_heading".into(),
        t!("web.default_division_heading").into(),
    );
    s.insert(
        "tremulants_hint".into(),
        t!("web.tremulants_hint").into(),
    );
    s.insert("no_tremulants".into(), t!("web.no_tremulants").into());
    s.insert(
        "audio_master_gain".into(),
        t!("web.audio_master_gain").into(),
    );
    s.insert("audio_polyphony".into(), t!("web.audio_polyphony").into());
    s.insert(
        "audio_reverb_heading".into(),
        t!("web.audio_reverb_heading").into(),
    );
    s.insert("audio_ir_label".into(), t!("web.audio_ir_label").into());
    s.insert("audio_mix_label".into(), t!("web.audio_mix_label").into());
    s.insert("reverb_disabled".into(), t!("web.reverb_disabled").into());

    // Recording
    s.insert("rec_midi_start".into(), t!("web.rec_midi_start").into());
    s.insert("rec_midi_stop".into(), t!("web.rec_midi_stop").into());
    s.insert("rec_audio_start".into(), t!("web.rec_audio_start").into());
    s.insert("rec_audio_stop".into(), t!("web.rec_audio_stop").into());
    s.insert("rec_midi_hint".into(), t!("web.rec_midi_hint").into());
    s.insert("rec_audio_hint".into(), t!("web.rec_audio_hint").into());

    // Organ list
    s.insert("organs_hint".into(), t!("web.organs_hint").into());
    s.insert("organs_none".into(), t!("web.organs_none").into());
    s.insert(
        "organ_badge_current".into(),
        t!("web.organ_badge_current").into(),
    );

    // Config: organ tab
    s.insert(
        "config_select_organ_heading".into(),
        t!("web.config_select_organ_heading").into(),
    );
    s.insert(
        "config_select_organ_hint".into(),
        t!("web.config_select_organ_hint").into(),
    );
    s.insert(
        "config_organs_empty".into(),
        t!("web.config_organs_empty").into(),
    );
    // Reuse the desktop UI's Start/Quit labels where they exist
    s.insert("config_btn_start".into(), t!("config.btn_start").into());
    s.insert("config_btn_quit".into(), t!("config.btn_quit").into());
    s.insert("config_btn_rescan".into(), t!("web.config_btn_rescan").into());
    s.insert(
        "config_btn_browse".into(),
        t!("web.config_btn_browse").into(),
    );
    s.insert(
        "config_btn_add_organ".into(),
        t!("web.config_btn_add_organ").into(),
    );
    s.insert(
        "config_btn_remove_organ".into(),
        t!("web.config_btn_remove_organ").into(),
    );

    // File browser
    s.insert("file_browser_title".into(), t!("web.file_browser_title").into());
    s.insert(
        "file_browser_title_organ".into(),
        t!("web.file_browser_title_organ").into(),
    );
    s.insert(
        "file_browser_title_ir".into(),
        t!("web.file_browser_title_ir").into(),
    );
    s.insert(
        "file_browser_up_title".into(),
        t!("web.file_browser_up_title").into(),
    );
    s.insert(
        "file_browser_empty".into(),
        t!("web.file_browser_empty").into(),
    );

    // Library mutation toasts/confirms
    s.insert(
        "config_added_organ_fmt".into(),
        t!("web.config_added_organ_fmt").into(),
    );
    s.insert(
        "config_removed_organ_fmt".into(),
        t!("web.config_removed_organ_fmt").into(),
    );
    s.insert(
        "config_remove_organ_confirm_fmt".into(),
        t!("web.config_remove_organ_confirm_fmt").into(),
    );
    s.insert(
        "config_ir_selected_fmt".into(),
        t!("web.config_ir_selected_fmt").into(),
    );
    s.insert("err_browse_fmt".into(), t!("web.err_browse_fmt").into());
    s.insert(
        "config_warn_no_organ".into(),
        t!("config.warn_select_organ").into(),
    );
    s.insert(
        "config_organ_badge_selected".into(),
        t!("web.config_organ_badge_selected").into(),
    );

    // Config: audio tab — reuse desktop labels where appropriate
    s.insert(
        "config_audio_device_heading".into(),
        t!("web.config_audio_device_heading").into(),
    );
    s.insert(
        "config_audio_device_default".into(),
        t!("config.status_default").into(),
    );
    s.insert(
        "config_sample_rate_heading".into(),
        t!("web.config_sample_rate_heading").into(),
    );
    s.insert(
        "config_sample_rate_hint".into(),
        t!("web.config_sample_rate_hint").into(),
    );
    s.insert(
        "config_reverb_heading".into(),
        t!("web.config_reverb_heading").into(),
    );
    s.insert(
        "config_reverb_none".into(),
        t!("config.status_no_reverb").into(),
    );

    // Config: MIDI tab
    s.insert(
        "config_midi_heading".into(),
        t!("web.config_midi_heading").into(),
    );
    s.insert("config_midi_hint".into(), t!("web.config_midi_hint").into());
    s.insert("config_midi_none".into(), t!("web.config_midi_none").into());
    s.insert(
        "config_midi_map_button".into(),
        t!("web.config_midi_map_button").into(),
    );
    s.insert(
        "config_midi_summary_complex_default".into(),
        t!("web.config_midi_summary_complex_default").into(),
    );

    // Config: advanced tab
    s.insert(
        "config_advanced_heading".into(),
        t!("web.config_advanced_heading").into(),
    );
    s.insert(
        "config_buffer_heading".into(),
        t!("web.config_buffer_heading").into(),
    );
    s.insert(
        "config_buffer_hint".into(),
        t!("web.config_buffer_hint").into(),
    );
    s.insert(
        "config_max_ram_heading".into(),
        t!("web.config_max_ram_heading").into(),
    );
    s.insert(
        "config_max_ram_hint".into(),
        t!("web.config_max_ram_hint").into(),
    );
    s.insert(
        "config_options_heading".into(),
        t!("web.config_options_heading").into(),
    );
    s.insert("config_chk_precache".into(), t!("config.chk_precache").into());
    s.insert("config_chk_convert".into(), t!("config.chk_convert").into());
    s.insert("config_chk_tuning".into(), t!("config.chk_tuning").into());

    // MIDI mapping modal — reuse desktop translations
    s.insert("midi_modal_title".into(), t!("midi_config.heading").into());
    s.insert(
        "midi_modal_mode_simple".into(),
        t!("web.midi_modal_mode_simple").into(),
    );
    s.insert(
        "midi_modal_mode_complex".into(),
        t!("web.midi_modal_mode_complex").into(),
    );
    s.insert(
        "midi_modal_target_label".into(),
        t!("midi_config.target_channel_label").into(),
    );
    s.insert(
        "midi_modal_complex_hint".into(),
        t!("midi_config.complex_desc").into(),
    );
    s.insert("midi_modal_done".into(), t!("midi_config.btn_done").into());

    // Modal — save preset (reuse desktop labels)
    s.insert(
        "modal_save_preset_title".into(),
        t!("gui.btn_save").into(),
    );
    s.insert(
        "modal_save_preset_slot_label".into(),
        t!("web.modal_save_preset_slot_label").into(),
    );
    s.insert(
        "modal_save_preset_placeholder".into(),
        t!("web.modal_save_preset_placeholder").into(),
    );
    s.insert("modal_btn_cancel".into(), t!("gui.btn_cancel").into());
    s.insert("modal_btn_save".into(), t!("gui.btn_save").into());
    s.insert("modal_btn_close".into(), t!("web.modal_btn_close").into());

    // Modal — preset actions
    s.insert("preset_action_load".into(), t!("web.preset_action_load").into());
    s.insert("preset_action_save".into(), t!("web.preset_action_save").into());
    s.insert(
        "preset_action_learn".into(),
        t!("web.preset_action_learn").into(),
    );
    s.insert(
        "preset_action_clear".into(),
        t!("web.preset_action_clear").into(),
    );
    s.insert("preset_empty".into(), t!("web.preset_empty").into());

    // Modal — stop actions
    s.insert("modal_stop_title".into(), t!("web.modal_stop_title").into());
    s.insert(
        "stop_action_learn_enable".into(),
        t!("web.stop_action_learn_enable").into(),
    );
    s.insert(
        "stop_action_learn_disable".into(),
        t!("web.stop_action_learn_disable").into(),
    );
    s.insert("stop_action_clear".into(), t!("web.stop_action_clear").into());

    // Modal — tremulant actions
    s.insert(
        "modal_tremulant_title".into(),
        t!("web.modal_tremulant_title").into(),
    );
    s.insert(
        "trem_action_learn_enable".into(),
        t!("web.trem_action_learn_enable").into(),
    );
    s.insert(
        "trem_action_learn_disable".into(),
        t!("web.trem_action_learn_disable").into(),
    );
    s.insert("trem_action_clear".into(), t!("web.trem_action_clear").into());

    // Modal — MIDI learn
    s.insert(
        "modal_learn_title".into(),
        t!("web.modal_learn_title").into(),
    );
    s.insert("learn_waiting".into(), t!("web.learn_waiting").into());
    s.insert("learn_done".into(), t!("web.learn_done").into());
    s.insert("learn_timed_out".into(), t!("web.learn_timed_out").into());

    // Toasts
    s.insert("toast_panic".into(), t!("web.toast_panic").into());
    s.insert(
        "toast_loading_organ".into(),
        t!("web.toast_loading_organ").into(),
    );
    s.insert("toast_reloading".into(), t!("web.toast_reloading").into());
    s.insert(
        "toast_rec_midi_started".into(),
        t!("web.toast_rec_midi_started").into(),
    );
    s.insert(
        "toast_rec_midi_saved".into(),
        t!("web.toast_rec_midi_saved").into(),
    );
    s.insert(
        "toast_rec_audio_started".into(),
        t!("web.toast_rec_audio_started").into(),
    );
    s.insert(
        "toast_rec_audio_saved".into(),
        t!("web.toast_rec_audio_saved").into(),
    );
    s.insert(
        "config_midi_rescan_done".into(),
        t!("web.config_midi_rescan_done").into(),
    );
    s.insert(
        "config_quit_confirm".into(),
        t!("web.config_quit_confirm").into(),
    );

    // Format strings (containing %{var}) — JS substitutes the placeholders.
    s.insert(
        "status_reconnecting_fmt".into(),
        t!("web.status_reconnecting_fmt").into(),
    );
    s.insert("channel_fmt".into(), t!("web.channel_fmt").into());
    s.insert(
        "stop_actions_channel_fmt".into(),
        t!("web.stop_actions_channel_fmt").into(),
    );
    s.insert(
        "midi_modal_title_fmt".into(),
        t!("web.midi_modal_title_fmt").into(),
    );
    s.insert(
        "midi_modal_input_fmt".into(),
        t!("web.midi_modal_input_fmt").into(),
    );
    s.insert(
        "modal_preset_title_fmt".into(),
        t!("web.modal_preset_title_fmt").into(),
    );
    s.insert(
        "modal_preset_title_named_fmt".into(),
        t!("web.modal_preset_title_named_fmt").into(),
    );
    s.insert(
        "config_midi_summary_simple_fmt".into(),
        t!("web.config_midi_summary_simple_fmt").into(),
    );
    s.insert(
        "config_midi_summary_complex_fmt".into(),
        t!("web.config_midi_summary_complex_fmt").into(),
    );
    s.insert("toast_loaded_fmt".into(), t!("web.toast_loaded_fmt").into());
    s.insert("toast_saved_fmt".into(), t!("web.toast_saved_fmt").into());
    s.insert(
        "toast_cleared_stop_fmt".into(),
        t!("web.toast_cleared_stop_fmt").into(),
    );
    s.insert(
        "toast_cleared_tremulant_fmt".into(),
        t!("web.toast_cleared_tremulant_fmt").into(),
    );
    s.insert(
        "toast_cleared_preset_fmt".into(),
        t!("web.toast_cleared_preset_fmt").into(),
    );
    s.insert("toast_learned_fmt".into(), t!("web.toast_learned_fmt").into());
    s.insert(
        "organ_already_loaded_fmt".into(),
        t!("web.organ_already_loaded_fmt").into(),
    );
    s.insert(
        "organ_load_confirm_fmt".into(),
        t!("web.organ_load_confirm_fmt").into(),
    );
    s.insert(
        "organ_loading_fmt".into(),
        t!("web.organ_loading_fmt").into(),
    );
    s.insert(
        "config_selected_fmt".into(),
        t!("web.config_selected_fmt").into(),
    );
    s.insert(
        "preset_empty_indicator".into(),
        t!("web.preset_empty").into(),
    );

    // Error toasts
    for (key, src) in [
        ("err_stop_toggle_fmt", "web.err_stop_toggle_fmt"),
        ("err_load_fmt", "web.err_load_fmt"),
        ("err_save_fmt", "web.err_save_fmt"),
        ("err_clear_fmt", "web.err_clear_fmt"),
        ("err_tremulant_fmt", "web.err_tremulant_fmt"),
        ("err_recording_fmt", "web.err_recording_fmt"),
        ("err_panic_fmt", "web.err_panic_fmt"),
        ("err_learn_fmt", "web.err_learn_fmt"),
        ("err_gain_fmt", "web.err_gain_fmt"),
        ("err_polyphony_fmt", "web.err_polyphony_fmt"),
        ("err_reverb_fmt", "web.err_reverb_fmt"),
        ("err_mix_fmt", "web.err_mix_fmt"),
        ("err_audio_device_fmt", "web.err_audio_device_fmt"),
        ("err_sample_rate_fmt", "web.err_sample_rate_fmt"),
        ("err_ir_file_fmt", "web.err_ir_file_fmt"),
        ("err_buffer_fmt", "web.err_buffer_fmt"),
        ("err_ram_fmt", "web.err_ram_fmt"),
        ("err_update_fmt", "web.err_update_fmt"),
        ("err_rescan_fmt", "web.err_rescan_fmt"),
        ("err_start_fmt", "web.err_start_fmt"),
        ("err_quit_fmt", "web.err_quit_fmt"),
        ("err_selection_fmt", "web.err_selection_fmt"),
    ] {
        s.insert(key.into(), Value::String(t!(src).to_string()));
    }

    json!({
        "locale": rust_i18n::locale().to_string(),
        "strings": Value::Object(s),
        "languages": languages_json(),
    })
}
