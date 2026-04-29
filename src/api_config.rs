use actix_web::dev::ServerHandle;
use actix_web::{App, HttpRequest, HttpResponse, HttpServer, Responder, web};
use anyhow::Result;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use tokio::sync::broadcast;

use crate::app::WsMessage;
use crate::audio::get_supported_sample_rates;
use crate::config::{
    AppSettings, ConfigShared, MidiDeviceConfig, MidiMappingMode, OrganProfile, load_organ_library,
};
use crate::gui_config::build_runtime_config;

/// Lifecycle handle for the config-mode API server. Drop to shut down.
pub struct ConfigApiServerHandle {
    handle: ServerHandle,
}

impl Drop for ConfigApiServerHandle {
    fn drop(&mut self) {
        let h = self.handle.clone();
        thread::spawn(move || {
            let sys = actix_web::rt::System::new();
            sys.block_on(async {
                log::info!("Shutting down config API server...");
                h.stop(true).await;
                log::info!("Config API server shut down.");
            });
        });
    }
}

#[derive(Clone)]
struct ConfigApiData {
    shared: Arc<Mutex<ConfigShared>>,
    ws_tx: broadcast::Sender<WsMessage>,
}

fn broadcast(data: &web::Data<ConfigApiData>, msg: WsMessage) {
    let _ = data.ws_tx.send(msg);
}

// --- Response models ---

#[derive(Serialize)]
struct ConfigStateResponse {
    settings: AppSettings,
    midi_file: Option<String>,
    available_audio_devices: Vec<String>,
    selected_audio_device_name: Option<String>,
    available_sample_rates: Vec<u32>,
    available_ir_files: Vec<IrFileEntry>,
    system_midi_ports: Vec<MidiPortEntry>,
    organ_library: Vec<OrganLibraryEntry>,
    last_used_organ: Option<String>,
    error_msg: Option<String>,
}

#[derive(Serialize)]
struct IrFileEntry {
    name: String,
    path: String,
}

#[derive(Serialize)]
struct MidiPortEntry {
    name: String,
}

#[derive(Serialize)]
struct OrganLibraryEntry {
    name: String,
    path: String,
}

// --- Request models ---

#[derive(Deserialize)]
struct AudioDeviceRequest {
    /// Device name; null/missing means "system default".
    name: Option<String>,
}

#[derive(Deserialize)]
struct SampleRateRequest {
    rate: u32,
}

#[derive(Deserialize)]
struct IrFileRequest {
    /// Path of the IR file (must match a discovered file). Null clears.
    path: Option<String>,
}

#[derive(Deserialize)]
struct OrganRequest {
    /// Path of the organ definition (must match a library entry).
    path: String,
}

#[derive(Deserialize)]
struct AudioSettingsRequest {
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
struct MidiDeviceUpdateRequest {
    /// Device name (must match an entry in settings.midi_devices)
    name: String,
    enabled: Option<bool>,
    mapping_mode: Option<String>, // "Simple" or "Complex"
    simple_target_channel: Option<u8>,
    complex_mapping: Option<Vec<u8>>, // length 16
}

// --- Handlers ---

async fn mode() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({"mode": "config"}))
}

async fn i18n() -> impl Responder {
    HttpResponse::Ok().json(crate::i18n_web::web_translations())
}

async fn get_state(data: web::Data<ConfigApiData>) -> impl Responder {
    let s = data.shared.lock().unwrap();
    let st = &s.state;

    let library = load_organ_library().unwrap_or_default();
    let organ_library: Vec<OrganLibraryEntry> = library
        .organs
        .iter()
        .map(|p: &OrganProfile| OrganLibraryEntry {
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
            .map(|(name, path)| IrFileEntry {
                name: name.clone(),
                path: path.to_string_lossy().to_string(),
            })
            .collect(),
        system_midi_ports: st
            .system_midi_ports
            .iter()
            .map(|(_, name)| MidiPortEntry { name: name.clone() })
            .collect(),
        organ_library,
        last_used_organ,
        error_msg: st.error_msg.clone(),
    };
    HttpResponse::Ok().json(resp)
}

async fn set_audio_device(
    body: web::Json<AudioDeviceRequest>,
    data: web::Data<ConfigApiData>,
) -> impl Responder {
    let mut s = data.shared.lock().unwrap();
    let st = &mut s.state;

    if let Some(name) = body.name.clone() {
        if !st.available_audio_devices.contains(&name) {
            return HttpResponse::BadRequest().body("Unknown audio device");
        }
        st.selected_audio_device_name = Some(name);
    } else {
        st.selected_audio_device_name = None;
    }

    // Refresh sample rates for the new device
    if let Ok(rates) = get_supported_sample_rates(st.selected_audio_device_name.clone()) {
        st.available_sample_rates = rates;
        if !st
            .available_sample_rates
            .contains(&st.settings.sample_rate)
        {
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

async fn set_sample_rate(
    body: web::Json<SampleRateRequest>,
    data: web::Data<ConfigApiData>,
) -> impl Responder {
    let mut s = data.shared.lock().unwrap();
    if !s.state.available_sample_rates.contains(&body.rate) {
        return HttpResponse::BadRequest().body("Unsupported sample rate for selected device");
    }
    s.state.settings.sample_rate = body.rate;
    s.revision = s.revision.wrapping_add(1);
    drop(s);
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
}

async fn set_ir_file(
    body: web::Json<IrFileRequest>,
    data: web::Data<ConfigApiData>,
) -> impl Responder {
    let mut s = data.shared.lock().unwrap();
    let st = &mut s.state;
    match &body.path {
        None => st.settings.ir_file = None,
        Some(p) => {
            let m = st
                .available_ir_files
                .iter()
                .find(|(_, path)| path.to_string_lossy() == *p);
            match m {
                Some((_, path)) => st.settings.ir_file = Some(path.clone()),
                None => return HttpResponse::BadRequest().body("Unknown IR file"),
            }
        }
    }
    s.revision = s.revision.wrapping_add(1);
    drop(s);
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
}

async fn set_organ(
    body: web::Json<OrganRequest>,
    data: web::Data<ConfigApiData>,
) -> impl Responder {
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

    let mut s = data.shared.lock().unwrap();
    s.state.settings.organ_file = Some(profile.path.clone());
    s.revision = s.revision.wrapping_add(1);
    drop(s);
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
}

async fn set_audio_settings(
    body: web::Json<AudioSettingsRequest>,
    data: web::Data<ConfigApiData>,
) -> impl Responder {
    let mut s = data.shared.lock().unwrap();
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

async fn update_midi_device(
    body: web::Json<MidiDeviceUpdateRequest>,
    data: web::Data<ConfigApiData>,
) -> impl Responder {
    let mut s = data.shared.lock().unwrap();
    let dev = s
        .state
        .settings
        .midi_devices
        .iter_mut()
        .find(|d| d.name == body.name);
    let dev = match dev {
        Some(d) => d,
        None => {
            // Create the entry if it doesn't exist (e.g. user sets mapping
            // for a device that hasn't been rescanned in shared yet).
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

async fn rescan_midi(data: web::Data<ConfigApiData>) -> impl Responder {
    let mut s = data.shared.lock().unwrap();
    if let Err(e) = s.rescan_midi_ports() {
        return HttpResponse::InternalServerError().body(e.to_string());
    }
    drop(s);
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
}

async fn start(data: web::Data<ConfigApiData>) -> impl Responder {
    let mut s = data.shared.lock().unwrap();
    if s.state.settings.organ_file.is_none() {
        return HttpResponse::BadRequest().body("No organ selected");
    }
    let rc = build_runtime_config(&s.state);
    s.web_start_request = Some(rc);
    s.revision = s.revision.wrapping_add(1);
    // We do NOT broadcast ServerRestarting here. The client uses that
    // message to abort in-flight fetches, which would also abort this very
    // request and cause the start to surface as an error. The natural
    // WebSocket closure when main.rs drops the server is sufficient signal
    // for the client to begin reconnecting.
    HttpResponse::Ok().json(serde_json::json!({"status": "starting"}))
}

async fn quit(data: web::Data<ConfigApiData>) -> impl Responder {
    let mut s = data.shared.lock().unwrap();
    s.web_quit_request = true;
    s.revision = s.revision.wrapping_add(1);
    HttpResponse::Ok().json(serde_json::json!({"status": "quitting"}))
}

#[derive(Deserialize)]
struct LocaleRequest {
    locale: String,
}

/// Switch the active translation locale at runtime. Validated against the
/// fixed list of supported locales so a malformed value can't put the app
/// into an unknown state.
async fn set_locale(
    body: web::Json<LocaleRequest>,
    data: web::Data<ConfigApiData>,
) -> impl Responder {
    let valid = crate::i18n_web::SUPPORTED_LANGUAGES
        .iter()
        .any(|(code, _, _)| *code == body.locale);
    if !valid {
        return HttpResponse::BadRequest().body("Unsupported locale");
    }
    rust_i18n::set_locale(&body.locale);
    log::info!("Locale switched to {}", body.locale);
    // Bump revision so the local UI can re-render with the new strings on
    // its next frame, and notify other web clients so they reload /i18n.
    {
        let mut s = data.shared.lock().unwrap();
        s.revision = s.revision.wrapping_add(1);
    }
    broadcast(&data, WsMessage::Refetch);
    HttpResponse::Ok().json(serde_json::json!({"status": "ok", "locale": body.locale}))
}

// --- WebSocket ---

async fn ws_handler(
    req: HttpRequest,
    stream: web::Payload,
    data: web::Data<ConfigApiData>,
) -> Result<HttpResponse, actix_web::Error> {
    let (response, mut session, mut msg_stream) = actix_ws::handle(&req, stream)?;
    let mut rx = data.ws_tx.subscribe();

    actix_web::rt::spawn(async move {
        if let Ok(json) = serde_json::to_string(&WsMessage::Refetch) {
            if session.text(json).await.is_err() {
                return;
            }
        }

        loop {
            tokio::select! {
                ws_msg = msg_stream.next() => match ws_msg {
                    Some(Ok(actix_ws::Message::Ping(b))) => {
                        if session.pong(&b).await.is_err() { break; }
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
                            if session.text(json).await.is_err() { break; }
                        }
                        if is_restart {
                            let _ = session.close(None).await;
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                },
            }
        }
    });

    Ok(response)
}

// --- Embedded Web UI (shared with play-mode server) ---

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

async fn redirect_to_ui() -> impl Responder {
    HttpResponse::Found()
        .append_header(("Location", "/ui/"))
        .finish()
}

// --- Listen-address discovery (duplicated from api_rest.rs) ---

fn list_listen_addresses() -> Vec<std::net::IpAddr> {
    use std::net::{IpAddr, Ipv4Addr};
    let mut out: Vec<IpAddr> = vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))];
    if let Ok(addrs) = local_ip_address::list_afinet_netifas() {
        let mut iface: Vec<IpAddr> = addrs
            .into_iter()
            .map(|(_name, ip)| ip)
            .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
            .filter(|ip| match ip {
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

fn format_host(ip: &std::net::IpAddr) -> String {
    match ip {
        std::net::IpAddr::V4(v4) => v4.to_string(),
        std::net::IpAddr::V6(v6) => format!("[{}]", v6),
    }
}

// --- Server launcher ---

pub fn start_config_api_server(
    shared: Arc<Mutex<ConfigShared>>,
    port: u16,
    ws_tx: broadcast::Sender<WsMessage>,
) -> Result<ConfigApiServerHandle> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let sys = actix_web::rt::System::new();

        let server_data = web::Data::new(ConfigApiData { shared, ws_tx });

        let server = HttpServer::new(move || {
            App::new()
                .app_data(server_data.clone())
                .route("/", web::get().to(redirect_to_ui))
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
                // Config endpoints
                .route("/config", web::get().to(get_state))
                .route("/config/audio-device", web::post().to(set_audio_device))
                .route("/config/sample-rate", web::post().to(set_sample_rate))
                .route("/config/ir-file", web::post().to(set_ir_file))
                .route("/config/organ", web::post().to(set_organ))
                .route("/config/audio-settings", web::post().to(set_audio_settings))
                .route("/config/midi-device", web::post().to(update_midi_device))
                .route("/config/midi/rescan", web::post().to(rescan_midi))
                .route("/config/start", web::post().to(start))
                .route("/config/quit", web::post().to(quit))
                .route("/config/locale", web::post().to(set_locale))
        })
        .bind(("0.0.0.0", port));

        match server {
            Ok(bound_server) => {
                let addrs = list_listen_addresses();
                println!("Config web UI available at:");
                for addr in &addrs {
                    println!("  http://{}:{}/ui/", format_host(addr), port);
                }
                let server = bound_server.run();
                let handle = server.handle();
                let _ = tx.send(handle);
                if let Err(e) = sys.block_on(server) {
                    eprintln!("Config API Server Error: {}", e);
                }
            }
            Err(e) => {
                eprintln!("Failed to bind config API server to port {}: {}", port, e);
                // Send a placeholder so the receiver doesn't deadlock — we'll
                // surface the error on the actual handle.
            }
        }
    });

    let handle = rx
        .recv()
        .map_err(|_| anyhow::anyhow!("Config API server failed to start"))?;
    Ok(ConfigApiServerHandle { handle })
}
