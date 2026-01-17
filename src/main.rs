use anyhow::Result;
use clap::{Parser, ValueEnum};
use midir::MidiInput;
use rust_i18n::t;
use simplelog::{Config, LevelFilter, WriteLogger};
use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

rust_i18n::i18n!("locales");

mod api_rest;
mod app;
mod app_state;
mod audio;
mod audio_convolver;
mod audio_event;
mod audio_loader;
mod audio_recorder;
mod config;
mod gui;
mod gui_config;
mod gui_filepicker;
mod gui_midi;
mod gui_midi_learn;
mod gui_organ_manager;
mod input;
mod loading_ui;
mod midi;
mod midi_control;
mod midi_recorder;
mod organ;
mod organ_grandorgue;
mod organ_hauptwerk;
mod tui;
mod tui_config;
mod tui_filepicker;
mod tui_midi;
mod tui_midi_learn;
mod voice;
mod wav;
mod wav_converter;

use app::{AppMessage, TuiMessage};
use app_state::{AppState, connect_to_midi};
use config::{AppSettings, MidiDeviceConfig, RuntimeConfig};
use input::KeyboardLayout;
use organ::Organ;

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
#[value(rename_all = "lower")]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to organ definition file (e.g., friesach/friesach.organ or friesach/OrganDefinitions/Friesach.Organ_Hauptwerk_xml)
    #[arg(value_name = "ORGAN_DEFINITION")]
    organ_file: Option<PathBuf>,

    /// Optional path to a MIDI file to play
    #[arg(long = "midi-file", value_name = "MIDI_FILE")]
    midi_file: Option<PathBuf>,

    /// Pre-cache all samples on startup (uses more memory, reduces latency)
    #[arg(long)]
    precache: Option<bool>,

    /// Convert all samples to 16-bit PCM on load (saves memory, may reduce quality)
    #[arg(long)]
    convert_to_16bit: Option<bool>,

    /// Set the application log level
    #[arg(long, value_name = "LEVEL", default_value = "info")]
    log_level: LogLevel,

    /// Optional path to a convolution reverb Impulse Response (IR) file
    #[arg(long, value_name = "IR_FILE")]
    ir_file: Option<PathBuf>,

    /// Reverb mix level (0.0 = dry, 1.0 = fully wet)
    #[arg(long, value_name = "REVERB_MIX")]
    reverb_mix: Option<f32>,

    /// Preserve original (de)tuning of recorded samples up to +/- 20 cents to preserve organ character
    #[arg(long)]
    original_tuning: Option<bool>,

    /// List all available MIDI input devices and exit
    #[arg(long)]
    list_midi_devices: bool,

    /// Select a MIDI device by name (Enables this device with default 1:1 channel mapping)
    #[arg(long, value_name = "MIDI_DEVICE")]
    midi_device: Option<String>,

    /// Select an audio device by name
    #[arg(long, value_name = "AUDIO_DEVICE")]
    audio_device: Option<String>,

    /// Audio buffer size in frames (lower values reduce latency but may cause glitches)
    #[arg(long, value_name = "NUM_FRAMES")]
    audio_buffer_frames: Option<usize>,

    /// How many audio frames to pre-load for each pipe's samples (uses RAM, prevents buffer underruns)
    #[arg(long, value_name = "NUM_PRELOAD_FRAMES")]
    preload_frames: Option<usize>,

    /// Run in terminal UI (TUI) mode as a fallback
    #[arg(long)]
    tui: bool,

    /// HTTP Port that the REST API server will listen on
    #[arg(long, value_name = "API_PORT", default_value_t = 8080)]
    api_server_port: u16,

    /// Force a specific language/locale (e.g., "en", "de", "nl-BE")
    #[arg(long, value_name = "LANG")]
    lang: Option<String>,
}

// Handle struct that manages the lifecycle for the midi thread
#[allow(dead_code)]
struct LogicThreadHandle {
    stop_signal: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for LogicThreadHandle {
    fn drop(&mut self) {
        log::info!("Signaling MIDI Logic thread to stop...");
        self.stop_signal.store(true, Ordering::SeqCst);
        // Optional: Wait for it to finish to ensure clean memory release before loading next organ
        //if let Some(h) = self.handle.take() {
        //    let _ = h.join();
        //}
    }
}

#[cfg_attr(feature = "hotpath", hotpath::main(percentiles = [99]))]
fn main() -> Result<()> {
    let args = Args::parse();

    // --- Setup Locale ---
    // Priority: 1. CLI Argument, 2. System Locale, 3. Fallback "en-US"
    let locale_to_use = args
        .lang
        .clone()
        .unwrap_or_else(|| sys_locale::get_locale().unwrap_or_else(|| String::from("en-US")));

    // Set the locale for rust-i18n
    rust_i18n::set_locale(&locale_to_use);

    // --- Setup logging ---
    let log_level = match args.log_level {
        LogLevel::Error => LevelFilter::Error,
        LogLevel::Warn => LevelFilter::Warn,
        LogLevel::Info => LevelFilter::Info,
        LogLevel::Debug => LevelFilter::Debug,
        LogLevel::Trace => LevelFilter::Trace,
    };

    let settings_path = confy::get_configuration_file_path("rusty-pipes", "settings")?;

    // Get the parent directory (e.g., .../Application Support/rusty-pipes/)
    let log_dir = settings_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Could not get log directory"))?;

    // Ensure this directory exists
    if !log_dir.exists() {
        fs::create_dir_all(log_dir)?;
    }

    // Create the log file inside that directory
    let log_path = log_dir.join("rusty-pipes.log");

    WriteLogger::init(log_level, Config::default(), File::create(log_path)?)?;

    // --- List MIDI devices and exit ---
    if args.list_midi_devices {
        println!("{}", t!("main.list_devices_header"));
        match midi::get_midi_device_names() {
            Ok(names) => {
                if names.is_empty() {
                    println!("  {}", t!("main.list_devices_none"));
                } else {
                    for (i, name) in names.iter().enumerate() {
                        println!("  {}: {}", i, name);
                    }
                }
            }
            Err(e) => {
                eprintln!("{}", t!("errors.midi_fetch_fail", err = e));
            }
        }
        return Ok(());
    }

    let midi_input_arc = Arc::new(Mutex::new(match MidiInput::new("Rusty Pipes MIDI Input") {
        Ok(mi) => Some(mi),
        Err(e) => {
            log::error!("Failed to initialize MIDI: {}", e);
            None
        }
    }));

    // --- Load Config and Merge CLI Args ---
    let mut settings = config::load_settings().unwrap_or_default();
    let tui_mode = args.tui;

    let active_layout = KeyboardLayout::detect();
    log::info!("Detected system locale, defaulting keyboard layout to: {:?}", active_layout);
     
    // Command-line arguments override saved config
    if let Some(f) = args.organ_file { settings.organ_file = Some(f); }
    if let Some(f) = args.ir_file { settings.ir_file = Some(f); }
    if let Some(m) = args.reverb_mix { settings.reverb_mix = m; }
    if let Some(b) = args.audio_buffer_frames { settings.audio_buffer_frames = b; }
    if let Some(p) = args.precache { settings.precache = p; }
    if let Some(c) = args.convert_to_16bit { settings.convert_to_16bit = c; }
    if let Some(o) = args.original_tuning { settings.original_tuning = o; }
    if let Some(d) = args.audio_device { settings.audio_device_name = Some(d); }

    // --- CLI: MIDI Device Selection ---
    // If a device is specified via CLI, we ensure it exists in settings and is enabled.
    // We treat it as a passthrough (1:1 mapping), which is the default for MidiDeviceConfig.
    if let Some(device_name) = args.midi_device {
        if let Some(dev) = settings.midi_devices.iter_mut().find(|d| d.name == device_name) {
            dev.enabled = true;
        } else {
            // New device from CLI, add it with defaults (Enabled=true, Simple/Complex defaults to 1:1)
            settings.midi_devices.push(MidiDeviceConfig {
                name: device_name,
                enabled: true,
                ..Default::default()
            });
        }
    }

    // --- Run Configuration UI ---
    let config_result = if tui_mode {
        tui_config::run_config_ui(settings, Arc::clone(&midi_input_arc))
    } else {
        gui_config::run_config_ui(settings, Arc::clone(&midi_input_arc))
    };

    // `config` is the final, user-approved configuration
    let mut config: RuntimeConfig = match config_result {
        Ok(Some(config)) => config,
        Ok(None) => {
            // User quit the config screen
            println!("{}", t!("main.config_cancelled"));
            return Ok(());
        }
        Err(e) => {
            // Need to make sure TUI is cleaned up if it failed
            if args.tui {
                let _ = tui::cleanup_terminal();
            }
            log::error!("Error in config UI: {}", e);
            return Err(e);
        }
    };
     
    // --- Save Final Settings (excluding runtime options) ---
    // We reconstruct the midi_devices list based on the active connections + config logic.
    // Note: This simple approach saves the state of devices that were active/configured in this session.
    let devices_to_save: Vec<MidiDeviceConfig> = config
        .active_midi_devices
        .iter()
        .map(|(_, cfg)| cfg.clone())
        .collect();

    let settings_to_save = AppSettings {
        organ_file: Some(config.organ_file.clone()),
        ir_file: config.ir_file.clone(),
        reverb_mix: config.reverb_mix,
        audio_buffer_frames: config.audio_buffer_frames,
        max_ram_gb: config.max_ram_gb,
        precache: config.precache,
        convert_to_16bit: config.convert_to_16bit,
        original_tuning: config.original_tuning,
        midi_devices: devices_to_save,
        gain: config.gain,
        polyphony: config.polyphony,
        audio_device_name: config.audio_device_name.clone(),
        sample_rate: config.sample_rate,
        tui_mode,
        keyboard_layout: active_layout,
    };
    if let Err(e) = config::save_settings(&settings_to_save) {
        log::warn!("Failed to save settings: {}", e);
    }

    // --- APPLICATION MAIN LOOP ---
    loop {
        if tui_mode {
            println!(
                "\n{}\n",
                t!("main.title", version = env!("CARGO_PKG_VERSION"))
            );
        }

        let organ: Arc<Organ>;
        let shared_midi_recorder = Arc::new(Mutex::new(None));

        let reverb_files = config::get_available_ir_files();

        // If we are in GUI mode, we generally want the loading window,
        // especially if we are precaching OR converting OR just parsing a large file.
        let needs_loading_ui = !tui_mode;

        if needs_loading_ui {
            // --- GUI Pre-caching with Progress Window ---
            log::info!("Starting GUI loading process...");

            // Channels for progress
            let (progress_tx, progress_rx) = mpsc::channel::<(f32, String)>();
            let is_finished = Arc::new(AtomicBool::new(false));

            // We need to move the config and is_finished Arc into the loading thread
            let load_config = config.clone();
            let is_finished_clone = Arc::clone(&is_finished);

            // This Arc<Mutex<...>> will hold the result from the loading thread
            let organ_result_arc = Arc::new(Mutex::new(None));
            let organ_result_clone = Arc::clone(&organ_result_arc);

            // --- Spawn the Loading Thread ---
            thread::spawn(move || {
                log::info!("[LoadingThread] Started.");

                // Call Organ::load, passing the progress transmitter
                let load_result = Organ::load(
                    &load_config.organ_file,
                    load_config.convert_to_16bit,
                    load_config.precache,
                    load_config.original_tuning,
                    load_config.sample_rate,
                    Some(progress_tx),
                    (load_config.max_ram_gb * 1024.0) as usize,
                );

                log::info!("[LoadingThread] Finished.");

                // Store the result
                *organ_result_clone.lock().unwrap() = Some(load_result);

                // Signal the UI thread that we are done
                is_finished_clone.store(true, Ordering::SeqCst);
            });

            // --- Run the Loading UI on the Main Thread ---
            // This will block until the loading thread sets `is_finished` to true
            // and the eframe window closes itself.
            if let Err(e) = loading_ui::run_loading_ui(progress_rx, is_finished) {
                log::error!("Failed to run loading UI: {}", e);
                // We might still be able to recover, but it's safer to exit
                return Err(anyhow::anyhow!(t!("errors.loading_ui_fail", err = e)));
            }

            // --- Retrieve the loaded organ ---
            let organ_result = organ_result_arc
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| anyhow::anyhow!("Loading thread did not produce an organ"))?;

            organ = Arc::new(organ_result?);
        } else {
            // --- TUI Loading (Simple text progress) ---
            if tui_mode {
                println!("{}", t!("main.loading_organ"));
            }

            // Create a dummy transmitter for TUI progress
            let (tui_progress_tx, tui_progress_rx) = mpsc::channel::<(f32, String)>();

            // TUI progress-printing thread
            let _tui_progress_thread = if config.precache && tui_mode {
                Some(thread::spawn(move || {
                    while let Ok((progress, file_name)) = tui_progress_rx.recv() {
                        // Simple TUI progress
                        use std::io::Write;
                        print!(
                            "\r{}      ",
                            t!(
                                "main.loading_samples_fmt",
                                percent = (progress * 100.0) as i32,
                                file = file_name
                            )
                        );
                        std::io::stdout().flush().unwrap();
                    }
                    println!("\r{}              ", t!("main.loading_complete"));
                }))
            } else {
                None
            };

            // Note: For TUI mode, we only pass the progress transmitter if we are precaching.
            // If we aren't precaching, the conversion is usually fast enough (or hidden) in TUI.
            organ = Arc::new(Organ::load(
                &config.organ_file,
                config.convert_to_16bit,
                config.precache,
                config.original_tuning,
                config.sample_rate,
                if config.precache && tui_mode {
                    Some(tui_progress_tx)
                } else {
                    None
                },
                (config.max_ram_gb * 1024.0) as usize,
            )?);
        }

        if tui_mode {
            println!("{}", t!("main.organ_loaded_fmt", name = organ.name));
            println!("{}", t!("main.found_stops_fmt", count = organ.stops.len()));
        }

        // --- Create channels for thread communication ---
        let (audio_tx, audio_rx) = mpsc::channel::<AppMessage>();
        let (tui_tx, tui_rx) = mpsc::channel::<TuiMessage>();
        let (gui_ctx_tx, gui_ctx_rx) = mpsc::channel::<egui::Context>();

        // --- Start the Audio thread ---
        if tui_mode {
            println!("{}", t!("main.starting_audio"));
        }
        let _audio_handle = audio::start_audio_playback(
            audio_rx,
            Arc::clone(&organ),
            config.audio_buffer_frames,
            config.gain,
            config.polyphony,
            config.audio_device_name.clone(),
            config.sample_rate,
            tui_tx.clone(),
            shared_midi_recorder.clone(),
        )?;
        if tui_mode {
            println!("{}", t!("main.audio_running"));
        }

        // --- Load IR file ---
        if let Some(path) = &config.ir_file {
            if path.exists() {
                log::info!("Loading IR file: {}", path.display());
                audio_tx.send(AppMessage::SetReverbIr(path.clone()))?;
                audio_tx.send(AppMessage::SetReverbWetDry(config.reverb_mix))?;
            } else {
                log::warn!("IR file not found: {}", path.display());
            }
        }

        // --- Create thread-safe AppState ---
        let app_state = Arc::new(Mutex::new(AppState::new(
            organ.clone(),
            config.gain,
            config.polyphony,
            active_layout,
        )?));

        // --- REST API SERVER ---
        let _api_server_handle = api_rest::start_api_server(app_state.clone(), audio_tx.clone(), args.api_server_port);

        // --- Spawn the dedicated MIDI logic thread ---
        let logic_app_state = Arc::clone(&app_state);
        let logic_audio_tx = audio_tx.clone();
        // Create a stop signal
        let logic_stop_signal = Arc::new(AtomicBool::new(false));
        let logic_stop_clone = logic_stop_signal.clone();

        let gui_is_running = Arc::new(AtomicBool::new(true)); 
        let gui_running_clone = gui_is_running.clone();

        let thread_handle = thread::spawn(move || {
            log::info!("MIDI logic thread started.");
            let mut egui_ctx: Option<egui::Context> = None;

            // This is a blocking loop, it waits for messages from either the MIDI callback or the file player.
            while !logic_stop_clone.load(Ordering::Relaxed) {
                
                // Use recv_timeout instead of recv
                // This wakes up every 250ms to check the while-loop condition (stop_signal)
                match tui_rx.recv_timeout(std::time::Duration::from_millis(250)) {
                    Ok(msg) => {
                        if logic_stop_clone.load(Ordering::Relaxed) {
                            log::info!("MIDI logic thread stop signal received. Exiting.");
                            break;
                        }
                        if egui_ctx.is_none() {
                            if let Ok(ctx) = gui_ctx_rx.try_recv() {
                                egui_ctx = Some(ctx);
                            }
                        }

                        let mut app_state_locked = logic_app_state.lock().unwrap();
                        if let Err(e) = app_state_locked.handle_tui_message(msg, &logic_audio_tx) {
                            let err_msg = format!("Error handling TUI message: {}", e);
                            log::error!("{}", err_msg);
                            app_state_locked.add_midi_log(err_msg);
                        }

                        if let Some(ctx) = &egui_ctx {
                            if gui_running_clone.load(Ordering::Relaxed) {
                                ctx.request_repaint();
                            }
                        }
                    },
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // No message arrived, just loop back and check logic_stop_clone again
                        continue; 
                    },
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        // All senders dropped, we can exit safely
                        log::info!("All TUI senders dropped. Logic thread exiting.");
                        break;
                    }
                }
            }
            log::info!("MIDI logic thread shutting down.");
        });

        let _logic_thread_handle = LogicThreadHandle {
            stop_signal: logic_stop_signal,
            handle: Some(thread_handle),
        };

        // --- Start MIDI ---
        let _midi_file_thread: Option<JoinHandle<()>>;

        if let Some(path) = config.midi_file.clone() {
            if tui_mode {
                println!("{}", t!("main.starting_midi_file", path = path.display()));
            }

            // Update state
            {
                let mut state = app_state.lock().unwrap();
                state.midi_file_path = Some(path.clone());
                state.is_midi_file_playing = true;
            }

            // We need access to the stop signal from state
            let stop_signal = app_state.lock().unwrap().midi_file_stop_signal.clone();

            midi::play_midi_file(path, tui_tx.clone(), stop_signal)?;
        }

        // We store multiple connections to keep them alive
        let mut midi_connections = Vec::new();

        // --- Use live MIDI input (Multiple Devices) ---
        // Iterate over the configured active devices
        if !config.active_midi_devices.is_empty() {
            for (port, dev_config) in &config.active_midi_devices {
                let client_name = format!("Rusty Pipes - {}", dev_config.name);

                // Create a new client for each connection (midir consumes the client on connect)
                match MidiInput::new(&client_name) {
                    Ok(client) => {
                        if tui_mode {
                            println!("{}", t!("main.connecting_midi", name = dev_config.name));
                        }

                        match connect_to_midi(
                            client,
                            port,
                            &dev_config.name,
                            &tui_tx,
                            dev_config.clone(),
                            Arc::clone(&shared_midi_recorder),
                        ) {
                            Ok(conn) => {
                                midi_connections.push(conn);
                                app_state
                                    .lock()
                                    .unwrap()
                                    .add_midi_log(format!("Connected: {}", dev_config.name));
                            }
                            Err(e) => {
                                log::error!("Failed to connect to {}: {}", dev_config.name, e);
                                app_state.lock().unwrap().add_midi_log(
                                    t!("errors.midi_connect_fail", name = dev_config.name, err = e)
                                        .to_string(),
                                );
                            }
                        }
                    }
                    Err(e) => log::error!(
                        "Failed to create MIDI client for {}: {}",
                        dev_config.name,
                        e
                    ),
                }
            }
        } else if tui_mode {
            println!("{}", t!("main.no_midi_devices"));
        }

        // --- Run the TUI or GUI on the main thread ---
        let loop_action = if tui_mode {
            tui::run_tui_loop(audio_tx, Arc::clone(&app_state))?;
            app::MainLoopAction::Exit
        } else {
            log::info!("Starting GUI...");
            gui::run_gui_loop(
                audio_tx,
                tui_tx,
                Arc::clone(&app_state),
                organ,
                midi_connections, // Pass the Vector of connections
                gui_ctx_tx,
                reverb_files,
                config.ir_file.clone(),
                config.reverb_mix,
                gui_is_running.clone(),
            )?
        };

        gui_is_running.store(false, Ordering::SeqCst);

        match loop_action {
            app::MainLoopAction::ReloadOrgan { file } => {
                log::info!("Reloading organ: {:?}", file);
                config.organ_file = file;
                // Threads (audio, logic) and channels will be dropped here and recreated in next iteration
                drop(_logic_thread_handle);
                drop(_api_server_handle);
                drop(_audio_handle);
            }
            app::MainLoopAction::Exit => {
                break;
            }
            app::MainLoopAction::Continue => {
                // Just restart same organ
            }
        }
    }

    // --- Shutdown ---
    if tui_mode { println!("{}", t!("main.shutting_down")); }
    log::info!("Shutting down...");
    Ok(())
}