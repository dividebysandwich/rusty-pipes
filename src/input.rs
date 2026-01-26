use crossterm::event::KeyCode;
use egui::Key;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyboardLayout {
    Qwerty, // US, UK, International
    Qwertz, // German, Central European
    Azerty, // French

            // Note: This may be a naive implementation, but should cover the basics for now
}

/// The result of a key press analysis
pub enum MusicCommand {
    PlayNote(u8), // The semitone offset (0 = C, 1 = C#, etc.)
    OctaveUp,
    OctaveDown,
    None,
}

impl KeyboardLayout {
    /// Detects the layout based on the system locale/language.
    pub fn detect() -> Self {
        let locale = sys_locale::get_locale().unwrap_or_else(|| "en-US".into());
        let lang = locale.split(['-', '_']).next().unwrap_or("en");

        match lang {
            "de" | "cs" | "sk" | "hu" => Self::Qwertz, // German, Czech, etc.
            "fr" | "be" => Self::Azerty,               // French, Belgian
            _ => Self::Qwerty,                         // Default to US/International
        }
    }

    /// Maps a Crossterm KeyCode (TUI) to a MusicCommand
    pub fn map_crossterm(&self, code: KeyCode) -> MusicCommand {
        match code {
            // Octave Control (Fairly standard across layouts for this row)
            KeyCode::Char('e') => MusicCommand::OctaveDown,
            KeyCode::Char('r') => MusicCommand::OctaveUp,

            // Note Mapping
            KeyCode::Char(c) => self.char_to_semitone(c),
            _ => MusicCommand::None,
        }
    }

    /// Maps an Egui Key (GUI) to a MusicCommand
    pub fn map_egui(&self, key: Key) -> MusicCommand {
        match key {
            Key::E => MusicCommand::OctaveDown,
            Key::R => MusicCommand::OctaveUp,
            _ => self.egui_key_to_semitone(key),
        }
    }

    /// Internal logic for characters (TUI)
    fn char_to_semitone(&self, c: char) -> MusicCommand {
        // Normalize to lowercase
        let s = match c.to_ascii_lowercase() {
            // Layout specific bottom-left key (Note C)
            'z' if matches!(self, Self::Qwerty) => Some(0),
            'y' if matches!(self, Self::Qwertz) => Some(0),
            'w' if matches!(self, Self::Azerty) => Some(0),

            // Standard middle row (Black keys)
            's' => Some(1), // C#
            'd' => Some(3), // D#

            // Special handling for D (Note D) vs A/Q swaps
            'x' => Some(2), // D

            // Standard bottom row continued...
            'c' => Some(4),  // E
            'v' => Some(5),  // F
            'g' => Some(6),  // F#
            'b' => Some(7),  // G
            'h' => Some(8),  // G#
            'n' => Some(9),  // A
            'j' => Some(10), // A#

            // The M key varies by layout location, but we map the char 'm'
            'm' => match self {
                Self::Azerty => Some(12), // On Azerty M is to the right of L (High C context)
                _ => Some(11),            // B
            },

            // Comma/High C handling
            ',' => match self {
                Self::Azerty => Some(11), // On Azerty comma is often where M is on Qwerty
                _ => Some(12),            // High C
            },
            'l' => Some(13), // C#
            '.' => Some(14), // D

            _ => None,
        };

        match s {
            Some(n) => MusicCommand::PlayNote(n),
            None => MusicCommand::None,
        }
    }

    /// Internal logic for Egui Keys
    fn egui_key_to_semitone(&self, key: Key) -> MusicCommand {
        let s = match key {
            // Layout specific: The Bottom Left Key (C)
            Key::Z if matches!(self, Self::Qwerty) => Some(0),
            Key::Y if matches!(self, Self::Qwertz) => Some(0),
            Key::W if matches!(self, Self::Azerty) => Some(0),

            // Black Keys
            Key::S => Some(1),  // C#
            Key::D => Some(3),  // D#
            Key::G => Some(6),  // F#
            Key::H => Some(8),  // G#
            Key::J => Some(10), // A#

            // White Keys
            Key::X => Some(2), // D
            Key::C => Some(4), // E
            Key::V => Some(5), // F
            Key::B => Some(7), // G
            Key::N => Some(9), // A

            // Last two keys are tricky across physical layouts
            Key::M => match self {
                Self::Azerty => Some(12),
                _ => Some(11),
            },
            Key::Comma => match self {
                Self::Azerty => Some(11),
                _ => Some(12),
            },
            Key::L => Some(13),      // C#
            Key::Period => Some(14), // D

            _ => None,
        };

        match s {
            Some(n) => MusicCommand::PlayNote(n),
            None => MusicCommand::None,
        }
    }
}
