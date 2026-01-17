use actix_web::{web, App, HttpResponse, HttpServer, Responder};
use actix_web::dev::ServerHandle;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{self, Sender};
use std::path::PathBuf;
use std::thread;
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;

use crate::app_state::AppState;
use crate::app::MainLoopAction;
use crate::app::AppMessage;
use crate::config::{self, load_organ_library};

/// A handle that controls the lifecycle of the API Server.
/// When this struct is dropped, the server shuts down and the background thread exits.
pub struct ApiServerHandle {
    handle: ServerHandle,
}

impl Drop for ApiServerHandle {
    fn drop(&mut self) {
        println!("Stopping API Server...");
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
        set_tremulant
    ),
    components(
        schemas(
            StopStatusResponse, 
            ChannelUpdateRequest, 
            OrganInfoResponse,
            OrganEntryResponse,
            LoadOrganRequest,
            PresetSaveRequest,
            ValueRequest,
            ReverbRequest,
            ReverbMixRequest,
            ReverbEntry,
            AudioSettingsResponse,
            TremulantResponse,
            TremulantSetRequest
        )
    ),
    tags(
        (name = "Rusty Pipes API", description = "Control endpoints for the virtual organ")
    )
)]
struct ApiDoc;

// --- Handlers ---

/// Redirects to Swagger UI
#[utoipa::path(get, path = "/", responses((status = 302, description = "Redirect to Swagger")))]
async fn index() -> impl Responder {
    HttpResponse::Found().append_header(("Location", "/swagger-ui/")).finish()
}

/// Returns information about the currently loaded organ.
#[utoipa::path(
    get, path = "/organ", tag = "General",
    responses((status = 200, body = OrganInfoResponse))
)]
async fn get_organ_info(data: web::Data<ApiData>) -> impl Responder {
    let state = data.app_state.lock().unwrap();
    HttpResponse::Ok().json(OrganInfoResponse { name: state.organ.name.clone() })
}

/// Returns a list of all organs available in the library.
#[utoipa::path(
    get, path = "/organs", tag = "General",
    responses((status = 200, body = Vec<OrganEntryResponse>))
)]
async fn get_organ_library() -> impl Responder {
    match load_organ_library() {
        Ok(lib) => {
            let response: Vec<OrganEntryResponse> = lib.organs.iter().map(|p| OrganEntryResponse {
                name: p.name.clone(),
                path: p.path.to_string_lossy().to_string(),
            }).collect();
            HttpResponse::Ok().json(response)
        },
        Err(e) => HttpResponse::InternalServerError().body(format!("Failed to load library: {}", e))
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
    let found = lib.organs.iter().find(|o| o.path.to_string_lossy() == *target_path_str);

    if let Some(profile) = found {
        log::info!("API: Requesting reload of organ: {}", profile.name);
        
        // Signal the main loop to reload
        *data.exit_action.lock().unwrap() = MainLoopAction::ReloadOrgan { 
            file: profile.path.clone() 
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
        let mut active_channels = state.stop_channels.get(&i)
            .map(|set| set.iter().cloned().collect::<Vec<u8>>())
            .unwrap_or_default();
        active_channels.sort();

        response_list.push(StopStatusResponse {
            index: i,
            name: stop.name.clone(),
            active_channels,
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
    data: web::Data<ApiData>
) -> impl Responder {
    let (stop_index, channel_id) = path.into_inner();
    if channel_id > 15 { return HttpResponse::BadRequest().body("Channel ID > 15"); }

    let mut state = data.app_state.lock().unwrap();
    if stop_index >= state.organ.stops.len() { return HttpResponse::NotFound().finish(); }

    match state.set_stop_channel_state(stop_index, channel_id, body.active, &data.audio_tx) {
        Ok(_) => {
            let action = if body.active { "Enabled" } else { "Disabled" };
            state.add_midi_log(format!("API: {} Stop {} for Ch {}", action, stop_index, channel_id + 1));
            HttpResponse::Ok().json(serde_json::json!({ "status": "success" }))
        },
        Err(e) => HttpResponse::InternalServerError().body(e.to_string())
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
    if !(1..=12).contains(&slot_id) { return HttpResponse::BadRequest().body("Invalid slot"); }

    let mut state = data.app_state.lock().unwrap();
    match state.recall_preset(slot_id - 1, &data.audio_tx) {
        Ok(_) => {
             if state.presets[slot_id - 1].is_some() {
                 state.add_midi_log(format!("API: Loaded Preset F{}", slot_id));
                 HttpResponse::Ok().json(serde_json::json!({ "status": "success" }))
             } else {
                 HttpResponse::NotFound().body("Preset empty")
             }
        },
        Err(e) => HttpResponse::InternalServerError().body(e.to_string())
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
    data: web::Data<ApiData>
) -> impl Responder {
    let slot_id = path.into_inner();
    if !(1..=12).contains(&slot_id) { return HttpResponse::BadRequest().body("Invalid slot"); }

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
    let mut state = data.app_state.lock().unwrap();
    state.gain = body.value.clamp(0.0, 2.0);
    let _ = data.audio_tx.send(AppMessage::SetGain(state.gain));
    state.persist_settings();
    HttpResponse::Ok().json(serde_json::json!({"status": "success", "gain": state.gain}))
}

/// Set Polyphony limit (minimum 1).
#[utoipa::path(
    post, path = "/audio/polyphony", tag = "Audio",
    request_body = ValueRequest,
    responses((status = 200))
)]
async fn set_polyphony(body: web::Json<ValueRequest>, data: web::Data<ApiData>) -> impl Responder {
    let mut state = data.app_state.lock().unwrap();
    state.polyphony = (body.value as usize).max(1);
    let _ = data.audio_tx.send(AppMessage::SetPolyphony(state.polyphony));
    state.persist_settings();
    HttpResponse::Ok().json(serde_json::json!({"status": "success", "polyphony": state.polyphony}))
}

/// Start or Stop MIDI Recording.
#[utoipa::path(
    post, path = "/record/midi", tag = "Recording",
    request_body = ChannelUpdateRequest, 
    responses((status = 200))
)]
async fn start_stop_midi_recording(body: web::Json<ChannelUpdateRequest>, data: web::Data<ApiData>) -> impl Responder {
    let mut state = data.app_state.lock().unwrap();
    state.is_recording_midi = body.active;
    if body.active {
        let _ = data.audio_tx.send(AppMessage::StartMidiRecording);
        state.add_midi_log("API: Started MIDI Recording".into());
    } else {
        let _ = data.audio_tx.send(AppMessage::StopMidiRecording);
        state.add_midi_log("API: Stopped MIDI Recording".into());
    }
    HttpResponse::Ok().json(serde_json::json!({"status": "success", "recording_midi": state.is_recording_midi}))
}

/// Start or Stop Audio (WAV) Recording.
#[utoipa::path(
    post, path = "/record/audio", tag = "Recording",
    request_body = ChannelUpdateRequest, 
    responses((status = 200))
)]
async fn start_stop_audio_recording(body: web::Json<ChannelUpdateRequest>, data: web::Data<ApiData>) -> impl Responder {
    let mut state = data.app_state.lock().unwrap();
    state.is_recording_audio = body.active;
    if body.active {
        let _ = data.audio_tx.send(AppMessage::StartAudioRecording);
        state.add_midi_log("API: Started Audio Recording".into());
    } else {
        let _ = data.audio_tx.send(AppMessage::StopAudioRecording);
        state.add_midi_log("API: Stopped Audio Recording".into());
    }
    HttpResponse::Ok().json(serde_json::json!({"status": "success", "recording_audio": state.is_recording_audio}))
}

/// Get available Impulse Response (Reverb) files.
#[utoipa::path(
    get, path = "/audio/reverbs", tag = "Audio",
    responses((status = 200, body = Vec<ReverbEntry>))
)]
async fn get_reverbs(data: web::Data<ApiData>) -> impl Responder {
    let list: Vec<ReverbEntry> = data.reverb_files.iter().enumerate().map(|(i, (name, _))| {
        ReverbEntry { index: i, name: name.clone() }
    }).collect();
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
    let mut state = data.app_state.lock().unwrap();

    if idx < 0 {
        state.selected_reverb_index = None;
        let _ = data.audio_tx.send(AppMessage::SetReverbWetDry(0.0));
        state.persist_settings();
        return HttpResponse::Ok().json(serde_json::json!({"status": "disabled"}));
    }

    let u_idx = idx as usize;
    if u_idx >= data.reverb_files.len() {
        return HttpResponse::BadRequest().body("Invalid reverb index");
    }

    let (name, path) = &data.reverb_files[u_idx];
    state.selected_reverb_index = Some(u_idx);
    let _ = data.audio_tx.send(AppMessage::SetReverbIr(path.clone()));
    let _ = data.audio_tx.send(AppMessage::SetReverbWetDry(state.reverb_mix));
    
    state.persist_settings();
    state.add_midi_log(format!("API: Reverb set to '{}'", name));
    
    HttpResponse::Ok().json(serde_json::json!({"status": "success", "reverb": name}))
}

/// Set Reverb Mix (0.0 - 1.0).
#[utoipa::path(
    post, path = "/audio/reverbs/mix", tag = "Audio",
    request_body = ReverbMixRequest,
    responses((status = 200))
)]
async fn set_reverb_mix(body: web::Json<ReverbMixRequest>, data: web::Data<ApiData>) -> impl Responder {
    let mut state = data.app_state.lock().unwrap();
    state.reverb_mix = body.mix.clamp(0.0, 1.0);
    let _ = data.audio_tx.send(AppMessage::SetReverbWetDry(state.reverb_mix));
    state.persist_settings();
    HttpResponse::Ok().json(serde_json::json!({"status": "success", "mix": state.reverb_mix}))
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
    data: web::Data<ApiData>
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

// --- Server Launcher ---

pub fn start_api_server(
    app_state: Arc<Mutex<AppState>>,
    audio_tx: Sender<AppMessage>,
    port: u16,
    exit_action: Arc<Mutex<MainLoopAction>>,
) -> ApiServerHandle{
    let reverb_files = Arc::new(config::get_available_ir_files());

    // Create a channel to send the ServerHandle from the background thread back to here
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let sys = actix_web::rt::System::new();
        
        let server_data = web::Data::new(ApiData {
            app_state,
            audio_tx,
            exit_action,
            reverb_files,
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
                // General
                .route("/organ", web::get().to(get_organ_info))
                .route("/organs", web::get().to(get_organ_library))
                .route("/organs/load", web::post().to(load_organ)) 
                .route("/panic", web::post().to(panic))
                // Stops
                .route("/stops", web::get().to(get_stops))
                .route("/stops/{stop_id}/channels/{channel_id}", web::post().to(update_stop_channel))
                // Presets
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
        })
        .bind(("0.0.0.0", port));

        match server {
            Ok(bound_server) => {
                println!("REST API server listening on http://0.0.0.0:{}", port);
                println!("Swagger UI available at http://0.0.0.0:{}/swagger-ui/", port);
                let server = bound_server.run();
                let handle = server.handle();
                let _ = tx.send(handle);
                if let Err(e) = sys.block_on(server) {
                    eprintln!("API Server Error: {}", e);
                }
            },
            Err(e) => eprintln!("Failed to bind API server to port {}: {}", port, e),
        }
    });
    // Wait for the server to start up and give us the handle
    let handle = rx.recv().expect("Failed to start API server or receive handle");

    ApiServerHandle { handle }
}