//! Common helpers for the audio-bench binaries (no sherpa dependency, reusable by candle bins).
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const SAMPLE_RATE: u32 = 16_000;

pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

pub fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn status_kb(key: &str) -> f64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        })
        .unwrap_or(0.0)
}

/// Peak resident set size of this process in MB (VmHWM).
pub fn peak_rss_mb() -> f64 {
    status_kb("VmHWM:") / 1024.0
}

/// Linear-interpolated percentile (p in 0..=100).
pub fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let rank = (p / 100.0) * (v.len() as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    v[lo] + (v[hi] - v[lo]) * (rank - lo as f64)
}

pub fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Read a WAV (int16/24/32 or float32) as mono f32 in [-1, 1]. Returns (samples, sample_rate).
pub fn read_wav_mono_f32(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;
    let inter: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32768.0))
            .collect::<std::result::Result<Vec<f32>, hound::Error>>()?,
        (hound::SampleFormat::Int, 24) | (hound::SampleFormat::Int, 32) => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<std::result::Result<Vec<f32>, hound::Error>>()?
        }
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<f32>, hound::Error>>()?,
        other => bail!("unsupported WAV format {:?} in {}", other, path.display()),
    };
    let mono = if ch == 1 {
        inter
    } else {
        inter.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect()
    };
    Ok((mono, spec.sample_rate))
}

/// Write mono 16-bit PCM WAV.
pub fn write_wav_i16(path: &Path, samples: &[f32], sample_rate: u32) -> Result<()> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0).round() as i16)?;
    }
    w.finalize()?;
    Ok(())
}

#[derive(Deserialize, Clone, Debug)]
pub struct RefEntry {
    pub id: String,
    pub file: String,
    pub text: String,
    #[serde(default)]
    pub duration_s: f64,
}

pub struct Clip {
    pub id: String,
    pub text: String,
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Clip {
    pub fn duration_s(&self) -> f64 {
        self.samples.len() as f64 / self.sample_rate as f64
    }
}

/// Load every clip listed in `<dir>/references.json` (order preserved).
pub fn load_corpus(dir: &Path) -> Result<Vec<Clip>> {
    let refs_path = dir.join("references.json");
    let refs: Vec<RefEntry> = serde_json::from_str(
        &fs::read_to_string(&refs_path).with_context(|| format!("read {}", refs_path.display()))?,
    )?;
    let mut clips = Vec::new();
    for r in refs {
        let (samples, sample_rate) = read_wav_mono_f32(&dir.join(&r.file))?;
        clips.push(Clip { id: r.id, text: r.text, samples, sample_rate });
    }
    if clips.is_empty() {
        return Err(anyhow!("empty corpus in {}", dir.display()));
    }
    Ok(clips)
}

pub fn write_json(path: &Path, v: &Value) -> Result<()> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(path, serde_json::to_string_pretty(v)?)?;
    Ok(())
}

/// One compact JSON object per line on stdout.
pub fn emit(v: &Value) {
    println!("{}", v);
}

pub fn ensure_file(p: &PathBuf) -> Result<()> {
    if !p.is_file() {
        bail!("missing file: {} (run tools/audio-bench/fetch.py)", p.display());
    }
    Ok(())
}
