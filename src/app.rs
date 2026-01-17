use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Instant;

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
