use anyhow::Result;
use chrono::Local;
use std::fs;
use std::sync::mpsc;
use std::thread;

pub struct AudioRecorder {
    sender: mpsc::Sender<Vec<f32>>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl AudioRecorder {
    pub fn start(organ_name: String, sample_rate: u32) -> Result<Self> {
        let config_path = confy::get_configuration_file_path("rusty-pipes", "settings")?;
        let parent = config_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("No config parent dir"))?;
        let recording_dir = parent.join("recordings");
        if !recording_dir.exists() {
            fs::create_dir_all(&recording_dir)?;
        }

        let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
        let filename = format!("{}_{}.wav", organ_name, timestamp);
        let path = recording_dir.join(filename);

        let spec = hound::WavSpec {
            channels: 2,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };

        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        let path_clone = path.clone();

        let handle = thread::spawn(move || {
            let mut writer = match hound::WavWriter::create(path_clone, spec) {
                Ok(w) => w,
                Err(e) => {
                    log::error!("Failed to create WAV writer: {}", e);
                    return;
                }
            };

            for buffer in rx {
                for sample in buffer {
                    if let Err(e) = writer.write_sample(sample) {
                        log::error!("Error writing sample: {}", e);
                    }
                }
            }
            log::info!("WAV recording saved.");
        });

        log::info!("Started recording audio to {:?}", path);

        Ok(Self {
            sender: tx,
            thread_handle: Some(handle),
        })
    }

    pub fn push(&mut self, buffer: &[f32]) {
        let _ = self.sender.send(buffer.to_vec());
    }

    pub fn stop(self) {
        drop(self.sender);
        if let Some(h) = self.thread_handle {
            let _ = h.join();
        }
    }
}
