use std::time::Instant;

/// Messages sent from the TUI and MIDI threads to the Audio thread.
#[derive(Debug)]
pub enum AppMessage {
    /// MIDI Note On event. (key, velocity)
    NoteOn(u8, u8),
    /// MIDI Note Off event. (key)
    NoteOff(u8),
    /// A command to stop all currently playing notes.
    AllNotesOff,
    /// TUI stop toggle event. (stop_index, is_active)
    StopToggle(usize, bool),
    /// TUI quit event.
    Quit,
}

/// Messages sent from other threads (like MIDI) to the TUI thread.
#[derive(Debug, Clone)]
pub enum TuiMessage {
    /// A formatted string for the MIDI log.
    MidiLog(String),
    /// An error message to display.
    Error(String),
    /// Messages for Piano Roll
    TuiNoteOn(u8, Instant),
    TuiNoteOff(u8, Instant),
    TuiAllNotesOff,
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

