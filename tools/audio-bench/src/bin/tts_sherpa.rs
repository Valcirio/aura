use anyhow::{anyhow, bail, Result};
use audio_bench::{emit, ensure_file, mean, ms, path_str, peak_rss_mb, rms, write_json, write_wav_i16};
use clap::Parser;
use serde_json::json;
use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsModelConfig, OfflineTtsVitsModelConfig};
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
    #[arg(long, value_parser = ["cadu", "faber"])]
    voice: String,
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

/// Returns (samples, sample_rate, phonemize_ms, synth_ms).
fn synth(tts: &OfflineTts, lang: &str, text: &str, speed: f32) -> Result<(Vec<f32>, i32, f64, f64)> {
    let t0 = Instant::now();
    let res = piper_phonemize::phonemize(text, lang).ok_or_else(|| anyhow!("phonemize failed for {text:?}"))?;
    let mut cps: Vec<Vec<i32>> = Vec::new();
    for i in 0..res.num_sentences() {
        cps.push(res.get_phonemes(i).unwrap_or_default());
    }
    drop(res);
    if cps.iter().all(|v| v.is_empty()) {
        bail!("phonemizer returned no phonemes for {text:?} (lang={lang})");
    }
    let phonemize_ms = ms(t0.elapsed());

    let t1 = Instant::now();
    let gen_cfg = GenerationConfig { sid: 0, speed, phoneme_codepoints: Some(cps), ..Default::default() };
    let audio = tts
        .generate_with_config("", &gen_cfg, None::<fn(&[f32], f32) -> bool>)
        .ok_or_else(|| anyhow!("generate_with_config failed"))?;
    let synth_ms = ms(t1.elapsed());
    Ok((audio.samples().to_vec(), audio.sample_rate(), phonemize_ms, synth_ms))
}

fn main() -> Result<()> {
    let args = Args::parse();
    let vdir = args.models_dir.join(format!("piper-{}", args.voice));
    let model = vdir.join(format!("pt_BR-{}-medium.onnx", args.voice));
    let tokens = vdir.join("tokens.txt");
    let meta = vdir.join(format!("pt_BR-{}-medium.onnx.json", args.voice));
    ensure_file(&model)?;
    ensure_file(&tokens)?;
    ensure_file(&meta)?;

    // Inference params from the voice metadata (fallback to Piper defaults).
    let meta_v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&meta)?)?;
    let inf = &meta_v["inference"];
    let noise_scale = inf["noise_scale"].as_f64().unwrap_or(0.667) as f32;
    let noise_scale_w = inf["noise_w"].as_f64().unwrap_or(0.8) as f32;
    let length_scale = inf["length_scale"].as_f64().unwrap_or(1.0) as f32;

    let cfg = OfflineTtsConfig {
        model: OfflineTtsModelConfig {
            vits: OfflineTtsVitsModelConfig {
                model: Some(path_str(&model)),
                tokens: Some(path_str(&tokens)),
                noise_scale,
                noise_scale_w,
                length_scale,
                ..Default::default()
            },
            num_threads: args.threads,
            debug: false,
            provider: Some("cpu".into()),
            ..Default::default()
        },
        ..Default::default()
    };

    let t0 = Instant::now();
    let tts = OfflineTts::create(&cfg).ok_or_else(|| anyhow!("OfflineTts::create failed"))?;
    let load_ms = ms(t0.elapsed());

    let lang = {
        let l = tts.lang();
        if l.is_empty() { "pt-br".to_string() } else { l }
    };
    eprintln!("[tts-sherpa] voice={} load={load_ms:.0}ms sr={} lang={lang}", args.voice, tts.sample_rate());

    // Warm-up (also triggers one-time espeak-ng-data extraction); excluded.
    let _ = synth(&tts, &lang, "Olá.", args.speed)?;

    let mut rtfs = Vec::new();
    let mut rows = Vec::new();
    let mut sample_rate = tts.sample_rate();
    let mut any_silent = false;
    for (i, text) in SENTENCES.iter().enumerate() {
        let (samples, sr, ph_ms, syn_ms) = synth(&tts, &lang, text, args.speed)?;
        sample_rate = sr;
        let audio_s = samples.len() as f64 / sr as f64;
        let latency_ms = ph_ms + syn_ms;
        let rtf = latency_ms / 1000.0 / audio_s;
        let r = rms(&samples);
        let silent = r < SILENT_RMS;
        any_silent |= silent;
        rtfs.push(rtf);
        let wav = args.out_dir.join(format!("{}-{:02}.wav", args.voice, i + 1));
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

    let summary = json!({
        "engine": "sherpa-onnx",
        "voice": args.voice,
        "threads": args.threads,
        "load_ms": load_ms,
        "sample_rate": sample_rate,
        "phonemizer_lang": lang,
        "mean_rtf": mean(&rtfs),
        "peak_rss_mb": peak_rss_mb(),
    });
    let mut line = summary.clone();
    line["type"] = json!("summary");
    emit(&line);

    let mut full = summary;
    full["sentences"] = json!(rows);
    let out = args.results_dir.join(format!("tts-sherpa-{}.json", args.voice));
    write_json(&out, &full)?;
    eprintln!("[tts-sherpa] wrote {}", out.display());
    if any_silent {
        eprintln!("[tts-sherpa] FAIL: at least one sentence is silent (rms < {SILENT_RMS})");
        std::process::exit(1);
    }
    Ok(())
}
