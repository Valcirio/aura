use anyhow::{anyhow, bail, Result};
use audio_bench::{emit, ensure_file, load_corpus, mean, ms, path_str, peak_rss_mb, percentile, write_json};
use clap::Parser;
use serde_json::json;
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineWhisperModelConfig};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, value_parser = ["turbo", "small"])]
    model: String,
    #[arg(long, default_value_t = 4)]
    threads: i32,
    #[arg(long, default_value = "data/corpus")]
    corpus: PathBuf,
    #[arg(long, default_value = "data/models")]
    models_dir: PathBuf,
    #[arg(long, default_value = "pt")]
    language: String,
    #[arg(long)]
    out: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let (dir, prefix) = match args.model.as_str() {
        "turbo" => ("whisper-turbo", "turbo"),
        "small" => ("whisper-small", "small"),
        m => bail!("unknown model {m}"),
    };
    let mdir = args.models_dir.join(dir);
    let encoder = mdir.join(format!("{prefix}-encoder.int8.onnx"));
    let decoder = mdir.join(format!("{prefix}-decoder.int8.onnx"));
    let tokens = mdir.join(format!("{prefix}-tokens.txt"));
    for p in [&encoder, &decoder, &tokens] {
        ensure_file(p)?;
    }
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("data/results/asr-sherpa-{}-t{}.json", args.model, args.threads)));
    let clips = load_corpus(&args.corpus)?;

    let mut cfg = OfflineRecognizerConfig::default();
    cfg.model_config.whisper = OfflineWhisperModelConfig {
        encoder: Some(path_str(&encoder)),
        decoder: Some(path_str(&decoder)),
        language: Some(args.language.clone()),
        task: Some("transcribe".into()),
        tail_paddings: 0,
        enable_token_timestamps: false,
        enable_segment_timestamps: false,
    };
    cfg.model_config.tokens = Some(path_str(&tokens));
    cfg.model_config.provider = Some("cpu".into());
    cfg.model_config.debug = false;
    cfg.model_config.num_threads = args.threads;

    eprintln!("[asr-sherpa] loading {} (threads={})", args.model, args.threads);
    let t0 = Instant::now();
    let rec = OfflineRecognizer::create(&cfg).ok_or_else(|| anyhow!("OfflineRecognizer::create failed"))?;
    let load_ms = ms(t0.elapsed());
    eprintln!("[asr-sherpa] loaded in {load_ms:.0} ms");

    let decode = |samples: &[f32], sr: u32| -> Result<String> {
        let stream = rec.create_stream();
        stream.accept_waveform(sr as i32, samples);
        rec.decode(&stream);
        let r = stream.get_result().ok_or_else(|| anyhow!("no recognition result"))?;
        Ok(r.text.trim().to_string())
    };

    // Warm-up on clip 01 (excluded from stats).
    let _ = decode(&clips[0].samples, clips[0].sample_rate)?;

    let mut lat = Vec::new();
    let mut rtfs = Vec::new();
    let mut clips_json = Vec::new();
    for c in &clips {
        let t = Instant::now();
        let text = decode(&c.samples, c.sample_rate)?;
        let latency_ms = ms(t.elapsed());
        let dur = c.duration_s();
        let rtf = latency_ms / 1000.0 / dur;
        lat.push(latency_ms);
        rtfs.push(rtf);
        let row = json!({"id": c.id, "duration_s": dur, "latency_ms": latency_ms, "rtf": rtf, "text": text});
        let mut line = row.clone();
        line["type"] = json!("clip");
        emit(&line);
        clips_json.push(row);
    }

    let summary = json!({
        "engine": "sherpa-onnx",
        "model": args.model,
        "threads": args.threads,
        "language": args.language,
        "load_ms": load_ms,
        "p50_ms": percentile(&lat, 50.0),
        "p95_ms": percentile(&lat, 95.0),
        "mean_rtf": mean(&rtfs),
        "peak_rss_mb": peak_rss_mb(),
    });
    let mut line = summary.clone();
    line["type"] = json!("summary");
    emit(&line);

    let mut full = summary;
    full["clips"] = json!(clips_json);
    write_json(&out, &full)?;
    eprintln!("[asr-sherpa] wrote {}", out.display());
    Ok(())
}
