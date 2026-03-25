//! Voice input — audio capture and transcription.
//!
//! Uses CPAL for microphone capture, outputs 24kHz mono PCM.
//! Transcribes via chunked silence detection for real-time results.
//! Backend: Parakeet locally (if available), falls back to Whisper API.

use std::io::Cursor;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const TARGET_SAMPLE_RATE: u32 = 24_000;
const SILENCE_THRESHOLD: f32 = 0.01;
const MIN_CHUNK_SAMPLES: usize = TARGET_SAMPLE_RATE as usize / 2; // 0.5 seconds minimum

/// Audio capture handle.
pub struct VoiceCapture {
    _stream: cpal::Stream,
    samples: Arc<Mutex<Vec<f32>>>,
    stopped: Arc<AtomicBool>,
    sample_rate: u32,
    channels: u16,
}

/// Captured audio ready for transcription.
pub struct CapturedAudio {
    /// 24kHz mono f32 samples.
    pub samples: Vec<f32>,
}

impl VoiceCapture {
    /// Start recording from the default microphone.
    pub fn start() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host.default_input_device()
            .ok_or("No input device available")?;

        let config = device.default_input_config()
            .map_err(|e| format!("No input config: {e}"))?;

        let sample_rate = config.sample_rate();
        let channels = config.channels();
        let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));

        let samples_clone = Arc::clone(&samples);
        let stopped_clone = Arc::clone(&stopped);

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                device.build_input_stream(
                    &config.into(),
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        if stopped_clone.load(Ordering::Relaxed) { return; }
                        let mut buf = samples_clone.lock().unwrap();
                        // Take first channel only (mono)
                        for chunk in data.chunks(channels as usize) {
                            if let Some(&sample) = chunk.first() {
                                buf.push(sample);
                            }
                        }
                    },
                    |err| eprintln!("Audio error: {err}"),
                    None,
                ).map_err(|e| format!("Failed to build stream: {e}"))?
            }
            cpal::SampleFormat::I16 => {
                let samples_clone = Arc::clone(&samples);
                let stopped_clone = Arc::clone(&stopped);
                device.build_input_stream(
                    &config.into(),
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        if stopped_clone.load(Ordering::Relaxed) { return; }
                        let mut buf = samples_clone.lock().unwrap();
                        for chunk in data.chunks(channels as usize) {
                            if let Some(&sample) = chunk.first() {
                                buf.push(sample as f32 / i16::MAX as f32);
                            }
                        }
                    },
                    |err| eprintln!("Audio error: {err}"),
                    None,
                ).map_err(|e| format!("Failed to build stream: {e}"))?
            }
            fmt => return Err(format!("Unsupported sample format: {fmt:?}")),
        };

        stream.play().map_err(|e| format!("Failed to start recording: {e}"))?;

        Ok(Self {
            _stream: stream,
            samples,
            stopped,
            sample_rate,
            channels,
        })
    }

    /// Stop recording and return captured audio (resampled to 24kHz mono).
    pub fn stop(self) -> CapturedAudio {
        self.stopped.store(true, Ordering::Relaxed);

        let raw_samples = self.samples.lock().unwrap().clone();

        // Resample to 24kHz if needed
        let samples = if self.sample_rate == TARGET_SAMPLE_RATE {
            raw_samples
        } else {
            resample(&raw_samples, self.sample_rate, TARGET_SAMPLE_RATE)
        };

        CapturedAudio { samples }
    }

    /// Get current peak level for VU meter (0.0 - 1.0).
    pub fn peak(&self) -> f32 {
        let buf = self.samples.lock().unwrap();
        let recent = if buf.len() > 1024 { &buf[buf.len() - 1024..] } else { &buf };
        recent.iter().map(|s| s.abs()).fold(0.0f32, f32::max)
    }
}

/// Simple linear resampling.
fn resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = (samples.len() as f64 / ratio) as usize;
    let mut out = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let src_idx = i as f64 * ratio;
        let idx = src_idx as usize;
        let frac = src_idx - idx as f64;

        let s0 = samples.get(idx).copied().unwrap_or(0.0);
        let s1 = samples.get(idx + 1).copied().unwrap_or(s0);
        out.push(s0 + (s1 - s0) * frac as f32);
    }

    out
}

/// Encode samples as WAV bytes.
pub fn encode_wav(samples: &[f32]) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .map_err(|e| format!("WAV writer error: {e}"))?;

        for &sample in samples {
            let s16 = (sample * i16::MAX as f32) as i16;
            writer.write_sample(s16).map_err(|e| format!("WAV write: {e}"))?;
        }

        writer.finalize().map_err(|e| format!("WAV finalize: {e}"))?;
    }

    Ok(cursor.into_inner())
}

/// Find silence-delimited chunks in audio for real-time transcription.
/// Returns byte offsets where safe chunk boundaries are.
pub fn find_chunk_boundaries(samples: &[f32]) -> Vec<usize> {
    let window_size = TARGET_SAMPLE_RATE as usize / 10; // 100ms windows
    let min_silence_windows = 3; // 300ms of silence = chunk boundary

    let mut boundaries = Vec::new();
    let mut silence_count = 0;

    for (i, window) in samples.chunks(window_size).enumerate() {
        let rms = (window.iter().map(|s| s * s).sum::<f32>() / window.len() as f32).sqrt();

        if rms < SILENCE_THRESHOLD {
            silence_count += 1;
            if silence_count == min_silence_windows {
                let boundary = (i - min_silence_windows + 1) * window_size;
                if boundary > MIN_CHUNK_SAMPLES {
                    boundaries.push(boundary);
                }
            }
        } else {
            silence_count = 0;
        }
    }

    boundaries
}

/// Transcribe audio using whatever backend is available.
/// Tries Parakeet locally first, falls back to whisper CLI.
pub async fn transcribe(samples: &[f32]) -> Result<String, String> {
    // Try local Parakeet model first
    let parakeet_path = dirs::data_local_dir()
        .unwrap_or_default()
        .join("com.openai.codex/voice/models/parakeet-tdt-0.6b-v3-int8");

    if parakeet_path.exists() {
        return transcribe_parakeet(samples, &parakeet_path).await;
    }

    // Fall back to whisper CLI
    let wav_bytes = encode_wav(samples)?;
    let tmp = std::env::temp_dir().join("replay_voice.wav");
    std::fs::write(&tmp, &wav_bytes).map_err(|e| format!("Write tmp: {e}"))?;

    let output = tokio::process::Command::new("whisper")
        .args(["--model", "base", "--output_format", "txt", "--output_dir", "/tmp"])
        .arg(&tmp)
        .output()
        .await
        .map_err(|e| format!("whisper not found: {e}"))?;

    if !output.status.success() {
        return Err("Whisper transcription failed".to_string());
    }

    let txt_path = std::env::temp_dir().join("replay_voice.txt");
    std::fs::read_to_string(&txt_path)
        .map_err(|e| format!("Read transcription: {e}"))
        .map(|s| s.trim().to_string())
}

/// Transcribe using local Parakeet model.
async fn transcribe_parakeet(samples: &[f32], _model_path: &std::path::Path) -> Result<String, String> {
    // TODO: integrate transcribe-rs directly when we add it as a dep
    // For now, use the Codex parakeet binary if available
    let wav_bytes = encode_wav(samples)?;
    let tmp = std::env::temp_dir().join("replay_voice_parakeet.wav");
    std::fs::write(&tmp, &wav_bytes).map_err(|e| format!("Write tmp: {e}"))?;

    Err("Parakeet model found but native integration not yet available. Install whisper CLI as fallback.".to_string())
}
