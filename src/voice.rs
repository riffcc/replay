//! Voice input — audio capture and local transcription via Parakeet V3.
//!
//! Uses CPAL for microphone capture (24kHz mono PCM).
//! Transcription via Parakeet V3 INT8 model (downloaded from blob.handy.computer).
//! No API key needed — runs entirely locally.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, atomic::{AtomicBool, Ordering}};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use flate2::read::GzDecoder;
use tar::Archive;
use transcribe_rs::TranscriptionEngine;
use transcribe_rs::engines::parakeet::{
    ParakeetEngine, ParakeetInferenceParams, ParakeetModelParams, TimestampGranularity,
};

const TARGET_SAMPLE_RATE: u32 = 24_000;
const LOCAL_VOICE_MODEL_DIRNAME: &str = "parakeet-tdt-0.6b-v3-int8";
const LOCAL_VOICE_MODEL_URL: &str = "https://blob.handy.computer/parakeet-v3-int8.tar.gz";

// ── Audio capture ──

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

    /// Number of samples captured so far (at native sample rate).
    pub fn duration_samples(&self) -> usize {
        self.samples.lock().unwrap().len()
    }

    /// Get a snapshot of all samples captured so far (resampled to 24kHz).
    pub fn samples_snapshot(&self) -> Vec<f32> {
        let raw = self.samples.lock().unwrap().clone();
        if self.sample_rate == TARGET_SAMPLE_RATE {
            raw
        } else {
            resample(&raw, self.sample_rate, TARGET_SAMPLE_RATE)
        }
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

// ── Local Parakeet transcription ──

/// Global transcriber instance — model stays loaded across calls.
static TRANSCRIBER: LazyLock<Mutex<LocalTranscriber>> =
    LazyLock::new(|| Mutex::new(LocalTranscriber::default()));

#[derive(Default)]
struct LocalTranscriber {
    loaded_model_dir: Option<PathBuf>,
    engine: Option<ParakeetEngine>,
}

impl LocalTranscriber {
    fn transcribe(&mut self, samples: Vec<f32>, model_dir: &Path) -> Result<String, String> {
        if samples.is_empty() {
            return Ok(String::new());
        }

        self.ensure_model_loaded(model_dir)?;

        let result = match self.engine.as_mut() {
            Some(engine) => {
                let params = ParakeetInferenceParams {
                    timestamp_granularity: TimestampGranularity::Segment,
                    ..Default::default()
                };
                engine
                    .transcribe_samples(samples, Some(params))
                    .map_err(|err| format!("Parakeet transcription failed: {err}"))?
                    .text
            }
            None => return Err("no local voice engine loaded".to_string()),
        };

        Ok(result.trim().to_string())
    }

    fn ensure_model_loaded(&mut self, model_dir: &Path) -> Result<(), String> {
        if self.loaded_model_dir.as_deref() == Some(model_dir) && self.engine.is_some() {
            return Ok(());
        }

        let mut engine = ParakeetEngine::new();
        engine
            .load_model_with_params(model_dir, ParakeetModelParams::int8())
            .map_err(|err| format!("failed to load Parakeet model: {err}"))?;
        self.engine = Some(engine);
        self.loaded_model_dir = Some(model_dir.to_path_buf());
        Ok(())
    }
}

/// Progress callback for model download.
pub type ProgressCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// Transcribe audio using the local Parakeet V3 model.
/// Downloads the model on first use (~200MB from blob.handy.computer).
/// If `on_progress` is provided, it's called with status messages during download.
pub async fn transcribe(
    samples: &[f32],
    on_progress: Option<ProgressCallback>,
) -> Result<String, String> {
    // Ensure model is downloaded (async-safe) before entering blocking thread
    let model_dir = ensure_model_assets_async(on_progress).await?;
    let samples = samples.to_vec();

    // Run transcription on a blocking thread (model inference is CPU-bound)
    tokio::task::spawn_blocking(move || {
        let mut transcriber = TRANSCRIBER
            .lock()
            .map_err(|_| "transcriber mutex poisoned".to_string())?;
        transcriber.transcribe(samples, &model_dir)
    })
    .await
    .map_err(|e| format!("transcription task failed: {e}"))?
}

// ── Model asset management ──

async fn ensure_model_assets_async(on_progress: Option<ProgressCallback>) -> Result<PathBuf, String> {
    let voice_root = voice_root()?;
    let model_dir = voice_root.join("models").join(LOCAL_VOICE_MODEL_DIRNAME);
    let ready_marker = model_dir.join("encoder-model.int8.onnx");

    if ready_marker.is_file() {
        return Ok(model_dir);
    }

    std::fs::create_dir_all(voice_root.join("models"))
        .map_err(|e| format!("failed to create voice model directory: {e}"))?;

    download_and_extract_model_async(&model_dir, &ready_marker, on_progress).await?;

    if !ready_marker.is_file() {
        return Err("model installed but encoder file is missing".to_string());
    }

    Ok(model_dir)
}

async fn download_and_extract_model_async(
    model_dir: &Path,
    ready_marker: &Path,
    on_progress: Option<ProgressCallback>,
) -> Result<(), String> {
    let report = |msg: &str| {
        if let Some(cb) = &on_progress {
            cb(msg);
        }
    };

    let cache_dir = voice_cache_dir()?;
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("failed to create voice cache directory: {e}"))?;
    let archive_path = cache_dir.join(format!("{LOCAL_VOICE_MODEL_DIRNAME}.tar.gz"));

    // Download if not cached
    if !archive_path.is_file() {
        report("Downloading Parakeet V3 model...");
        let response = reqwest::get(LOCAL_VOICE_MODEL_URL)
            .await
            .map_err(|e| format!("failed to download voice model: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("download failed: HTTP {}", response.status()));
        }
        let total_size = response.content_length().unwrap_or(0);
        let mut downloaded: u64 = 0;
        let mut bytes_buf = Vec::with_capacity(total_size as usize);

        let mut stream = response.bytes_stream();
        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("download error: {e}"))?;
            downloaded += chunk.len() as u64;
            bytes_buf.extend_from_slice(&chunk);
            if total_size > 0 {
                let pct = (downloaded * 100 / total_size).min(100);
                report(&format!("Downloading Parakeet V3... {pct}%"));
            }
        }
        report("Download complete, extracting...");
        std::fs::write(&archive_path, &bytes_buf)
            .map_err(|e| format!("failed to write voice archive: {e}"))?;
    }

    // Extract to temp dir, then move into place (CPU-bound, run on blocking thread)
    let archive_path_clone = archive_path.clone();
    let model_dir = model_dir.to_path_buf();
    let ready_marker = ready_marker.to_path_buf();
    let cache_dir_clone = cache_dir.clone();

    tokio::task::spawn_blocking(move || {
        let tmp_dir = cache_dir_clone.join(format!("extract-{}", std::process::id()));
        if tmp_dir.exists() {
            std::fs::remove_dir_all(&tmp_dir).ok();
        }
        std::fs::create_dir_all(&tmp_dir)
            .map_err(|e| format!("failed to create temp dir: {e}"))?;

        let extract_result = (|| -> Result<(), String> {
            let file = std::fs::File::open(&archive_path_clone)
                .map_err(|e| format!("failed to open archive: {e}"))?;
            let decoder = GzDecoder::new(file);
            let mut archive = Archive::new(decoder);
            archive.unpack(&tmp_dir)
                .map_err(|e| format!("failed to unpack archive: {e}"))?;
            Ok(())
        })();

        if extract_result.is_err() {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return extract_result;
        }

        let extracted = tmp_dir.join(LOCAL_VOICE_MODEL_DIRNAME);
        if !extracted.is_dir() {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(format!("archive missing expected directory {LOCAL_VOICE_MODEL_DIRNAME}"));
        }

        if model_dir.exists() {
            std::fs::remove_dir_all(&model_dir)
                .map_err(|e| format!("failed to replace model directory: {e}"))?;
        }
        std::fs::rename(&extracted, &model_dir)
            .map_err(|e| format!("failed to install model: {e}"))?;
        let _ = std::fs::remove_dir_all(&tmp_dir);

        if !ready_marker.is_file() {
            return Err("model installed but encoder file is missing".to_string());
        }

        Ok(())
    })
    .await
    .map_err(|e| format!("extraction task failed: {e}"))?
}

/// Resolve voice data root directory.
/// Checks: REPLAY_VOICE_ROOT env > codex legacy path > replay default.
fn voice_root() -> Result<PathBuf, String> {
    if let Ok(root) = std::env::var("REPLAY_VOICE_ROOT") {
        return Ok(PathBuf::from(root));
    }

    let data_dir = dirs::data_local_dir()
        .ok_or("failed to resolve local data directory")?;

    // Reuse codex's model if already downloaded
    let codex_root = data_dir.join("com.openai.codex").join("voice");
    let codex_marker = codex_root.join("models")
        .join(LOCAL_VOICE_MODEL_DIRNAME)
        .join("encoder-model.int8.onnx");
    if codex_marker.is_file() {
        return Ok(codex_root);
    }

    // Check legacy Handy path
    let handy_root = data_dir.join("com.pais.handy");
    let handy_marker = handy_root.join("models")
        .join(LOCAL_VOICE_MODEL_DIRNAME)
        .join("encoder-model.int8.onnx");
    if handy_marker.is_file() {
        return Ok(handy_root);
    }

    // Default to replay's own path
    Ok(data_dir.join("cc.riff.replay").join("voice"))
}

fn voice_cache_dir() -> Result<PathBuf, String> {
    if let Ok(root) = std::env::var("REPLAY_VOICE_CACHE_DIR") {
        return Ok(PathBuf::from(root));
    }
    let cache_dir = dirs::cache_dir()
        .ok_or("failed to resolve cache directory")?;
    Ok(cache_dir.join("replay"))
}
