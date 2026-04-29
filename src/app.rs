use serde::Serialize;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Instant;

/// Change hints broadcast to connected web clients. The server sends a hint
/// and the client refetches the relevant REST endpoint — this avoids having
/// to serialize full state over the wire and keeps the REST API authoritative.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum WsMessage {
    StopsChanged,
    PresetsChanged,
    TremulantsChanged,
    AudioChanged,
    /// Instructs the web client to reload all state. Pushed by the server
    /// to every newly-connected WebSocket and on every mode change.
    Refetch,
    MidiLearn {
        state: String,
        target_name: Option<String>,
        event_description: Option<String>,
    },
    /// Periodic update from the organ-loading thread, mirroring the local
    /// progress bar. Web clients connected to the (single, long-lived)
    /// API server see this and open a loading modal.
    LoadingProgress {
        /// Progress in the 0.0..=1.0 range.
        percent: f32,
        /// Human-readable, already-localized status line (e.g. "Loading
        /// samples into RAM"). The web UI displays this verbatim.
        message: String,
    },
    /// Sent once at the end of an organ load. The web UI uses this as the
    /// authoritative signal to close the loading modal — relying on a
    /// final `LoadingProgress { percent: 1.0 }` is racy because the loader
    /// doesn't guarantee a 100% event before exiting.
    LoadingComplete,
}

/// Snapshot of the most recent organ-loading progress. Lives for the entire
/// program lifetime; the API server's WebSocket handler reads it on every
/// new connection so a freshly-connected client immediately learns whether
/// a load is in flight (and at what percentage).
#[derive(Debug, Clone, Default)]
pub struct LoadingState {
    pub active: bool,
    pub percent: f32,
    pub message: String,
}

/// Messages sent from the TUI and MIDI threads to the Audio thread.
#[derive(Debug)]
pub enum AppMessage {
    /// MIDI Note On event. (key, velocity, stop name)
    NoteOn(u8, u8, String),
    /// MIDI Note Off event. (key, stop name)
    NoteOff(u8, String),
    /// A command to stop all currently playing notes.
    AllNotesOff,
    /// Set the reverb impulse response file path.
    SetReverbIr(PathBuf),
    /// Set the reverb wet/dry mix.
    SetReverbWetDry(f32),
    SetGain(f32),
    SetPolyphony(usize),
    /// Activate or Deactivate a specific Tremulant (ID, Active)
    SetTremulantActive(String, bool),
    StartAudioRecording,
    StopAudioRecording,
    StartMidiRecording,
    StopMidiRecording,
    /// TUI quit event.
    Quit,
}

#[allow(dead_code)]
#[derive(Debug, PartialEq, Clone)]
pub enum MainLoopAction {
    Continue,
    Exit,
    ReloadOrgan { file: PathBuf },
}

/// Messages sent from other threads (like MIDI) to the TUI thread.
#[derive(Debug, Clone)]
pub enum TuiMessage {
    /// A formatted string for the MIDI log.
    MidiLog(String),
    /// An error message to display.
    Error(String),
    /// Triggered whenever a buffer underrun occurs.
    AudioUnderrun,
    ActiveVoicesUpdate(usize),
    CpuLoadUpdate(f32),
    /// Messages for Piano Roll
    TuiNoteOn(u8, u8, Instant),
    TuiNoteOff(u8, u8, Instant),
    TuiAllNotesOff,
    /// --- Midi events to TUI---
    /// (note, velocity, channel)
    MidiNoteOn(u8, u8, u8),
    /// (note, channel)
    MidiNoteOff(u8, u8),
    /// (channel)
    MidiChannelNotesOff(u8),
    MidiPlaybackFinished,
    MidiProgress(f32, u32, u32),
    MidiSeekChannel(Sender<i32>),
    MidiSysEx(Vec<u8>),
    ForceClose,
}

/// Holds information about a currently playing note.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ActiveNote {
    /// The MIDI note number.
    pub note: u8,
    /// When the note-on was received.
    pub start_time: Instant,
    /// The stop this note is playing on.
    pub stop_index: usize,
    /// The rank this note is playing on.
    pub rank_id: String,
    pub voice_id: u64,
}

pub const PIPES: &str = r"
                   ███                   
        ▐          ███          ▌        
      ▐ ▐          ███          ▌ ▌      
      ▐ ▐      ▐█▋ ███ ▐█▋      ▌ ▌      
    ▐ ▐ ▐      ▐█▋ ███ ▐█▋      ▌ ▌ ▌    
    ▐ ▐ ▐  ▐█▋ ▐█▋ ███ ▐█▋ ▐█▋  ▌ ▌ ▌    
  ▐ ▐ ▐ ▐  ▐█▋ ▐█▋ ███ ▐█▋ ▐█▋  ▌ ▌ ▌ ▌  
  ▐ ▐ ▐ ▐  ▐█▋ ▐█▋ ███ ▐█▋ ▐█▋  ▌ ▌ ▌ ▌  
▐ ▐ ▐ ▐ ▐  ▐▅▋ ▐▄▋ ▐▄▋ ▐▄▋ ▐▄▋  ▌ ▌ ▌ ▌ ▌
▐ ▐ ▐ ▐ ▐   ▀   ▀   █   ▀   ▀   ▌ ▌ ▌ ▌ ▌
▐███████████████████████████████████████▌
                  ▀▀▀▀▀                  ";

pub const LOGO: &str = r"██████╗ ██╗   ██╗███████╗████████╗██╗   ██╗    ██████╗ ██╗██████╗ ███████╗███████╗
██╔══██╗██║   ██║██╔════╝╚══██╔══╝╚██╗ ██╔╝    ██╔══██╗██║██╔══██╗██╔════╝██╔════╝
██████╔╝██║   ██║███████╗   ██║    ╚████╔╝     ██████╔╝██║██████╔╝█████╗  ███████╗
██╔══██╗██║   ██║╚════██║   ██║     ╚██╔╝      ██╔═══╝ ██║██╔═══╝ ██╔══╝  ╚════██║
██║  ██║╚██████╔╝███████║   ██║      ██║       ██║     ██║██║     ███████╗███████║
╚═╝  ╚═╝ ╚═════╝ ╚══════╝   ╚═╝      ╚═╝       ╚═╝     ╚═╝╚═╝     ╚══════╝╚══════╝
";
