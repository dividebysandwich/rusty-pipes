use actix_web::dev::ServerHandle;
use actix_web::{App, HttpRequest, HttpResponse, HttpServer, Responder, web};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;

use crate::app::{AppMessage, MainLoopAction, WsMessage};
use crate::app_state::{AppState, WebLearnSession, WebLearnTarget};
use crate::config::{self, MidiEventSpec, load_organ_library};

/// A handle that controls the lifecycle of the API Server.
/// When this struct is dropped, the server shuts down and the background thread exits.
pub struct ApiServerHandle {
    handle: ServerHandle,
    /// Signals the MIDI-learn ticker thread to exit. Without this, every
    /// call to `start_api_server` (e.g. on organ reload) would leak a new
    /// ticker thread that holds a strong reference to the old `AppState`,
    /// pinning the old organ's sample data in memory.
    ticker_stop: Arc<AtomicBool>,
}

impl Drop for ApiServerHandle {
    fn drop(&mut self) {
        println!("Stopping API Server...");
        // Tell the ticker thread to exit at its next wake-up. It holds a
        // clone of Arc<Mutex<AppState>>, so releasing it is required for
        // the old AppState (and therefore Arc<Organ>) to be freed.
        self.ticker_stop.store(true, Ordering::Release);
        let handle = self.handle.clone();

        // Actix's stop() method is async, but Drop is sync.
        // We spawn a temporary thread with a minimal runtime just to await the stop signal.
        thread::spawn(move || {
            let sys = actix_web::rt::System::new();
            sys.block_on(async {
                // stop(true) means graceful shutdown (finish processing current requests)
                log::info!("Shutting down API server...");
                handle.stop(true).await;
                log::info!("API server shut down complete.");
            });
        });
    }
}

// --- Data Models ---

#[derive(Serialize, Clone, ToSchema)]
pub struct StopStatusResponse {
    /// The internal index of the stop
    index: usize,
    /// The name of the stop (e.g., "Principal 8'")
    name: String,
    /// List of active internal virtual channels (0-15) for this stop
    active_channels: Vec<u8>,
    /// Division (manual) identifier from the underlying organ definition,
    /// e.g. "GO" (Great), "SW" (Swell), "P" (Pedal). Empty if unknown.
    division: String,
}

#[derive(Serialize, Clone, ToSchema)]
pub struct PresetSlotResponse {
    /// 1-based slot number (1..=12)
    slot: usize,
    /// Preset name if occupied; None for empty slots
    name: Option<String>,
    occupied: bool,
    /// True if this slot was the most recently recalled preset.
    is_last_loaded: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct MidiLearnStartRequest {
    /// "stop", "tremulant", or "preset"
    target: String,
    /// Required for "stop"
    stop_index: Option<usize>,
    /// Required for "stop": virtual channel 0-15
    channel: Option<u8>,
    /// Required for "stop" and "tremulant": true to learn the enable
    /// trigger, false to learn the disable trigger
    is_enable: Option<bool>,
    /// Required for "tremulant"
    tremulant_id: Option<String>,
    /// Required for "preset": 1-based slot id
    preset_slot: Option<usize>,
}

#[derive(Serialize, Clone, ToSchema)]
pub struct MidiLearnStatusResponse {
    /// "idle", "waiting", "captured", or "timed_out"
    state: String,
    target_name: Option<String>,
    /// Human-readable description of the captured event, populated once state == "captured"
    event_description: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct ChannelUpdateRequest {
    /// True to enable the stop for this channel, False to disable
    active: bool,
}

#[derive(Serialize, Clone, ToSchema)]
pub struct OrganInfoResponse {
    /// The name of the loaded organ definition
    name: String,
}

#[derive(Serialize, Clone, ToSchema)]
pub struct OrganEntryResponse {
    /// Name of the organ
    name: String,
    /// Path relative to library or absolute path
    path: String,
}

#[derive(Deserialize, ToSchema)]
pub struct LoadOrganRequest {
    /// The path of the organ to load (must match an entry in the library)
    path: String,
}

#[derive(Deserialize, ToSchema)]
pub struct PresetSaveRequest {
    name: String,
}

#[derive(Deserialize, ToSchema)]
pub struct ValueRequest {
    value: f32,
}

#[derive(Deserialize, ToSchema)]
pub struct ReverbRequest {
    index: i32,
}

#[derive(Deserialize, ToSchema)]
pub struct ReverbMixRequest {
    mix: f32,
}

#[derive(Serialize, Clone, ToSchema)]
pub struct ReverbEntry {
    index: usize,
    name: String,
}

#[derive(Serialize, Clone, ToSchema)]
pub struct AudioSettingsResponse {
    gain: f32,
    polyphony: usize,
    reverb_mix: f32,
    active_reverb_index: Option<usize>,
    is_recording_midi: bool,
    is_recording_audio: bool,
}

#[derive(Serialize, Clone, ToSchema)]
pub struct TremulantResponse {
    id: String,
    name: String,
    active: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct TremulantSetRequest {
    active: bool,
}

// --- Shared State ---

struct ApiData {
    app_state: Arc<Mutex<AppState>>,
    audio_tx: Sender<AppMessage>,
    // We need access to the exit action mutex to trigger organ reload
    exit_action: Arc<Mutex<MainLoopAction>>,
    reverb_files: Arc<Vec<(String, PathBuf)>>,
    ws_tx: broadcast::Sender<WsMessage>,
}

fn broadcast(data: &web::Data<ApiData>, msg: WsMessage) {
    let _ = data.ws_tx.send(msg);
}

// --- OpenAPI Documentation ---

#[derive(OpenApi)]
#[openapi(
    paths(
        get_organ_info,
        get_organ_library,
        load_organ,
        get_stops,
        panic,
        update_stop_channel,
        get_presets,
        load_preset,
        save_preset,
        get_audio_settings,
        set_gain,
        set_polyphony,
        start_stop_midi_recording,
        start_stop_audio_recording,
        get_reverbs,
        set_reverb,
        set_reverb_mix,
        get_tremulants,
        set_tremulant,
        midi_learn_start,
        midi_learn_status,
        midi_learn_cancel,
        clear_stop_binding,
        clear_tremulant_binding,
        clear_preset_binding
    ),
    components(
        schemas(
            StopStatusResponse,
            ChannelUpdateRequest,
            OrganInfoResponse,
            OrganEntryResponse,
            LoadOrganRequest,
            PresetSaveRequest,
            PresetSlotResponse,
            ValueRequest,
            ReverbRequest,
            ReverbMixRequest,
            ReverbEntry,
            AudioSettingsResponse,
            TremulantResponse,
            TremulantSetRequest,
            MidiLearnStartRequest,
            MidiLearnStatusResponse
        )
    ),
    tags(
        (name = "Rusty Pipes API", description = "Control endpoints for the virtual organ")
    )
)]
struct ApiDoc;

// --- Handlers ---

/// Redirects to the embedded web UI.
#[utoipa::path(get, path = "/", responses((status = 302, description = "Redirect to web UI")))]
async fn index() -> impl Responder {
    HttpResponse::Found()
        .append_header(("Location", "/ui/"))
        .finish()
}

/// Returns the server's current operating mode. The web UI uses this to
/// decide whether to render the configuration view or the play view.
async fn mode() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({"mode": "play"}))
}

/// Returns the active locale and the dictionary of translated strings used
/// by the web UI. Mirrored on the config-mode server.
async fn i18n() -> impl Responder {
    HttpResponse::Ok().json(crate::i18n_web::web_translations())
}

/// Returns information about the currently loaded organ.
#[utoipa::path(
    get, path = "/organ", tag = "General",
    responses((status = 200, body = OrganInfoResponse))
)]
async fn get_organ_info(data: web::Data<ApiData>) -> impl Responder {
    let state = data.app_state.lock().unwrap();
    HttpResponse::Ok().json(OrganInfoResponse {
        name: state.organ.name.clone(),
    })
}

/// Returns a list of all organs available in the library.
#[utoipa::path(
    get, path = "/organs", tag = "General",
    responses((status = 200, body = Vec<OrganEntryResponse>))
)]
async fn get_organ_library() -> impl Responder {
    match load_organ_library() {
        Ok(lib) => {
            let response: Vec<OrganEntryResponse> = lib
                .organs
                .iter()
                .map(|p| OrganEntryResponse {
                    name: p.name.clone(),
                    path: p.path.to_string_lossy().to_string(),
                })
                .collect();
            HttpResponse::Ok().json(response)
        }
        Err(e) => {
            HttpResponse::InternalServerError().body(format!("Failed to load library: {}", e))
        }
    }
}

/// Triggers the application to load a different organ.
/// Note: This will cause the API server to restart shortly after the response is sent.
#[utoipa::path(
    post, path = "/organs/load", tag = "General",
    request_body = LoadOrganRequest,
    responses(
        (status = 200, description = "Reload initiated"),
        (status = 404, description = "Organ not found in library")
    )
)]
async fn load_organ(body: web::Json<LoadOrganRequest>, data: web::Data<ApiData>) -> impl Responder {
    let target_path_str = &body.path;
    let lib = match load_organ_library() {
        Ok(l) => l,
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };

    // Verify the path exists in the library (security + validation)
    let found = lib
        .organs
        .iter()
        .find(|o| o.path.to_string_lossy() == *target_path_str);

    if let Some(profile) = found {
        log::info!("API: Requesting reload of organ: {}", profile.name);

        // Tell every connected web client the server is about to restart
        // BEFORE we start shutting things down. The WS tasks will forward
        // this frame and then close their sessions, so the clients see
        // the close event promptly and begin reconnecting. By the time
        // they successfully reconnect, they'll be talking to the new
        // server (with the new organ's state).
        broadcast(&data, WsMessage::ServerRestarting);

        // Give the async WS tasks a moment to flush the ServerRestarting
        // frame and close their sessions before the main thread tears
        // down. Without this, the tasks can be cancelled mid-send.
        actix_web::rt::time::sleep(Duration::from_millis(100)).await;

        // Signal the main loop to reload
        *data.exit_action.lock().unwrap() = MainLoopAction::ReloadOrgan {
            file: profile.path.clone(),
        };

        let _ = data.audio_tx.send(AppMessage::Quit);

        HttpResponse::Ok().json(serde_json::json!({"status": "reloading", "organ": profile.name}))
    } else {
        HttpResponse::NotFound().body("Organ path not found in library")
    }
}

/// Executes the MIDI Panic function (All Notes Off).
#[utoipa::path(
    post, path = "/panic", tag = "General",
    responses((status = 200, description = "Panic executed"))
)]
async fn panic(data: web::Data<ApiData>) -> impl Responder {
    let mut state = data.app_state.lock().unwrap();

    // Send the signal to the audio engine
    let _ = data.audio_tx.send(AppMessage::AllNotesOff);

    state.add_midi_log("API: Executed Panic (All Notes Off)".into());
    HttpResponse::Ok().json(serde_json::json!({"status": "success"}))
}

/// Returns a JSON list of all stops and their currently enabled virtual channels.
#[utoipa::path(
    get, path = "/stops", tag = "Stops",
    responses((status = 200, body = Vec<StopStatusResponse>))
)]
async fn get_stops(data: web::Data<ApiData>) -> impl Responder {
    let state = data.app_state.lock().unwrap();
    let mut response_list = Vec::with_capacity(state.organ.stops.len());

    for (i, stop) in state.organ.stops.iter().enumerate() {
        let mut active_channels = state
            .stop_channels
            .get(&i)
            .map(|set| set.iter().cloned().collect::<Vec<u8>>())
            .unwrap_or_default();
        active_channels.sort();

        // Prefer the division recorded on the stop itself (populated by the
        // organ loaders). Fall back to the first rank's division when the
        // stop doesn't carry one, which keeps older code paths working.
        let division = if !stop.division_id.is_empty() {
            stop.division_id.clone()
        } else {
            stop.rank_ids
                .first()
                .and_then(|rid| state.organ.ranks.get(rid))
                .map(|r| r.division_id.clone())
                .unwrap_or_default()
        };

        response_list.push(StopStatusResponse {
            index: i,
            name: stop.name.clone(),
            active_channels,
            division,
        });
    }
    HttpResponse::Ok().json(response_list)
}

/// Enables or disables a specific stop for a specific virtual MIDI channel.
#[utoipa::path(
    post, path = "/stops/{stop_id}/channels/{channel_id}", tag = "Stops",
    request_body = ChannelUpdateRequest,
    params(
        ("stop_id" = usize, Path, description = "Index of the stop"),
        ("channel_id" = u8, Path, description = "Virtual MIDI Channel (0-15)")
    ),
    responses((status = 200), (status = 404), (status = 400))
)]
async fn update_stop_channel(
    path: web::Path<(usize, u8)>,
    body: web::Json<ChannelUpdateRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let (stop_index, channel_id) = path.into_inner();
    if channel_id > 15 {
        return HttpResponse::BadRequest().body("Channel ID > 15");
    }

    let mut state = data.app_state.lock().unwrap();
    if stop_index >= state.organ.stops.len() {
        return HttpResponse::NotFound().finish();
    }

    match state.set_stop_channel_state(stop_index, channel_id, body.active, &data.audio_tx) {
        Ok(_) => {
            let action = if body.active { "Enabled" } else { "Disabled" };
            state.add_midi_log(format!(
                "API: {} Stop {} for Ch {}",
                action,
                stop_index,
                channel_id + 1
            ));
            HttpResponse::Ok().json(serde_json::json!({ "status": "success" }))
        }
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
    }
}

/// Recalls a stop mapping preset (1-12).
#[utoipa::path(
    post, path = "/presets/{slot_id}/load", tag = "Presets",
    params(
        ("slot_id" = usize, Path, description = "Preset Slot ID (1-12)")
    ),
    responses((status = 200), (status = 404))
)]
async fn load_preset(path: web::Path<usize>, data: web::Data<ApiData>) -> impl Responder {
    let slot_id = path.into_inner();
    if !(1..=12).contains(&slot_id) {
        return HttpResponse::BadRequest().body("Invalid slot");
    }

    let mut state = data.app_state.lock().unwrap();
    match state.recall_preset(slot_id - 1, &data.audio_tx) {
        Ok(_) => {
            if state.presets[slot_id - 1].is_some() {
                state.add_midi_log(format!("API: Loaded Preset F{}", slot_id));
                HttpResponse::Ok().json(serde_json::json!({ "status": "success" }))
            } else {
                HttpResponse::NotFound().body("Preset empty")
            }
        }
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
    }
}

/// Saves the current mapping to a preset (1-12).
#[utoipa::path(
    post, path = "/presets/{slot_id}/save", tag = "Presets",
    request_body = PresetSaveRequest,
    params(
        ("slot_id" = usize, Path, description = "Preset Slot ID (1-12)")
    ),
    responses((status = 200))
)]
async fn save_preset(
    path: web::Path<usize>,
    body: web::Json<PresetSaveRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let slot_id = path.into_inner();
    if !(1..=12).contains(&slot_id) {
        return HttpResponse::BadRequest().body("Invalid slot");
    }

    let mut state = data.app_state.lock().unwrap();
    state.save_preset(slot_id - 1, body.name.clone());
    state.add_midi_log(format!("API: Saved Preset F{} as '{}'", slot_id, body.name));
    HttpResponse::Ok().json(serde_json::json!({ "status": "success" }))
}

// --- Audio & Config Handlers ---

/// Get current audio settings.
#[utoipa::path(
    get, path = "/audio/settings", tag = "Audio",
    responses((status = 200, body = AudioSettingsResponse))
)]
async fn get_audio_settings(data: web::Data<ApiData>) -> impl Responder {
    let state = data.app_state.lock().unwrap();
    let resp = AudioSettingsResponse {
        gain: state.gain,
        polyphony: state.polyphony,
        reverb_mix: state.reverb_mix,
        active_reverb_index: state.selected_reverb_index,
        is_recording_midi: state.is_recording_midi,
        is_recording_audio: state.is_recording_audio,
    };
    HttpResponse::Ok().json(resp)
}

/// Set Master Gain (0.0 - 2.0).
#[utoipa::path(
    post, path = "/audio/gain", tag = "Audio",
    request_body = ValueRequest,
    responses((status = 200))
)]
async fn set_gain(body: web::Json<ValueRequest>, data: web::Data<ApiData>) -> impl Responder {
    let gain = {
        let mut state = data.app_state.lock().unwrap();
        state.gain = body.value.clamp(0.0, 2.0);
        let _ = data.audio_tx.send(AppMessage::SetGain(state.gain));
        state.persist_settings();
        state.gain
    };
    broadcast(&data, WsMessage::AudioChanged);
    HttpResponse::Ok().json(serde_json::json!({"status": "success", "gain": gain}))
}

/// Set Polyphony limit (minimum 1).
#[utoipa::path(
    post, path = "/audio/polyphony", tag = "Audio",
    request_body = ValueRequest,
    responses((status = 200))
)]
async fn set_polyphony(body: web::Json<ValueRequest>, data: web::Data<ApiData>) -> impl Responder {
    let polyphony = {
        let mut state = data.app_state.lock().unwrap();
        state.polyphony = (body.value as usize).max(1);
        let _ = data
            .audio_tx
            .send(AppMessage::SetPolyphony(state.polyphony));
        state.persist_settings();
        state.polyphony
    };
    broadcast(&data, WsMessage::AudioChanged);
    HttpResponse::Ok().json(serde_json::json!({"status": "success", "polyphony": polyphony}))
}

/// Start or Stop MIDI Recording.
#[utoipa::path(
    post, path = "/record/midi", tag = "Recording",
    request_body = ChannelUpdateRequest, 
    responses((status = 200))
)]
async fn start_stop_midi_recording(
    body: web::Json<ChannelUpdateRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let active = body.active;
    {
        let mut state = data.app_state.lock().unwrap();
        state.is_recording_midi = active;
        if active {
            let _ = data.audio_tx.send(AppMessage::StartMidiRecording);
            state.add_midi_log("API: Started MIDI Recording".into());
        } else {
            let _ = data.audio_tx.send(AppMessage::StopMidiRecording);
            state.add_midi_log("API: Stopped MIDI Recording".into());
        }
    }
    broadcast(&data, WsMessage::AudioChanged);
    HttpResponse::Ok().json(serde_json::json!({"status": "success", "recording_midi": active}))
}

/// Start or Stop Audio (WAV) Recording.
#[utoipa::path(
    post, path = "/record/audio", tag = "Recording",
    request_body = ChannelUpdateRequest, 
    responses((status = 200))
)]
async fn start_stop_audio_recording(
    body: web::Json<ChannelUpdateRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let active = body.active;
    {
        let mut state = data.app_state.lock().unwrap();
        state.is_recording_audio = active;
        if active {
            let _ = data.audio_tx.send(AppMessage::StartAudioRecording);
            state.add_midi_log("API: Started Audio Recording".into());
        } else {
            let _ = data.audio_tx.send(AppMessage::StopAudioRecording);
            state.add_midi_log("API: Stopped Audio Recording".into());
        }
    }
    broadcast(&data, WsMessage::AudioChanged);
    HttpResponse::Ok().json(serde_json::json!({"status": "success", "recording_audio": active}))
}

/// Get available Impulse Response (Reverb) files.
#[utoipa::path(
    get, path = "/audio/reverbs", tag = "Audio",
    responses((status = 200, body = Vec<ReverbEntry>))
)]
async fn get_reverbs(data: web::Data<ApiData>) -> impl Responder {
    let list: Vec<ReverbEntry> = data
        .reverb_files
        .iter()
        .enumerate()
        .map(|(i, (name, _))| ReverbEntry {
            index: i,
            name: name.clone(),
        })
        .collect();
    HttpResponse::Ok().json(list)
}

/// Set active Reverb by index (-1 to disable).
#[utoipa::path(
    post, path = "/audio/reverbs/select", tag = "Audio",
    request_body = ReverbRequest,
    responses((status = 200))
)]
async fn set_reverb(body: web::Json<ReverbRequest>, data: web::Data<ApiData>) -> impl Responder {
    let idx = body.index;

    if idx < 0 {
        {
            let mut state = data.app_state.lock().unwrap();
            state.selected_reverb_index = None;
            let _ = data.audio_tx.send(AppMessage::SetReverbWetDry(0.0));
            state.persist_settings();
        }
        broadcast(&data, WsMessage::AudioChanged);
        return HttpResponse::Ok().json(serde_json::json!({"status": "disabled"}));
    }

    let u_idx = idx as usize;
    if u_idx >= data.reverb_files.len() {
        return HttpResponse::BadRequest().body("Invalid reverb index");
    }

    let (name, path) = &data.reverb_files[u_idx];
    {
        let mut state = data.app_state.lock().unwrap();
        state.selected_reverb_index = Some(u_idx);
        let _ = data.audio_tx.send(AppMessage::SetReverbIr(path.clone()));
        let _ = data
            .audio_tx
            .send(AppMessage::SetReverbWetDry(state.reverb_mix));
        state.persist_settings();
        state.add_midi_log(format!("API: Reverb set to '{}'", name));
    }
    broadcast(&data, WsMessage::AudioChanged);

    HttpResponse::Ok().json(serde_json::json!({"status": "success", "reverb": name}))
}

/// Set Reverb Mix (0.0 - 1.0).
#[utoipa::path(
    post, path = "/audio/reverbs/mix", tag = "Audio",
    request_body = ReverbMixRequest,
    responses((status = 200))
)]
async fn set_reverb_mix(
    body: web::Json<ReverbMixRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let mix = {
        let mut state = data.app_state.lock().unwrap();
        state.reverb_mix = body.mix.clamp(0.0, 1.0);
        let _ = data
            .audio_tx
            .send(AppMessage::SetReverbWetDry(state.reverb_mix));
        state.persist_settings();
        state.reverb_mix
    };
    broadcast(&data, WsMessage::AudioChanged);
    HttpResponse::Ok().json(serde_json::json!({"status": "success", "mix": mix}))
}

/// Get list of Tremulants and their status.
#[utoipa::path(
    get, path = "/tremulants", tag = "Tremulants",
    responses((status = 200, body = Vec<TremulantResponse>))
)]
async fn get_tremulants(data: web::Data<ApiData>) -> impl Responder {
    let state = data.app_state.lock().unwrap();
    let mut list = Vec::new();

    let mut trem_ids: Vec<_> = state.organ.tremulants.keys().collect();
    trem_ids.sort();

    for id in trem_ids {
        let trem = &state.organ.tremulants[id];
        let active = state.active_tremulants.contains(id);
        list.push(TremulantResponse {
            id: id.clone(),
            name: trem.name.clone(),
            active,
        });
    }
    HttpResponse::Ok().json(list)
}

/// Enable/Disable a Tremulant by ID.
#[utoipa::path(
    post, path = "/tremulants/{trem_id}", tag = "Tremulants",
    request_body = TremulantSetRequest,
    params(
        ("trem_id" = String, Path, description = "Tremulant ID")
    ),
    responses((status = 200), (status = 404))
)]
async fn set_tremulant(
    path: web::Path<String>,
    body: web::Json<TremulantSetRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let trem_id = path.into_inner();
    let mut state = data.app_state.lock().unwrap();

    if !state.organ.tremulants.contains_key(&trem_id) {
        return HttpResponse::NotFound().body("Tremulant ID not found");
    }

    state.set_tremulant_active(trem_id.clone(), body.active, &data.audio_tx);

    let action = if body.active { "Enabled" } else { "Disabled" };
    state.add_midi_log(format!("API: {} Tremulant '{}'", action, trem_id));

    HttpResponse::Ok().json(serde_json::json!({"status": "success"}))
}

/// Lists all 12 preset slots with their names (if any) and occupied state.
#[utoipa::path(
    get, path = "/presets", tag = "Presets",
    responses((status = 200, body = Vec<PresetSlotResponse>))
)]
async fn get_presets(data: web::Data<ApiData>) -> impl Responder {
    let state = data.app_state.lock().unwrap();
    let last_loaded = state.last_recalled_preset_slot;
    let mut list = Vec::with_capacity(state.presets.len());
    for (i, slot) in state.presets.iter().enumerate() {
        let slot_num = i + 1;
        list.push(PresetSlotResponse {
            slot: slot_num,
            name: slot.as_ref().map(|p| p.name.clone()),
            occupied: slot.is_some(),
            is_last_loaded: last_loaded == Some(slot_num),
        });
    }
    HttpResponse::Ok().json(list)
}

fn describe_event(event: &MidiEventSpec) -> String {
    match event {
        MidiEventSpec::Note {
            channel,
            note,
            is_note_off,
        } => format!(
            "Ch{} Note {} ({})",
            channel + 1,
            note,
            if *is_note_off { "Off" } else { "On" }
        ),
        MidiEventSpec::SysEx(bytes) => {
            let hex: Vec<String> = bytes.iter().map(|b| format!("{:02X}", b)).collect();
            format!("SysEx: {}", hex.join(" "))
        }
    }
}

const WEB_LEARN_TIMEOUT: Duration = Duration::from_secs(30);

/// Begins a web-driven MIDI learn session. Only one session can be active
/// at a time; starting a new one cancels the previous.
#[utoipa::path(
    post, path = "/midi-learn/start", tag = "MIDI Learn",
    request_body = MidiLearnStartRequest,
    responses((status = 200, body = MidiLearnStatusResponse), (status = 400), (status = 404))
)]
async fn midi_learn_start(
    body: web::Json<MidiLearnStartRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let mut state = data.app_state.lock().unwrap();

    let (target, target_name) = match body.target.as_str() {
        "stop" => {
            let stop_index = match body.stop_index {
                Some(i) => i,
                None => return HttpResponse::BadRequest().body("stop_index is required"),
            };
            let channel = match body.channel {
                Some(c) if c <= 15 => c,
                _ => return HttpResponse::BadRequest().body("channel (0-15) is required"),
            };
            let is_enable = body.is_enable.unwrap_or(true);
            let stop_name = match state.organ.stops.get(stop_index) {
                Some(s) => s.name.clone(),
                None => return HttpResponse::NotFound().body("Stop not found"),
            };
            let label = format!(
                "Stop '{}' Ch{} ({})",
                stop_name,
                channel + 1,
                if is_enable { "Enable" } else { "Disable" }
            );
            (
                WebLearnTarget::Stop {
                    stop_index,
                    channel,
                    is_enable,
                },
                label,
            )
        }
        "tremulant" => {
            let id = match body.tremulant_id.clone() {
                Some(s) => s,
                None => return HttpResponse::BadRequest().body("tremulant_id is required"),
            };
            if !state.organ.tremulants.contains_key(&id) {
                return HttpResponse::NotFound().body("Tremulant not found");
            }
            let is_enable = body.is_enable.unwrap_or(true);
            let label = format!(
                "Tremulant '{}' ({})",
                id,
                if is_enable { "Enable" } else { "Disable" }
            );
            (
                WebLearnTarget::Tremulant { id, is_enable },
                label,
            )
        }
        "preset" => {
            let slot = match body.preset_slot {
                Some(s) if (1..=12).contains(&s) => s,
                _ => return HttpResponse::BadRequest().body("preset_slot must be 1..=12"),
            };
            (
                WebLearnTarget::Preset {
                    slot_index: slot - 1,
                },
                format!("Preset F{}", slot),
            )
        }
        other => {
            return HttpResponse::BadRequest()
                .body(format!("Unknown target type: {}", other));
        }
    };

    state.web_learn_session = Some(WebLearnSession {
        target,
        target_name: target_name.clone(),
        started_at: Instant::now(),
    });
    drop(state);

    broadcast(
        &data,
        WsMessage::MidiLearn {
            state: "waiting".into(),
            target_name: Some(target_name.clone()),
            event_description: None,
        },
    );

    HttpResponse::Ok().json(MidiLearnStatusResponse {
        state: "waiting".into(),
        target_name: Some(target_name),
        event_description: None,
    })
}

/// Returns the status of the current web MIDI-learn session. If a MIDI event
/// has been received since the session started, the binding is persisted and
/// the session transitions to "captured" before being cleared.
#[utoipa::path(
    get, path = "/midi-learn", tag = "MIDI Learn",
    responses((status = 200, body = MidiLearnStatusResponse))
)]
/// Inspects the active learn session and resolves it if a MIDI event has
/// arrived since the session started, or if the timeout has elapsed. Returns
/// a status response when the session transitioned (captured / timed_out);
/// returns None when nothing changed.
fn tick_learn_session(state: &mut AppState) -> Option<MidiLearnStatusResponse> {
    let session = state.web_learn_session.as_ref()?.clone();

    if session.started_at.elapsed() > WEB_LEARN_TIMEOUT {
        state.web_learn_session = None;
        return Some(MidiLearnStatusResponse {
            state: "timed_out".into(),
            target_name: Some(session.target_name),
            event_description: None,
        });
    }

    let event = state
        .last_midi_event_received
        .as_ref()
        .filter(|(_, t)| *t > session.started_at)
        .map(|(e, _)| e.clone())?;

    let description = describe_event(&event);
    let organ_name = state.organ.name.clone();
    match &session.target {
        WebLearnTarget::Stop {
            stop_index,
            channel,
            is_enable,
        } => {
            state
                .midi_control_map
                .learn_stop(*stop_index, *channel, event, *is_enable);
        }
        WebLearnTarget::Tremulant { id, is_enable } => {
            state
                .midi_control_map
                .learn_tremulant(id.clone(), event, *is_enable);
        }
        WebLearnTarget::Preset { slot_index } => {
            state.midi_control_map.learn_preset(*slot_index, event);
        }
    }
    let _ = state.midi_control_map.save(&organ_name);
    state.add_midi_log(format!(
        "Web MIDI Learn: {} -> {}",
        session.target_name, description
    ));
    state.web_learn_session = None;

    Some(MidiLearnStatusResponse {
        state: "captured".into(),
        target_name: Some(session.target_name),
        event_description: Some(description),
    })
}

/// Returns the status of the current web MIDI-learn session. The web client
/// normally receives this via the WebSocket; this endpoint is also useful as
/// a fallback or for non-WS clients.
#[utoipa::path(
    get, path = "/midi-learn", tag = "MIDI Learn",
    responses((status = 200, body = MidiLearnStatusResponse))
)]
async fn midi_learn_status(data: web::Data<ApiData>) -> impl Responder {
    let resp = {
        let mut state = data.app_state.lock().unwrap();
        if let Some(transitioned) = tick_learn_session(&mut state) {
            transitioned
        } else if let Some(s) = &state.web_learn_session {
            MidiLearnStatusResponse {
                state: "waiting".into(),
                target_name: Some(s.target_name.clone()),
                event_description: None,
            }
        } else {
            MidiLearnStatusResponse {
                state: "idle".into(),
                target_name: None,
                event_description: None,
            }
        }
    };

    if resp.state == "captured" || resp.state == "timed_out" {
        broadcast(
            &data,
            WsMessage::MidiLearn {
                state: resp.state.clone(),
                target_name: resp.target_name.clone(),
                event_description: resp.event_description.clone(),
            },
        );
    }

    HttpResponse::Ok().json(resp)
}

/// Clears the learned MIDI binding for a stop+channel pair. Removes both
/// enable and disable triggers if present. Idempotent.
#[utoipa::path(
    delete, path = "/midi-bindings/stop/{stop_index}/{channel}", tag = "MIDI Learn",
    params(
        ("stop_index" = usize, Path, description = "Index of the stop"),
        ("channel" = u8, Path, description = "Virtual MIDI Channel (0-15)")
    ),
    responses((status = 200))
)]
async fn clear_stop_binding(
    path: web::Path<(usize, u8)>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let (stop_index, channel) = path.into_inner();
    if channel > 15 {
        return HttpResponse::BadRequest().body("Channel ID > 15");
    }
    let mut state = data.app_state.lock().unwrap();
    state.midi_control_map.clear_stop(stop_index, channel);
    let organ_name = state.organ.name.clone();
    let _ = state.midi_control_map.save(&organ_name);
    state.add_midi_log(format!(
        "Cleared MIDI binding for stop {} ch {}",
        stop_index,
        channel + 1
    ));
    HttpResponse::Ok().json(serde_json::json!({"status": "cleared"}))
}

/// Clears the learned MIDI binding for a tremulant.
#[utoipa::path(
    delete, path = "/midi-bindings/tremulant/{trem_id}", tag = "MIDI Learn",
    params(("trem_id" = String, Path, description = "Tremulant ID")),
    responses((status = 200))
)]
async fn clear_tremulant_binding(
    path: web::Path<String>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let trem_id = path.into_inner();
    let mut state = data.app_state.lock().unwrap();
    state.midi_control_map.clear_tremulant(&trem_id);
    let organ_name = state.organ.name.clone();
    let _ = state.midi_control_map.save(&organ_name);
    state.add_midi_log(format!("Cleared MIDI binding for tremulant '{}'", trem_id));
    HttpResponse::Ok().json(serde_json::json!({"status": "cleared"}))
}

/// Clears the learned MIDI binding for a preset slot (1-based).
#[utoipa::path(
    delete, path = "/midi-bindings/preset/{slot}", tag = "MIDI Learn",
    params(("slot" = usize, Path, description = "Preset slot ID (1-12)")),
    responses((status = 200), (status = 400))
)]
async fn clear_preset_binding(
    path: web::Path<usize>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let slot = path.into_inner();
    if !(1..=12).contains(&slot) {
        return HttpResponse::BadRequest().body("Invalid slot");
    }
    let mut state = data.app_state.lock().unwrap();
    state.midi_control_map.clear_preset(slot - 1);
    let organ_name = state.organ.name.clone();
    let _ = state.midi_control_map.save(&organ_name);
    state.add_midi_log(format!("Cleared MIDI binding for preset F{}", slot));
    HttpResponse::Ok().json(serde_json::json!({"status": "cleared"}))
}

/// Cancels any active web MIDI-learn session.
#[utoipa::path(
    post, path = "/midi-learn/cancel", tag = "MIDI Learn",
    responses((status = 200))
)]
async fn midi_learn_cancel(data: web::Data<ApiData>) -> impl Responder {
    {
        let mut state = data.app_state.lock().unwrap();
        state.web_learn_session = None;
    }
    broadcast(
        &data,
        WsMessage::MidiLearn {
            state: "idle".into(),
            target_name: None,
            event_description: None,
        },
    );
    HttpResponse::Ok().json(serde_json::json!({"status": "cancelled"}))
}

// --- WebSocket ---

/// WebSocket endpoint that streams state-change hints to the connected
/// client. Each broadcast message is forwarded as a single JSON text frame.
async fn ws_handler(
    req: HttpRequest,
    stream: web::Payload,
    data: web::Data<ApiData>,
) -> Result<HttpResponse, actix_web::Error> {
    let (response, mut session, mut msg_stream) = actix_ws::handle(&req, stream)?;
    let mut rx = data.ws_tx.subscribe();

    actix_web::rt::spawn(async move {
        // Immediately tell this client to reload everything. This is the
        // authoritative signal: "you're talking to the current server, the
        // data behind the REST endpoints is whatever this server has now."
        // Critical after an organ switch — the new server broadcasts this
        // to every client that reconnects to it, so the UI refreshes even
        // if an old in-flight fetch races with the reconnect.
        if let Ok(json) = serde_json::to_string(&WsMessage::Refetch) {
            if session.text(json).await.is_err() {
                return;
            }
        }

        loop {
            tokio::select! {
                ws_msg = msg_stream.next() => match ws_msg {
                    Some(Ok(actix_ws::Message::Ping(b))) => {
                        if session.pong(&b).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(actix_ws::Message::Close(reason))) => {
                        let _ = session.close(reason).await;
                        break;
                    }
                    Some(Err(_)) | None => break,
                    _ => {}
                },
                bcast = rx.recv() => match bcast {
                    Ok(msg) => {
                        let is_restart = matches!(msg, WsMessage::ServerRestarting);
                        if let Ok(json) = serde_json::to_string(&msg) {
                            if session.text(json).await.is_err() {
                                break;
                            }
                        }
                        if is_restart {
                            // Close the session ourselves so the client
                            // sees the close event quickly and reconnects,
                            // instead of waiting for a network timeout
                            // when the process exits.
                            let _ = session.close(None).await;
                            break;
                        }
                    }
                    // If we lagged, just keep going — the client refetches state.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                },
            }
        }
    });

    Ok(response)
}

// --- Listen-address discovery ---

/// Returns the list of IP addresses the web UI can be reached at, with
/// loopback (127.0.0.1) first followed by the addresses of every up
/// network interface. Sorted so IPv4 comes before IPv6 within each group.
fn list_listen_addresses() -> Vec<std::net::IpAddr> {
    use std::net::{IpAddr, Ipv4Addr};
    let mut out: Vec<IpAddr> = vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))];
    if let Ok(addrs) = local_ip_address::list_afinet_netifas() {
        let mut iface: Vec<IpAddr> = addrs
            .into_iter()
            .map(|(_name, ip)| ip)
            .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
            .filter(|ip| match ip {
                // Drop link-local IPv6 (fe80::) — they need a scope id
                // and aren't useful for connecting to the web UI.
                IpAddr::V6(v6) => {
                    let s = v6.segments();
                    !(s[0] & 0xffc0 == 0xfe80)
                }
                IpAddr::V4(_) => true,
            })
            .collect();
        iface.sort_by_key(|ip| match ip {
            IpAddr::V4(_) => 0,
            IpAddr::V6(_) => 1,
        });
        iface.dedup();
        out.extend(iface);
    }
    out
}

/// Format an IP address for inclusion in a URL (wraps IPv6 in brackets).
fn format_host(ip: &std::net::IpAddr) -> String {
    match ip {
        std::net::IpAddr::V4(v4) => v4.to_string(),
        std::net::IpAddr::V6(v6) => format!("[{}]", v6),
    }
}

// --- Embedded Web UI ---

const WEB_UI_HTML: &str = include_str!("../assets/web/index.html");
const WEB_UI_CSS: &str = include_str!("../assets/web/app.css");
const WEB_UI_JS: &str = include_str!("../assets/web/app.js");

async fn web_ui_index() -> impl Responder {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(WEB_UI_HTML)
}

async fn web_ui_css() -> impl Responder {
    HttpResponse::Ok()
        .content_type("text/css; charset=utf-8")
        .body(WEB_UI_CSS)
}

async fn web_ui_js() -> impl Responder {
    HttpResponse::Ok()
        .content_type("application/javascript; charset=utf-8")
        .body(WEB_UI_JS)
}

// --- Server Launcher ---

pub fn start_api_server(
    app_state: Arc<Mutex<AppState>>,
    audio_tx: Sender<AppMessage>,
    port: u16,
    exit_action: Arc<Mutex<MainLoopAction>>,
    ws_tx: broadcast::Sender<WsMessage>,
) -> ApiServerHandle {
    let reverb_files = Arc::new(config::get_available_ir_files());

    // Background ticker: detects MIDI-learn captures driven by external MIDI
    // input and broadcasts the transition to web clients. Exits when
    // ApiServerHandle is dropped (e.g. on organ reload), which prevents
    // stale tickers from leaking Arc<Mutex<AppState>> across reloads.
    let ticker_stop = Arc::new(AtomicBool::new(false));
    {
        let ticker_state = app_state.clone();
        let ticker_ws = ws_tx.clone();
        let ticker_stop = ticker_stop.clone();
        std::thread::spawn(move || {
            while !ticker_stop.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(100));
                if ticker_stop.load(Ordering::Acquire) {
                    break;
                }
                let resp_opt = {
                    let mut state = ticker_state.lock().unwrap();
                    if state.web_learn_session.is_some() {
                        tick_learn_session(&mut state)
                    } else {
                        None
                    }
                };
                if let Some(resp) = resp_opt {
                    let _ = ticker_ws.send(WsMessage::MidiLearn {
                        state: resp.state,
                        target_name: resp.target_name,
                        event_description: resp.event_description,
                    });
                }
            }
            log::info!("[ApiTicker] Stop signal received. Exiting.");
        });
    }

    // Create a channel to send the ServerHandle from the background thread back to here
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let sys = actix_web::rt::System::new();

        let server_data = web::Data::new(ApiData {
            app_state,
            audio_tx,
            exit_action,
            reverb_files,
            ws_tx,
        });

        let openapi = ApiDoc::openapi();

        let server = HttpServer::new(move || {
            App::new()
                .app_data(server_data.clone())
                .service(
                    SwaggerUi::new("/swagger-ui/{_:.*}")
                        .url("/api-docs/openapi.json", openapi.clone()),
                )
                .route("/", web::get().to(index))
                // Embedded Web UI
                .route("/ui", web::get().to(web_ui_index))
                .route("/ui/", web::get().to(web_ui_index))
                .route("/ui/app.css", web::get().to(web_ui_css))
                .route("/ui/app.js", web::get().to(web_ui_js))
                // Mode discovery
                .route("/mode", web::get().to(mode))
                .route("/i18n", web::get().to(i18n))
                // Live updates
                .route("/ws", web::get().to(ws_handler))
                // General
                .route("/organ", web::get().to(get_organ_info))
                .route("/organs", web::get().to(get_organ_library))
                .route("/organs/load", web::post().to(load_organ))
                .route("/panic", web::post().to(panic))
                // Stops
                .route("/stops", web::get().to(get_stops))
                .route(
                    "/stops/{stop_id}/channels/{channel_id}",
                    web::post().to(update_stop_channel),
                )
                // Presets
                .route("/presets", web::get().to(get_presets))
                .route("/presets/{slot_id}/load", web::post().to(load_preset))
                .route("/presets/{slot_id}/save", web::post().to(save_preset))
                // Audio
                .route("/audio/settings", web::get().to(get_audio_settings))
                .route("/audio/gain", web::post().to(set_gain))
                .route("/audio/polyphony", web::post().to(set_polyphony))
                .route("/audio/reverbs", web::get().to(get_reverbs))
                .route("/audio/reverbs/select", web::post().to(set_reverb))
                .route("/audio/reverbs/mix", web::post().to(set_reverb_mix))
                // Recording
                .route("/record/midi", web::post().to(start_stop_midi_recording))
                .route("/record/audio", web::post().to(start_stop_audio_recording))
                // Tremulants
                .route("/tremulants", web::get().to(get_tremulants))
                .route("/tremulants/{trem_id}", web::post().to(set_tremulant))
                // MIDI Learn (web flow)
                .route("/midi-learn", web::get().to(midi_learn_status))
                .route("/midi-learn/start", web::post().to(midi_learn_start))
                .route("/midi-learn/cancel", web::post().to(midi_learn_cancel))
                .route(
                    "/midi-bindings/stop/{stop_index}/{channel}",
                    web::delete().to(clear_stop_binding),
                )
                .route(
                    "/midi-bindings/tremulant/{trem_id}",
                    web::delete().to(clear_tremulant_binding),
                )
                .route(
                    "/midi-bindings/preset/{slot}",
                    web::delete().to(clear_preset_binding),
                )
        })
        .bind(("0.0.0.0", port));

        match server {
            Ok(bound_server) => {
                let addrs = list_listen_addresses();
                println!("REST API server listening on port {}.", port);
                println!("Web UI available at:");
                for addr in &addrs {
                    println!("  http://{}:{}/ui/", format_host(addr), port);
                }
                println!("Swagger UI available at:");
                for addr in &addrs {
                    println!("  http://{}:{}/swagger-ui/", format_host(addr), port);
                }
                let server = bound_server.run();
                let handle = server.handle();
                let _ = tx.send(handle);
                if let Err(e) = sys.block_on(server) {
                    eprintln!("API Server Error: {}", e);
                }
            }
            Err(e) => eprintln!("Failed to bind API server to port {}: {}", port, e),
        }
    });
    // Wait for the server to start up and give us the handle
    let handle = rx
        .recv()
        .expect("Failed to start API server or receive handle");

    ApiServerHandle {
        handle,
        ticker_stop,
    }
}
