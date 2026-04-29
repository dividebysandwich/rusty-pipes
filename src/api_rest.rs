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

use crate::app::{AppMessage, LoadingState, MainLoopAction, WsMessage};
use crate::app_state::{AppState, WebLearnSession, WebLearnTarget};
use crate::audio::get_supported_sample_rates;
use crate::config::{
    self, ConfigShared, MidiDeviceConfig, MidiEventSpec, MidiMappingMode, OrganProfile,
    load_organ_library,
};
use crate::gui_config::build_runtime_config;

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
//
// The server lives for the entire program lifetime. Its `mode` switches as
// the application transitions Idle → Config → (loading) → Play → (loading) →
// Play …, and each handler short-circuits with 503 when called in the wrong
// mode. WebSocket connections persist across mode changes so the loading
// modal can be driven through every transition.

/// Snapshot of the per-mode context. `Idle` is the boot state, used during
/// transitions and when no organ is loaded.
pub enum Mode {
    Idle,
    Config(Arc<Mutex<ConfigShared>>),
    Play(PlayContext),
}

/// All the play-mode resources captured by play-only handlers. Cloning is
/// cheap because every field is either an `Arc` or an `mpsc::Sender`.
#[derive(Clone)]
pub struct PlayContext {
    pub app_state: Arc<Mutex<AppState>>,
    pub audio_tx: Sender<AppMessage>,
    pub exit_action: Arc<Mutex<MainLoopAction>>,
    pub reverb_files: Arc<Vec<(String, PathBuf)>>,
}

pub struct ApiData {
    pub mode: Arc<Mutex<Mode>>,
    pub loading_state: Arc<Mutex<LoadingState>>,
    pub ws_tx: broadcast::Sender<WsMessage>,
}

fn broadcast(data: &web::Data<ApiData>, msg: WsMessage) {
    let _ = data.ws_tx.send(msg);
}

/// Returns a cheap clone of the play-mode context, or `None` if the server
/// is currently in another mode.
fn get_play(data: &web::Data<ApiData>) -> Option<PlayContext> {
    let mode = data.mode.lock().unwrap();
    match &*mode {
        Mode::Play(ctx) => Some(ctx.clone()),
        _ => None,
    }
}

fn get_config(data: &web::Data<ApiData>) -> Option<Arc<Mutex<ConfigShared>>> {
    let mode = data.mode.lock().unwrap();
    match &*mode {
        Mode::Config(shared) => Some(Arc::clone(shared)),
        _ => None,
    }
}

/// Mode-name for the `/mode` endpoint.
fn current_mode_name(data: &web::Data<ApiData>) -> &'static str {
    let mode = data.mode.lock().unwrap();
    match &*mode {
        Mode::Idle => "idle",
        Mode::Config(_) => "config",
        Mode::Play(_) => "play",
    }
}

macro_rules! require_play {
    ($data:expr) => {
        match get_play(&$data) {
            Some(p) => p,
            None => {
                return HttpResponse::ServiceUnavailable()
                    .body("Server is not currently in play mode");
            }
        }
    };
}

macro_rules! require_config {
    ($data:expr) => {
        match get_config(&$data) {
            Some(c) => c,
            None => {
                return HttpResponse::ServiceUnavailable()
                    .body("Server is not currently in configuration mode");
            }
        }
    };
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
async fn get_mode(data: web::Data<ApiData>) -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({"mode": current_mode_name(&data)}))
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
    let play = require_play!(data);
    let state = play.app_state.lock().unwrap();
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
    let play = require_play!(data);
    let target_path_str = &body.path;
    let lib = match load_organ_library() {
        Ok(l) => l,
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };

    let found = lib
        .organs
        .iter()
        .find(|o| o.path.to_string_lossy() == *target_path_str);

    if let Some(profile) = found {
        log::info!("API: Requesting reload of organ: {}", profile.name);

        // Now that the API server is long-lived (a single instance covers
        // the entire program lifetime), reloading no longer involves a
        // WebSocket close. The web UI shows the loading modal in response
        // to LoadingProgress events broadcast by the loader thread.

        // Signal the main loop to reload
        *play.exit_action.lock().unwrap() = MainLoopAction::ReloadOrgan {
            file: profile.path.clone(),
        };

        let _ = play.audio_tx.send(AppMessage::Quit);

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
    let play = require_play!(data);
    let mut state = play.app_state.lock().unwrap();

    let _ = play.audio_tx.send(AppMessage::AllNotesOff);

    state.add_midi_log("API: Executed Panic (All Notes Off)".into());
    HttpResponse::Ok().json(serde_json::json!({"status": "success"}))
}

/// Returns a JSON list of all stops and their currently enabled virtual channels.
#[utoipa::path(
    get, path = "/stops", tag = "Stops",
    responses((status = 200, body = Vec<StopStatusResponse>))
)]
async fn get_stops(data: web::Data<ApiData>) -> impl Responder {
    let play = require_play!(data);
    let state = play.app_state.lock().unwrap();
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
    let play = require_play!(data);
    let (stop_index, channel_id) = path.into_inner();
    if channel_id > 15 {
        return HttpResponse::BadRequest().body("Channel ID > 15");
    }

    let mut state = play.app_state.lock().unwrap();
    if stop_index >= state.organ.stops.len() {
        return HttpResponse::NotFound().finish();
    }

    match state.set_stop_channel_state(stop_index, channel_id, body.active, &play.audio_tx) {
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
    let play = require_play!(data);
    let slot_id = path.into_inner();
    if !(1..=12).contains(&slot_id) {
        return HttpResponse::BadRequest().body("Invalid slot");
    }

    let mut state = play.app_state.lock().unwrap();
    match state.recall_preset(slot_id - 1, &play.audio_tx) {
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
    let play = require_play!(data);
    let slot_id = path.into_inner();
    if !(1..=12).contains(&slot_id) {
        return HttpResponse::BadRequest().body("Invalid slot");
    }

    let mut state = play.app_state.lock().unwrap();
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
    let play = require_play!(data);
    let state = play.app_state.lock().unwrap();
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
    let play = require_play!(data);
    let gain = {
        let mut state = play.app_state.lock().unwrap();
        state.gain = body.value.clamp(0.0, 2.0);
        let _ = play.audio_tx.send(AppMessage::SetGain(state.gain));
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
    let play = require_play!(data);
    let polyphony = {
        let mut state = play.app_state.lock().unwrap();
        state.polyphony = (body.value as usize).max(1);
        let _ = play
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
    let play = require_play!(data);
    let active = body.active;
    {
        let mut state = play.app_state.lock().unwrap();
        state.is_recording_midi = active;
        if active {
            let _ = play.audio_tx.send(AppMessage::StartMidiRecording);
            state.add_midi_log("API: Started MIDI Recording".into());
        } else {
            let _ = play.audio_tx.send(AppMessage::StopMidiRecording);
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
    let play = require_play!(data);
    let active = body.active;
    {
        let mut state = play.app_state.lock().unwrap();
        state.is_recording_audio = active;
        if active {
            let _ = play.audio_tx.send(AppMessage::StartAudioRecording);
            state.add_midi_log("API: Started Audio Recording".into());
        } else {
            let _ = play.audio_tx.send(AppMessage::StopAudioRecording);
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
    let play = require_play!(data);
    let list: Vec<ReverbEntry> = play
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
    let play = require_play!(data);
    let idx = body.index;

    if idx < 0 {
        {
            let mut state = play.app_state.lock().unwrap();
            state.selected_reverb_index = None;
            let _ = play.audio_tx.send(AppMessage::SetReverbWetDry(0.0));
            state.persist_settings();
        }
        broadcast(&data, WsMessage::AudioChanged);
        return HttpResponse::Ok().json(serde_json::json!({"status": "disabled"}));
    }

    let u_idx = idx as usize;
    if u_idx >= play.reverb_files.len() {
        return HttpResponse::BadRequest().body("Invalid reverb index");
    }

    let (name, path) = &play.reverb_files[u_idx];
    {
        let mut state = play.app_state.lock().unwrap();
        state.selected_reverb_index = Some(u_idx);
        let _ = play.audio_tx.send(AppMessage::SetReverbIr(path.clone()));
        let _ = play
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
    let play = require_play!(data);
    let mix = {
        let mut state = play.app_state.lock().unwrap();
        state.reverb_mix = body.mix.clamp(0.0, 1.0);
        let _ = play
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
    let play = require_play!(data);
    let state = play.app_state.lock().unwrap();
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
    let play = require_play!(data);
    let trem_id = path.into_inner();
    let mut state = play.app_state.lock().unwrap();

    if !state.organ.tremulants.contains_key(&trem_id) {
        return HttpResponse::NotFound().body("Tremulant ID not found");
    }

    state.set_tremulant_active(trem_id.clone(), body.active, &play.audio_tx);

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
    let play = require_play!(data);
    let state = play.app_state.lock().unwrap();
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
    let play = require_play!(data);
    let mut state = play.app_state.lock().unwrap();

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
    let play = require_play!(data);
    let resp = {
        let mut state = play.app_state.lock().unwrap();
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
    let play = require_play!(data);
    let (stop_index, channel) = path.into_inner();
    if channel > 15 {
        return HttpResponse::BadRequest().body("Channel ID > 15");
    }
    let mut state = play.app_state.lock().unwrap();
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
    let play = require_play!(data);
    let trem_id = path.into_inner();
    let mut state = play.app_state.lock().unwrap();
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
    let play = require_play!(data);
    let slot = path.into_inner();
    if !(1..=12).contains(&slot) {
        return HttpResponse::BadRequest().body("Invalid slot");
    }
    let mut state = play.app_state.lock().unwrap();
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
    let play = require_play!(data);
    {
        let mut state = play.app_state.lock().unwrap();
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

// =========================================================================
// CONFIG-MODE HANDLERS
// =========================================================================
// These handlers are mirrors of the former api_config.rs endpoints. Each
// requires the server to be in `Mode::Config(...)`; otherwise it returns
// 503 Service Unavailable.

// --- Config response/request models ---

#[derive(Serialize)]
struct ConfigStateResponse {
    settings: crate::config::AppSettings,
    midi_file: Option<String>,
    available_audio_devices: Vec<String>,
    selected_audio_device_name: Option<String>,
    available_sample_rates: Vec<u32>,
    available_ir_files: Vec<ConfigIrFileEntry>,
    system_midi_ports: Vec<ConfigMidiPortEntry>,
    organ_library: Vec<ConfigOrganLibraryEntry>,
    last_used_organ: Option<String>,
    error_msg: Option<String>,
}

#[derive(Serialize)]
struct ConfigIrFileEntry {
    name: String,
    path: String,
}

#[derive(Serialize)]
struct ConfigMidiPortEntry {
    name: String,
}

#[derive(Serialize)]
struct ConfigOrganLibraryEntry {
    name: String,
    path: String,
}

#[derive(Deserialize)]
struct ConfigAudioDeviceRequest {
    name: Option<String>,
}

#[derive(Deserialize)]
struct ConfigSampleRateRequest {
    rate: u32,
}

#[derive(Deserialize)]
struct ConfigIrFileRequest {
    path: Option<String>,
}

#[derive(Deserialize)]
struct ConfigOrganRequest {
    path: String,
}

#[derive(Deserialize)]
struct ConfigAudioSettingsRequest {
    gain: Option<f32>,
    polyphony: Option<usize>,
    reverb_mix: Option<f32>,
    audio_buffer_frames: Option<usize>,
    max_ram_gb: Option<f32>,
    precache: Option<bool>,
    convert_to_16bit: Option<bool>,
    original_tuning: Option<bool>,
}

#[derive(Deserialize)]
struct ConfigMidiDeviceUpdateRequest {
    name: String,
    enabled: Option<bool>,
    mapping_mode: Option<String>,
    simple_target_channel: Option<u8>,
    complex_mapping: Option<Vec<u8>>,
}

#[derive(Deserialize)]
struct ConfigLocaleRequest {
    locale: String,
}

#[derive(Deserialize)]
struct BrowseQuery {
    path: Option<String>,
    exts: Option<String>,
}

#[derive(Serialize)]
struct BrowseEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: Option<u64>,
}

#[derive(Serialize)]
struct BrowseResponse {
    current_path: String,
    parent_path: Option<String>,
    entries: Vec<BrowseEntry>,
}

#[derive(Deserialize)]
struct AddOrganRequest {
    path: String,
    name: Option<String>,
}

#[derive(Deserialize)]
struct RemoveOrganRequest {
    path: String,
}

// --- Config handlers ---

async fn get_config_state(data: web::Data<ApiData>) -> impl Responder {
    let cfg = require_config!(data);
    let s = cfg.lock().unwrap();
    let st = &s.state;

    let library = load_organ_library().unwrap_or_default();
    let organ_library: Vec<ConfigOrganLibraryEntry> = library
        .organs
        .iter()
        .map(|p: &OrganProfile| ConfigOrganLibraryEntry {
            name: p.name.clone(),
            path: p.path.to_string_lossy().to_string(),
        })
        .collect();

    let last_used_organ = st
        .settings
        .organ_file
        .as_ref()
        .map(|p| p.to_string_lossy().to_string());

    let resp = ConfigStateResponse {
        settings: st.settings.clone(),
        midi_file: st.midi_file.as_ref().map(|p| p.to_string_lossy().to_string()),
        available_audio_devices: st.available_audio_devices.clone(),
        selected_audio_device_name: st.selected_audio_device_name.clone(),
        available_sample_rates: st.available_sample_rates.clone(),
        available_ir_files: st
            .available_ir_files
            .iter()
            .map(|(name, path)| ConfigIrFileEntry {
                name: name.clone(),
                path: path.to_string_lossy().to_string(),
            })
            .collect(),
        system_midi_ports: st
            .system_midi_ports
            .iter()
            .map(|(_, name)| ConfigMidiPortEntry { name: name.clone() })
            .collect(),
        organ_library,
        last_used_organ,
        error_msg: st.error_msg.clone(),
    };
    HttpResponse::Ok().json(resp)
}

async fn config_set_audio_device(
    body: web::Json<ConfigAudioDeviceRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let cfg = require_config!(data);
    let mut s = cfg.lock().unwrap();
    let st = &mut s.state;

    if let Some(name) = body.name.clone() {
        if !st.available_audio_devices.contains(&name) {
            return HttpResponse::BadRequest().body("Unknown audio device");
        }
        st.selected_audio_device_name = Some(name);
    } else {
        st.selected_audio_device_name = None;
    }

    if let Ok(rates) = get_supported_sample_rates(st.selected_audio_device_name.clone()) {
        st.available_sample_rates = rates;
        if !st.available_sample_rates.contains(&st.settings.sample_rate) {
            if let Some(&first) = st.available_sample_rates.first() {
                st.settings.sample_rate = first;
            }
        }
    }
    s.revision = s.revision.wrapping_add(1);
    drop(s);
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
}

async fn config_set_sample_rate(
    body: web::Json<ConfigSampleRateRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let cfg = require_config!(data);
    let mut s = cfg.lock().unwrap();
    if !s.state.available_sample_rates.contains(&body.rate) {
        return HttpResponse::BadRequest().body("Unsupported sample rate for selected device");
    }
    s.state.settings.sample_rate = body.rate;
    s.revision = s.revision.wrapping_add(1);
    drop(s);
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
}

async fn config_set_ir_file(
    body: web::Json<ConfigIrFileRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let cfg = require_config!(data);
    let mut s = cfg.lock().unwrap();
    let st = &mut s.state;
    match &body.path {
        None => st.settings.ir_file = None,
        Some(p) => {
            let matched = st
                .available_ir_files
                .iter()
                .find(|(_, path)| path.to_string_lossy() == *p)
                .cloned();
            if let Some((_, path)) = matched {
                st.settings.ir_file = Some(path);
            } else {
                let path = PathBuf::from(p);
                if !path.exists() || !path.is_file() {
                    return HttpResponse::BadRequest().body("File not found");
                }
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase());
                let ok = matches!(ext.as_deref(), Some("wav") | Some("flac") | Some("mp3"));
                if !ok {
                    return HttpResponse::BadRequest().body("Unsupported IR file extension");
                }
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Custom")
                    .to_string();
                if !st.available_ir_files.iter().any(|(_, p2)| p2 == &path) {
                    st.available_ir_files.push((name, path.clone()));
                }
                st.settings.ir_file = Some(path);
            }
        }
    }
    s.revision = s.revision.wrapping_add(1);
    drop(s);
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
}

async fn config_set_organ(
    body: web::Json<ConfigOrganRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let cfg = require_config!(data);
    let library = match load_organ_library() {
        Ok(l) => l,
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };
    let profile = library
        .organs
        .iter()
        .find(|o| o.path.to_string_lossy() == body.path);
    let profile = match profile {
        Some(p) => p,
        None => return HttpResponse::NotFound().body("Organ not in library"),
    };

    let mut s = cfg.lock().unwrap();
    s.state.settings.organ_file = Some(profile.path.clone());
    s.revision = s.revision.wrapping_add(1);
    drop(s);
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
}

async fn config_set_audio_settings(
    body: web::Json<ConfigAudioSettingsRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let cfg = require_config!(data);
    let mut s = cfg.lock().unwrap();
    let st = &mut s.state.settings;
    if let Some(v) = body.gain {
        st.gain = v.clamp(0.0, 1.0);
    }
    if let Some(v) = body.polyphony {
        st.polyphony = v.max(1);
    }
    if let Some(v) = body.reverb_mix {
        st.reverb_mix = v.clamp(0.0, 1.0);
    }
    if let Some(v) = body.audio_buffer_frames {
        st.audio_buffer_frames = v.clamp(32, 4096);
    }
    if let Some(v) = body.max_ram_gb {
        st.max_ram_gb = v.max(0.0);
    }
    if let Some(v) = body.precache {
        st.precache = v;
    }
    if let Some(v) = body.convert_to_16bit {
        st.convert_to_16bit = v;
    }
    if let Some(v) = body.original_tuning {
        st.original_tuning = v;
    }
    s.revision = s.revision.wrapping_add(1);
    drop(s);
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
}

async fn config_update_midi_device(
    body: web::Json<ConfigMidiDeviceUpdateRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let cfg = require_config!(data);
    let mut s = cfg.lock().unwrap();
    let dev = s
        .state
        .settings
        .midi_devices
        .iter_mut()
        .find(|d| d.name == body.name);
    let dev = match dev {
        Some(d) => d,
        None => {
            s.state.settings.midi_devices.push(MidiDeviceConfig {
                name: body.name.clone(),
                enabled: false,
                ..Default::default()
            });
            s.state.settings.midi_devices.last_mut().unwrap()
        }
    };

    if let Some(v) = body.enabled {
        dev.enabled = v;
    }
    if let Some(mode) = body.mapping_mode.as_deref() {
        match mode {
            "Simple" => dev.mapping_mode = MidiMappingMode::Simple,
            "Complex" => dev.mapping_mode = MidiMappingMode::Complex,
            _ => return HttpResponse::BadRequest().body("mapping_mode must be Simple or Complex"),
        }
    }
    if let Some(ch) = body.simple_target_channel {
        if ch > 15 {
            return HttpResponse::BadRequest().body("simple_target_channel must be 0..=15");
        }
        dev.simple_target_channel = ch;
    }
    if let Some(map) = &body.complex_mapping {
        if map.len() != 16 || map.iter().any(|&c| c > 15) {
            return HttpResponse::BadRequest()
                .body("complex_mapping must be a length-16 array with values 0..=15");
        }
        let mut arr = [0u8; 16];
        arr.copy_from_slice(map);
        dev.complex_mapping = arr;
    }
    s.revision = s.revision.wrapping_add(1);
    drop(s);
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
}

async fn config_rescan_midi(data: web::Data<ApiData>) -> impl Responder {
    let cfg = require_config!(data);
    let mut s = cfg.lock().unwrap();
    if let Err(e) = s.rescan_midi_ports() {
        return HttpResponse::InternalServerError().body(e.to_string());
    }
    drop(s);
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
}

async fn config_start(data: web::Data<ApiData>) -> impl Responder {
    let cfg = require_config!(data);
    let mut s = cfg.lock().unwrap();
    if s.state.settings.organ_file.is_none() {
        return HttpResponse::BadRequest().body("No organ selected");
    }
    let rc = build_runtime_config(&s.state);
    s.web_start_request = Some(rc);
    s.revision = s.revision.wrapping_add(1);
    HttpResponse::Ok().json(serde_json::json!({"status": "starting"}))
}

async fn config_quit(data: web::Data<ApiData>) -> impl Responder {
    let cfg = require_config!(data);
    let mut s = cfg.lock().unwrap();
    s.web_quit_request = true;
    s.revision = s.revision.wrapping_add(1);
    HttpResponse::Ok().json(serde_json::json!({"status": "quitting"}))
}

async fn config_set_locale(
    body: web::Json<ConfigLocaleRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    // Locale is a global setting, allowed in either mode for symmetry with
    // the play UI (which currently has no language picker but might later).
    let valid = crate::i18n_web::SUPPORTED_LANGUAGES
        .iter()
        .any(|(code, _, _)| *code == body.locale);
    if !valid {
        return HttpResponse::BadRequest().body("Unsupported locale");
    }
    rust_i18n::set_locale(&body.locale);
    log::info!("Locale switched to {}", body.locale);
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok", "locale": body.locale}))
}

async fn config_browse(query: web::Query<BrowseQuery>) -> impl Responder {
    let raw = query
        .path
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    let path: PathBuf = match raw {
        Some(p) => PathBuf::from(p),
        None => dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
    };

    let path = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            return HttpResponse::BadRequest().body(format!("Invalid path: {}", e));
        }
    };

    if !path.is_dir() {
        return HttpResponse::BadRequest().body("Path is not a directory");
    }

    let exts: Option<Vec<String>> = query.exts.as_deref().map(|s| {
        s.split(',')
            .map(|e| e.trim().trim_start_matches('.').to_lowercase())
            .filter(|e| !e.is_empty())
            .collect()
    });

    let mut entries: Vec<BrowseEntry> = Vec::new();
    let read = match std::fs::read_dir(&path) {
        Ok(rd) => rd,
        Err(e) => {
            return HttpResponse::InternalServerError()
                .body(format!("Failed to read directory: {}", e));
        }
    };

    for entry in read.flatten() {
        let entry_path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = metadata.is_dir();
        if !is_dir {
            if let Some(ref filter_exts) = exts {
                let ext = entry_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase());
                let matches = ext.as_ref().is_some_and(|e| filter_exts.contains(e));
                if !matches {
                    continue;
                }
            }
        }
        entries.push(BrowseEntry {
            name,
            path: entry_path.to_string_lossy().to_string(),
            is_dir,
            size: if is_dir { None } else { Some(metadata.len()) },
        });
    }

    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    let parent_path = path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .filter(|s| !s.is_empty());

    HttpResponse::Ok().json(BrowseResponse {
        current_path: path.to_string_lossy().to_string(),
        parent_path,
        entries,
    })
}

async fn config_add_organ(
    body: web::Json<AddOrganRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let path = PathBuf::from(&body.path);
    if !path.exists() || !path.is_file() {
        return HttpResponse::BadRequest().body("File not found");
    }
    let mut library = match load_organ_library() {
        Ok(l) => l,
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };
    if library.organs.iter().any(|o| o.path == path) {
        return HttpResponse::Conflict().body("Organ already in library");
    }
    let name = body
        .name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| path.to_string_lossy().to_string())
        });
    library.organs.push(OrganProfile {
        name,
        path,
        activation_trigger: None,
    });
    if let Err(e) = config::save_organ_library(&library) {
        return HttpResponse::InternalServerError().body(e.to_string());
    }
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
}

async fn config_remove_organ(
    body: web::Json<RemoveOrganRequest>,
    data: web::Data<ApiData>,
) -> impl Responder {
    let path = PathBuf::from(&body.path);
    let mut library = match load_organ_library() {
        Ok(l) => l,
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };
    let before = library.organs.len();
    library.organs.retain(|o| o.path != path);
    if library.organs.len() == before {
        return HttpResponse::NotFound().body("Organ not found in library");
    }
    if let Err(e) = config::save_organ_library(&library) {
        return HttpResponse::InternalServerError().body(e.to_string());
    }
    // If we're in config mode and the removed organ was the selection,
    // clear it so the user has to make a fresh choice.
    if let Some(cfg) = get_config(&data) {
        let mut s = cfg.lock().unwrap();
        if s.state.settings.organ_file.as_ref() == Some(&path) {
            s.state.settings.organ_file = None;
        }
        s.revision = s.revision.wrapping_add(1);
    }
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
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
    // If a load is in flight, capture a snapshot to send right after Refetch
    // so the freshly-connected client sees the loading modal immediately.
    let initial_loading = {
        let ls = data.loading_state.lock().unwrap();
        if ls.active {
            Some(WsMessage::LoadingProgress {
                percent: ls.percent,
                message: ls.message.clone(),
            })
        } else {
            None
        }
    };

    actix_web::rt::spawn(async move {
        // Immediately tell this client to reload everything. This is the
        // authoritative signal: "the data behind the REST endpoints is
        // whatever the server has now." Sent on every new connection so
        // reconnect-after-mode-change refreshes the UI cleanly.
        if let Ok(json) = serde_json::to_string(&WsMessage::Refetch) {
            if session.text(json).await.is_err() {
                return;
            }
        }
        if let Some(msg) = initial_loading {
            if let Ok(json) = serde_json::to_string(&msg) {
                if session.text(json).await.is_err() {
                    return;
                }
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
                        // Server lives across mode changes now, so we no
                        // longer treat ServerRestarting specially — the
                        // client just hides the loading modal on
                        // LoadingComplete or on a Refetch.
                        if let Ok(json) = serde_json::to_string(&msg) {
                            if session.text(json).await.is_err() {
                                break;
                            }
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

/// Start the unified API server. The server lives for the entire program
/// lifetime; mode transitions are made by the caller writing into the
/// `mode` mutex (and broadcasting `Refetch` so connected web clients
/// re-discover the new mode).
pub fn start_api_server(
    mode: Arc<Mutex<Mode>>,
    loading_state: Arc<Mutex<LoadingState>>,
    ws_tx: broadcast::Sender<WsMessage>,
    port: u16,
) -> ApiServerHandle {
    // Background ticker: detects MIDI-learn captures driven by external MIDI
    // input and broadcasts the transition to web clients. With the unified
    // long-lived server, the ticker also lives for the program lifetime —
    // when the server isn't in play mode it simply finds no app_state and
    // sleeps.
    let ticker_stop = Arc::new(AtomicBool::new(false));
    {
        let ticker_mode = Arc::clone(&mode);
        let ticker_ws = ws_tx.clone();
        let ticker_stop = ticker_stop.clone();
        std::thread::spawn(move || {
            while !ticker_stop.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(100));
                if ticker_stop.load(Ordering::Acquire) {
                    break;
                }
                // Pull a clone of the current play app_state if any, then
                // drop the mode guard before locking app_state so we don't
                // hold both locks at once.
                let app_state = {
                    let mode = ticker_mode.lock().unwrap();
                    match &*mode {
                        Mode::Play(ctx) => Some(Arc::clone(&ctx.app_state)),
                        _ => None,
                    }
                };
                let Some(app_state) = app_state else { continue };
                let resp_opt = {
                    let mut state = app_state.lock().unwrap();
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
            mode,
            loading_state,
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
                .route("/mode", web::get().to(get_mode))
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
                // Config-mode routes (return 503 outside config mode)
                .route("/config", web::get().to(get_config_state))
                .route("/config/audio-device", web::post().to(config_set_audio_device))
                .route("/config/sample-rate", web::post().to(config_set_sample_rate))
                .route("/config/ir-file", web::post().to(config_set_ir_file))
                .route("/config/organ", web::post().to(config_set_organ))
                .route(
                    "/config/audio-settings",
                    web::post().to(config_set_audio_settings),
                )
                .route(
                    "/config/midi-device",
                    web::post().to(config_update_midi_device),
                )
                .route("/config/midi/rescan", web::post().to(config_rescan_midi))
                .route("/config/start", web::post().to(config_start))
                .route("/config/quit", web::post().to(config_quit))
                .route("/config/locale", web::post().to(config_set_locale))
                .route("/config/browse", web::get().to(config_browse))
                .route(
                    "/config/library/add-organ",
                    web::post().to(config_add_organ),
                )
                .route(
                    "/config/library/remove-organ",
                    web::post().to(config_remove_organ),
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
