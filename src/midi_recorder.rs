use anyhow::Result;
use std::fs;
use std::time::Instant;
use midly::{
    Format, Header, Smf, TrackEvent, TrackEventKind, 
    MidiMessage as MidlyMidiMessage, Timing, num::*
};
use chrono::Local;


pub struct MidiRecorder {
    track: Vec<TrackEvent<'static>>, 
    last_event_time: Instant,
    organ_name: String,
}

impl MidiRecorder {
    pub fn new(organ_name: String) -> Self {
        Self {
            track: Vec::new(),
            last_event_time: Instant::now(),
            organ_name,
        }
    }

    pub fn record(&mut self, channel: u8, status_byte: u8, param1: u8, param2: u8) {
        let now = Instant::now();
        let delta_micros = now.duration_since(self.last_event_time).as_micros() as u32;
        self.last_event_time = now;

        // Convert micros to MIDI ticks (approximate).
        // 120 BPM = 500,000 micros/beat. 480 ticks/beat.
        // Factor = 480 / 500,000 = 0.00096
        let delta_ticks = (delta_micros as f32 * 0.00096) as u32;

        // midly types require specific wrappers
        let u4_channel = u4::from(channel & 0x0F);
        let u7_p1 = u7::from(param1 & 0x7F);
        let u7_p2 = u7::from(param2 & 0x7F);

        let kind = match status_byte & 0xF0 {
            0x90 => Some(TrackEventKind::Midi { 
                channel: u4_channel, 
                message: MidlyMidiMessage::NoteOn { key: u7_p1, vel: u7_p2 } 
            }),
            0x80 => Some(TrackEventKind::Midi { 
                channel: u4_channel, 
                message: MidlyMidiMessage::NoteOff { key: u7_p1, vel: u7_p2 } 
            }),
            0xB0 => Some(TrackEventKind::Midi { 
                channel: u4_channel, 
                message: MidlyMidiMessage::Controller { controller: u7_p1, value: u7_p2 } 
            }),
            _ => None,
        };

        if let Some(kind) = kind {
            self.track.push(TrackEvent {
                delta: u28::from(delta_ticks), // Wrap delta time in u28
                kind,
            });
        }
    }

    pub fn save(&self) -> Result<String> {
        let config_path = confy::get_configuration_file_path("rusty-pipes", "settings")?;
        let parent = config_path.parent().ok_or_else(|| anyhow::anyhow!("No config parent dir"))?;
        let recording_dir = parent.join("recordings");
        if !recording_dir.exists() {
            fs::create_dir_all(&recording_dir)?;
        }

        let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
        let filename = format!("{}_{}_virtual.mid", self.organ_name, timestamp);
        let path = recording_dir.join(&filename);

        // Use Format::SingleTrack (Type 0 MIDI file)
        // Wrap timing (480) in u15
        let header = Header::new(
            Format::SingleTrack, 
            Timing::Metrical(u15::from(480))
        );
        
        let mut smf = Smf::new(header);
        
        // Smf expects a Vec of tracks. Since Format is SingleTrack, we push one track.
        smf.tracks.push(self.track.clone());

        smf.save(&path)?;
        
        log::info!("Saved MIDI file to {:?}", path);
        Ok(path.to_string_lossy().to_string())
    }
}