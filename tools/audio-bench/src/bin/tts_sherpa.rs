use anyhow::{anyhow, bail, Result};
use audio_bench::{emit, ensure_file, mean, ms, path_str, peak_rss_mb, rms, write_json, write_wav_i16};
use clap::Parser;
use serde_json::json;
use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig, OfflineTtsModelConfig, OfflineTtsSupertonicModelConfig, OfflineTtsVitsModelConfig};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

const SENTENCES: [&str; 5] = [
    "Olá, eu sou a Aura. Como posso ajudar você hoje?",
    "Sua reunião está marcada para amanhã às 14h30.",
    "Hoje é sexta-feira, 30 de outubro de 2026, e a temperatura em São Paulo é de 24 graus.",
    "Para preparar o relatório mensal, reúna os dados de vendas de cada região, compare os resultados com as metas do trimestre e destaque as principais oportunidades de melhoria para a equipe.",
    "O pedido número 4587 foi enviado e deve chegar em até três dias úteis.",
];
const SILENT_RMS: f32 = 0.005;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, value_parser = ["cadu", "faber", "dii", "kokoro-dora", "supertonic"])]
    voice: String,
    #[arg(long)]
    sid: Option<i32>,
    /// Supertonic only: number of denoising steps.
    #[arg(long, default_value_t = 8)]
    num_steps: i32,
    /// Kokoro: disables the tʃ→ʧ / dʒ→ʤ phoneme remap.
    #[arg(long)]
    no_remap: bool,
    #[arg(long, default_value_t = 4)]
    threads: i32,
    #[arg(long, default_value = "data/models")]
    models_dir: PathBuf,
    #[arg(long, default_value = "data/out")]
    out_dir: PathBuf,
    #[arg(long, default_value = "data/results")]
    results_dir: PathBuf,
    #[arg(long, default_value_t = 1.0)]
    speed: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Piper,
    Kokoro,
    Supertonic,
}

struct Gen {
    kind: Kind,
    lang: String,
    remap: bool,
    speed: f32,
    sid: i32,
    num_steps: i32,
}

fn remap_cps(v: &[i32]) -> Vec<i32> {
    let s: String = v.iter().filter_map(|&c| char::from_u32(c as u32)).collect();
    s.replace("tʃ", "ʧ").replace("dʒ", "ʤ").chars().map(|c| c as i32).collect()
}

/// Returns (samples, sample_rate, phonemize_ms, synth_ms).
fn synth(tts: &OfflineTts, g: &Gen, text: &str) -> Result<(Vec<f32>, i32, f64, f64)> {
    let t0 = Instant::now();
    let (cps, text_arg): (Option<Vec<Vec<i32>>>, &str) = if g.kind == Kind::Supertonic {
        (None, text)
    } else {
        let lang = &g.lang;
        let res = piper_phonemize::phonemize(text, lang).ok_or_else(|| anyhow!("phonemize failed for {text:?}"))?;
        let mut cps: Vec<Vec<i32>> = Vec::new();
        for i in 0..res.num_sentences() {
            let v = res.get_phonemes(i).unwrap_or_default();
            cps.push(if g.remap { remap_cps(&v) } else { v });
        }
        drop(res);
        if cps.iter().all(|v| v.is_empty()) {
            bail!("phonemizer returned no phonemes for {text:?} (lang={lang})");
        }
        (Some(cps), "")
    };
    let phonemize_ms = ms(t0.elapsed());

    let t1 = Instant::now();
    let mut gen_cfg = GenerationConfig { sid: g.sid, speed: g.speed, phoneme_codepoints: cps, ..Default::default() };
    if g.kind == Kind::Supertonic {
        gen_cfg.num_steps = g.num_steps;
        gen_cfg.extra = Some(HashMap::from([("lang".to_string(), json!("pt"))]));
    }
    let audio = tts
        .generate_with_config(text_arg, &gen_cfg, None::<fn(&[f32], f32) -> bool>)
        .ok_or_else(|| anyhow!("generate_with_config failed"))?;
    let synth_ms = ms(t1.elapsed());
    Ok((audio.samples().to_vec(), audio.sample_rate(), phonemize_ms, synth_ms))
}

fn main() -> Result<()> {
    let args = Args::parse();
    let kind = match args.voice.as_str() {
        "kokoro-dora" => Kind::Kokoro,
        "supertonic" => Kind::Supertonic,
        _ => Kind::Piper,
    };
    let sid = args.sid.unwrap_or(match kind {
        Kind::Kokoro => 42,
        _ => 0,
    });
    let tag = match kind {
        Kind::Piper => args.voice.clone(),
        Kind::Kokoro => if args.no_remap { "kokoro-dora-noremap".to_string() } else { "kokoro-dora".to_string() },
        Kind::Supertonic => format!("supertonic-sid{sid}"),
    };
    let quality = if args.voice == "dii" { "high" } else { "medium" };
    let vdir = args.models_dir.join(match kind {
        Kind::Piper => format!("piper-{}", args.voice),
        Kind::Kokoro => "kokoro-multi-lang-v1_0".to_string(),
        Kind::Supertonic => "supertonic-3-int8".to_string(),
    });

    let mut model_cfg = OfflineTtsModelConfig {
        num_threads: args.threads,
        debug: false,
        provider: Some("cpu".into()),
        ..Default::default()
    };
    let model_name: String;
    match kind {
        Kind::Piper => {
            let model = vdir.join(format!("pt_BR-{}-{quality}.onnx", args.voice));
            let tokens = vdir.join("tokens.txt");
            let meta = vdir.join(format!("pt_BR-{}-{quality}.onnx.json", args.voice));
            ensure_file(&model)?;
            ensure_file(&tokens)?;
            ensure_file(&meta)?;

            // Inference params from the voice metadata (fallback to Piper defaults).
            let meta_v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&meta)?)?;
            let inf = &meta_v["inference"];
            let noise_scale = inf["noise_scale"].as_f64().unwrap_or(0.667) as f32;
            let noise_scale_w = inf["noise_w"].as_f64().unwrap_or(0.8) as f32;
            let length_scale = inf["length_scale"].as_f64().unwrap_or(1.0) as f32;
            model_cfg.vits = OfflineTtsVitsModelConfig {
                model: Some(path_str(&model)),
                tokens: Some(path_str(&tokens)),
                noise_scale,
                noise_scale_w,
                length_scale,
                ..Default::default()
            };
            model_name = format!("pt_BR-{}-{quality}", args.voice);
        }
        Kind::Kokoro => {
            let model = vdir.join("model.onnx");
            let voices = vdir.join("voices.bin");
            let tokens = vdir.join("tokens.txt");
            ensure_file(&model)?;
            ensure_file(&voices)?;
            ensure_file(&tokens)?;
            model_cfg.kokoro = OfflineTtsKokoroModelConfig {
                model: Some(path_str(&model)),
                voices: Some(path_str(&voices)),
                tokens: Some(path_str(&tokens)),
                ..Default::default()
            };
            model_name = "kokoro-multi-lang-v1_0".to_string();
        }
        Kind::Supertonic => {
            let p = |n: &str| vdir.join(n);
            let files = [
                "duration_predictor.int8.onnx",
                "text_encoder.int8.onnx",
                "vector_estimator.int8.onnx",
                "vocoder.int8.onnx",
                "tts.json",
                "unicode_indexer.bin",
                "voice.bin",
            ];
            for f in files {
                ensure_file(&p(f))?;
            }
            model_cfg.supertonic = OfflineTtsSupertonicModelConfig {
                duration_predictor: Some(path_str(&p(files[0]))),
                text_encoder: Some(path_str(&p(files[1]))),
                vector_estimator: Some(path_str(&p(files[2]))),
                vocoder: Some(path_str(&p(files[3]))),
                tts_json: Some(path_str(&p(files[4]))),
                unicode_indexer: Some(path_str(&p(files[5]))),
                voice_style: Some(path_str(&p(files[6]))),
            };
            model_name = "supertonic-3-int8".to_string();
        }
    }
    let cfg = OfflineTtsConfig { model: model_cfg, ..Default::default() };

    let t0 = Instant::now();
    let tts = OfflineTts::create(&cfg).ok_or_else(|| anyhow!("OfflineTts::create failed"))?;
    let load_ms = ms(t0.elapsed());

    let lang = match kind {
        Kind::Piper => {
            let l = tts.lang();
            if l.is_empty() { "pt-br".to_string() } else { l }
        }
        Kind::Kokoro => "pt-br".to_string(),
        Kind::Supertonic => String::new(),
    };
    eprintln!("[tts-sherpa] voice={tag} load={load_ms:.0}ms sr={} lang={lang}", tts.sample_rate());

    let g = Gen {
        kind,
        lang: lang.clone(),
        remap: kind == Kind::Kokoro && !args.no_remap,
        speed: args.speed,
        sid,
        num_steps: args.num_steps,
    };

    // Warm-up (also triggers one-time espeak-ng-data extraction); excluded.
    let _ = synth(&tts, &g, "Olá.")?;

    let mut rtfs = Vec::new();
    let mut rows = Vec::new();
    let mut sample_rate = tts.sample_rate();
    let mut any_silent = false;
    for (i, text) in SENTENCES.iter().enumerate() {
        let (samples, sr, ph_ms, syn_ms) = synth(&tts, &g, text)?;
        sample_rate = sr;
        let audio_s = samples.len() as f64 / sr as f64;
        let latency_ms = ph_ms + syn_ms;
        let rtf = latency_ms / 1000.0 / audio_s;
        let r = rms(&samples);
        let silent = r < SILENT_RMS;
        any_silent |= silent;
        rtfs.push(rtf);
        let wav = args.out_dir.join(format!("{}-{:02}.wav", tag, i + 1));
        write_wav_i16(&wav, &samples, sr as u32)?;
        let row = json!({
            "idx": i + 1, "text": text, "chars": text.chars().count(),
            "phonemize_ms": ph_ms, "synth_ms": syn_ms, "latency_ms": latency_ms,
            "audio_s": audio_s, "rtf": rtf, "rms": r, "silent": silent, "wav": wav.to_string_lossy()
        });
        let mut line = row.clone();
        line["type"] = json!("sentence");
        emit(&line);
        rows.push(row);
    }

    let speaker = match kind {
        Kind::Piper => args.voice.clone(),
        Kind::Kokoro => if sid == 42 { "pf_dora".to_string() } else { format!("sid{sid}") },
        Kind::Supertonic => if sid < 5 { format!("F{}", sid + 1) } else { format!("M{}", sid - 4) },
    };
    let license = match args.voice.as_str() {
        "dii" => "CC BY-NC (não comercial)",
        "kokoro-dora" => "Apache-2.0",
        "supertonic" => "OpenRAIL-M",
        _ => "CC0",
    };
    let summary = json!({
        "engine": "sherpa-onnx",
        "voice": tag,
        "model": model_name,
        "speaker": speaker,
        "sid": sid,
        "num_steps": if kind == Kind::Supertonic { json!(args.num_steps) } else { json!(null) },
        "license": license,
        "threads": args.threads,
        "load_ms": load_ms,
        "sample_rate": sample_rate,
        "phonemizer_lang": if kind == Kind::Supertonic { json!(null) } else { json!(lang) },
        "mean_rtf": mean(&rtfs),
        "peak_rss_mb": peak_rss_mb(),
    });
    let mut line = summary.clone();
    line["type"] = json!("summary");
    emit(&line);

    let mut full = summary;
    full["sentences"] = json!(rows);
    let out = args.results_dir.join(format!("tts-sherpa-{tag}.json"));
    write_json(&out, &full)?;
    eprintln!("[tts-sherpa] wrote {}", out.display());
    if any_silent {
        eprintln!("[tts-sherpa] FAIL: at least one sentence is silent (rms < {SILENT_RMS})");
        std::process::exit(1);
    }
    Ok(())
}
