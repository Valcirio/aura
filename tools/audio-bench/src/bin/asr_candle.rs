//! asr-candle: Whisper large-v3-turbo (HF safetensors) on candle, CPU only, greedy pt transcription.
//! Decoding adapted from candle-examples/examples/whisper/main.rs (tag 0.11.0), simplified:
//! temperature 0, no timestamps, forced language + transcribe, no fallback / no-speech logic.
use anyhow::{anyhow, bail, Context, Result};
use audio_bench::{emit, ensure_file, load_corpus, mean, ms, peak_rss_mb, percentile, write_json, SAMPLE_RATE};
use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::whisper::{self as m, audio, model::Whisper, Config};
use clap::Parser;
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;
use tokenizers::Tokenizer;

#[derive(Parser, Debug)]
struct Args {
    /// Threads for candle (sets RAYON_NUM_THREADS and CANDLE_NUM_THREADS).
    #[arg(long, default_value_t = 4)]
    threads: usize,
    #[arg(long, default_value = "f32", value_parser = ["f32", "f16", "bf16"])]
    dtype: String,
    #[arg(long, default_value = "data/corpus")]
    corpus: PathBuf,
    #[arg(long, default_value = "data/models")]
    models_dir: PathBuf,
    #[arg(long, default_value = "pt")]
    language: String,
    #[arg(long)]
    out: Option<PathBuf>,
}

struct Ctx<'a> {
    tokenizer: &'a Tokenizer,
    filters: &'a [f32],
    device: &'a Device,
    dtype: DType,
    prompt: Vec<u32>,
    eot: u32,
    suppress: Vec<bool>,
    begin_suppress: Vec<u32>,
}

fn token_id(tok: &Tokenizer, t: &str) -> Result<u32> {
    tok.token_to_id(t).ok_or_else(|| anyhow!("no token id for {t}"))
}

fn transcribe(model: &mut Whisper, cx: &Ctx, pcm: &[f32]) -> Result<String> {
    if pcm.len() > m::N_SAMPLES {
        bail!("clip longer than 30 s is not supported by this bench ({} samples)", pcm.len());
    }
    let n_mel = model.config.num_mel_bins;
    let max_target = model.config.max_target_positions;

    // mel (CPU, multi-threaded inside candle) -> (1, n_mel, 3000)
    let mel = audio::pcm_to_mel(&model.config, pcm, cx.filters);
    let n_frames = mel.len() / n_mel;
    let mel = Tensor::from_vec(mel, (1, n_mel, n_frames), cx.device)?;
    let mel = if n_frames > m::N_FRAMES { mel.narrow(2, 0, m::N_FRAMES)? } else { mel };
    let mel = if cx.dtype != DType::F32 { mel.to_dtype(cx.dtype)? } else { mel };

    let audio_features = model.encoder.forward(&mel, true)?;

    let sample_len = max_target / 2;
    let mut tokens = cx.prompt.clone();
    let prompt_len = tokens.len();
    for i in 0..sample_len {
        let tokens_t = Tensor::new(tokens.as_slice(), cx.device)?.unsqueeze(0)?;
        let ys = model.decoder.forward(&tokens_t, &audio_features, i == 0)?;
        let (_, seq_len, _) = ys.dims3()?;
        let logits = model.decoder.final_linear(&ys.i((..1, seq_len - 1..))?)?.i(0)?.i(0)?;
        let mut logits: Vec<f32> = logits.to_dtype(DType::F32)?.to_vec1()?;
        for (j, l) in logits.iter_mut().enumerate() {
            if cx.suppress.get(j).copied().unwrap_or(false) {
                *l = f32::NEG_INFINITY;
            }
        }
        if i == 0 {
            for &t in &cx.begin_suppress {
                if let Some(l) = logits.get_mut(t as usize) {
                    *l = f32::NEG_INFINITY;
                }
            }
        }
        let next = logits
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(i, _)| i as u32)
            .ok_or_else(|| anyhow!("empty logits"))?;
        tokens.push(next);
        if next == cx.eot || tokens.len() > max_target {
            break;
        }
    }
    let text = cx
        .tokenizer
        .decode(&tokens[prompt_len..], true)
        .map_err(|e| anyhow!("tokenizer decode: {e}"))?;
    Ok(text.trim().to_string())
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.threads == 0 {
        bail!("--threads must be >= 1");
    }
    // Must happen before any candle call (thread pools read these lazily).
    std::env::set_var("RAYON_NUM_THREADS", args.threads.to_string());
    std::env::set_var("CANDLE_NUM_THREADS", args.threads.to_string());

    let dtype = match args.dtype.as_str() {
        "f32" => DType::F32,
        "f16" => DType::F16,
        "bf16" => DType::BF16,
        d => bail!("unsupported dtype {d}"),
    };

    let mdir = args.models_dir.join("whisper-turbo-hf");
    let config_p = mdir.join("config.json");
    let tokenizer_p = mdir.join("tokenizer.json");
    let weights_p = mdir.join("model.safetensors");
    let filters_p = mdir.join("melfilters128.bytes");
    for p in [&config_p, &tokenizer_p, &weights_p, &filters_p] {
        ensure_file(p)?;
    }
    let out = args.out.clone().unwrap_or_else(|| {
        PathBuf::from(format!("data/results/asr-candle-turbo-{}-t{}.json", args.dtype, args.threads))
    });
    let clips = load_corpus(&args.corpus)?;
    for c in &clips {
        if c.sample_rate != SAMPLE_RATE {
            bail!("clip {} has sample rate {} (expected {SAMPLE_RATE})", c.id, c.sample_rate);
        }
    }

    let config_str = fs::read_to_string(&config_p)?;
    let config: Config = serde_json::from_str(&config_str).context("parse config.json")?;
    let cfg_json: serde_json::Value = serde_json::from_str(&config_str)?;
    let mut begin_suppress: Vec<u32> = cfg_json["begin_suppress_tokens"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_u64().map(|x| x as u32)).collect())
        .unwrap_or_default();

    let tokenizer = Tokenizer::from_file(&tokenizer_p).map_err(|e| anyhow!("tokenizer: {e}"))?;

    // Mel filters: 128 x 201 f32 little-endian, read at runtime.
    let bytes = fs::read(&filters_p)?;
    let expected = config.num_mel_bins * (m::N_FFT / 2 + 1) * 4;
    if bytes.len() != expected {
        bail!("{}: {} bytes, expected {expected}", filters_p.display(), bytes.len());
    }
    let filters: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();

    let sot = token_id(&tokenizer, m::SOT_TOKEN)?;
    let lang = token_id(&tokenizer, &format!("<|{}|>", args.language))
        .map_err(|_| anyhow!("language {} not supported by tokenizer", args.language))?;
    let transcribe_tok = token_id(&tokenizer, m::TRANSCRIBE_TOKEN)?;
    let no_ts = token_id(&tokenizer, m::NO_TIMESTAMPS_TOKEN)?;
    let eot = token_id(&tokenizer, m::EOT_TOKEN)?;
    if !begin_suppress.contains(&eot) {
        begin_suppress.push(eot);
    }
    // Suppress every special token above EOT (langs, task, SOT, notimestamps, timestamps)
    // plus config.suppress_tokens (empty for this checkpoint's config.json).
    let suppress: Vec<bool> = (0..config.vocab_size as u32)
        .map(|i| i > eot || config.suppress_tokens.contains(&i))
        .collect();

    let device = Device::Cpu;
    eprintln!("[asr-candle] loading turbo dtype={} threads={}", args.dtype, args.threads);
    let t0 = Instant::now();
    let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[&weights_p], dtype, &device)? };
    let mut model = Whisper::load(&vb, config)?;
    let load_ms = ms(t0.elapsed());
    eprintln!("[asr-candle] loaded in {load_ms:.0} ms");

    let cx = Ctx {
        tokenizer: &tokenizer,
        filters: &filters,
        device: &device,
        dtype,
        prompt: vec![sot, lang, transcribe_tok, no_ts],
        eot,
        suppress,
        begin_suppress,
    };

    // Warm-up on clip 0 (excluded from stats).
    let _ = transcribe(&mut model, &cx, &clips[0].samples)?;

    let mut lat = Vec::new();
    let mut rtfs = Vec::new();
    let mut clips_json = Vec::new();
    for c in &clips {
        let t = Instant::now();
        let text = transcribe(&mut model, &cx, &c.samples)?;
        let latency_ms = ms(t.elapsed());
        let dur = c.duration_s();
        let rtf = latency_ms / 1000.0 / dur;
        if text.is_empty() {
            eprintln!("[asr-candle] WARNING: empty transcript for clip {}", c.id);
        }
        lat.push(latency_ms);
        rtfs.push(rtf);
        let row = json!({"id": c.id, "duration_s": dur, "latency_ms": latency_ms, "rtf": rtf, "text": text});
        let mut line = row.clone();
        line["type"] = json!("clip");
        emit(&line);
        clips_json.push(row);
    }

    let summary = json!({
        "engine": "candle",
        "model": "turbo",
        "threads": args.threads,
        "dtype": args.dtype,
        "language": args.language,
        "decoding": "greedy",
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
    eprintln!("[asr-candle] wrote {}", out.display());
    Ok(())
}
