use anyhow::{anyhow, bail, Result};
use audio_bench::{emit, ensure_file, load_corpus, mean, ms, path_str, peak_rss_mb, write_json};
use clap::Parser;
use serde_json::json;
use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};
use std::path::PathBuf;
use std::time::Instant;

const WINDOW: usize = 512;
const SR: i32 = 16_000;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "data/corpus")]
    corpus: PathBuf,
    #[arg(long, default_value = "data/models/silero-vad/silero_vad.onnx")]
    model: PathBuf,
    #[arg(long, default_value = "data/results/vad-sherpa.json")]
    out: PathBuf,
}

fn drain(vad: &VoiceActivityDetector, segs: &mut Vec<(i64, i64)>) {
    while let Some(seg) = vad.front() {
        let start = seg.start();
        let n = seg.n() as i64;
        drop(seg);
        vad.pop();
        segs.push((start, start + n));
    }
}

/// Returns (segments in samples, processing ms, number of full 512 windows).
fn run(vad: &VoiceActivityDetector, samples: &[f32]) -> (Vec<(i64, i64)>, f64, usize) {
    vad.reset();
    vad.clear();
    let mut segs = Vec::new();
    let t = Instant::now();
    for chunk in samples.chunks(WINDOW) {
        vad.accept_waveform(chunk);
        drain(vad, &mut segs);
    }
    vad.flush();
    drain(vad, &mut segs);
    (segs, ms(t.elapsed()), samples.len() / WINDOW)
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure_file(&args.model)?;
    let clips = load_corpus(&args.corpus)?;
    for c in &clips {
        if c.sample_rate != SR as u32 {
            bail!("clip {} is {} Hz, expected 16000", c.id, c.sample_rate);
        }
    }

    let silero = SileroVadModelConfig {
        model: Some(path_str(&args.model)),
        threshold: 0.5,
        min_silence_duration: 0.6,
        min_speech_duration: 0.2,
        max_speech_duration: 30.0,
        window_size: WINDOW as i32,
    };
    let cfg = VadModelConfig {
        silero_vad: silero,
        ten_vad: Default::default(),
        sample_rate: SR,
        num_threads: 1,
        provider: Some("cpu".into()),
        debug: false,
    };
    let t0 = Instant::now();
    let vad = VoiceActivityDetector::create(&cfg, 30.0).ok_or_else(|| anyhow!("VoiceActivityDetector::create failed"))?;
    let load_ms = ms(t0.elapsed());

    // Warm-up (excluded).
    let _ = run(&vad, &clips[0].samples);

    let mut per_frame = Vec::new();
    let mut clips_json = Vec::new();
    for c in &clips {
        let (segs, proc_ms, frames) = run(&vad, &c.samples);
        let us_per_frame = if frames > 0 { proc_ms * 1000.0 / frames as f64 } else { 0.0 };
        per_frame.push(us_per_frame);
        let seg_json: Vec<_> = segs
            .iter()
            .map(|(s, e)| json!({"start_s": *s as f64 / SR as f64, "end_s": *e as f64 / SR as f64}))
            .collect();
        let row = json!({
            "id": c.id, "duration_s": c.duration_s(), "num_segments": segs.len(),
            "segments": seg_json, "process_ms": proc_ms, "frames": frames, "us_per_frame": us_per_frame
        });
        let mut line = row.clone();
        line["type"] = json!("clip");
        emit(&line);
        clips_json.push(row);
    }

    // 3 s of digital silence -> must yield zero segments.
    let silence = vec![0.0f32; 3 * SR as usize];
    let (sil_segs, sil_ms, sil_frames) = run(&vad, &silence);
    let silence_ok = sil_segs.is_empty();

    let summary = json!({
        "engine": "sherpa-onnx",
        "model": "silero-vad-v5",
        "params": {"threshold": 0.5, "min_speech_s": 0.2, "min_silence_s": 0.6, "max_speech_s": 30.0, "window": WINDOW},
        "load_ms": load_ms,
        "mean_us_per_frame": mean(&per_frame),
        "silence": {"duration_s": 3.0, "num_segments": sil_segs.len(), "process_ms": sil_ms, "frames": sil_frames, "ok": silence_ok},
        "peak_rss_mb": peak_rss_mb(),
    });
    let mut line = summary.clone();
    line["type"] = json!("summary");
    emit(&line);

    let mut full = summary;
    full["clips"] = json!(clips_json);
    write_json(&args.out, &full)?;
    eprintln!("[vad-sherpa] wrote {}", args.out.display());
    if !silence_ok {
        eprintln!("[vad-sherpa] FAIL: silence produced {} segments", sil_segs.len());
        std::process::exit(1);
    }
    Ok(())
}
