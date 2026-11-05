use crate::{
    AppError, Result,
    speech::{Pcm, SpeechBackend},
};
use serde::Serialize;
#[cfg(any(feature = "native-audio", test))]
use serde_json::json;
use std::sync::atomic::AtomicBool;
#[cfg(feature = "native-audio")]
use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub direction: String,
    pub default: bool,
}

#[cfg(any(feature = "native-audio", test))]
const DEVICE_ID_PREFIX: &str = "v1:";
#[cfg(any(feature = "native-audio", test))]
const DEVICE_RESELECT: &str = "Selecione novamente o dispositivo nas configurações de áudio.";

#[cfg(any(feature = "native-audio", test))]
#[derive(Debug, PartialEq, Eq)]
struct DeviceId {
    direction: String,
    name: String,
}

#[cfg(any(feature = "native-audio", test))]
fn device_unavailable(reason: &str) -> AppError {
    AppError::new("DEVICE_UNAVAILABLE", format!("{reason} {DEVICE_RESELECT}"))
}

#[cfg(any(feature = "native-audio", test))]
fn device_id(direction: &str, name: &str) -> String {
    format!("{DEVICE_ID_PREFIX}{}", json!([direction, name]))
}

#[cfg(any(feature = "native-audio", test))]
fn parse_device_id(id: &str) -> Result<DeviceId> {
    let malformed = || device_unavailable("O identificador do dispositivo salvo é inválido.");
    let Some(encoded) = id.strip_prefix(DEVICE_ID_PREFIX) else {
        return Err(device_unavailable(
            "O dispositivo salvo usa um formato anterior de identificação.",
        ));
    };
    let value: serde_json::Value = serde_json::from_str(encoded).map_err(|_| malformed())?;
    let Some(parts) = value.as_array().filter(|parts| parts.len() == 2) else {
        return Err(malformed());
    };
    let (Some(direction), Some(name)) = (parts[0].as_str(), parts[1].as_str()) else {
        return Err(malformed());
    };
    if direction.is_empty() || name.is_empty() {
        return Err(malformed());
    }
    Ok(DeviceId {
        direction: direction.into(),
        name: name.into(),
    })
}

#[cfg(any(feature = "native-audio", test))]
fn resolve_device(direction: &str, id: &str, names: &[Option<&str>]) -> Result<usize> {
    let parsed = parse_device_id(id)?;
    if parsed.direction != direction {
        return Err(device_unavailable(
            "O dispositivo salvo pertence a outra direção de áudio.",
        ));
    }
    let mut found = None;
    for (index, name) in names.iter().enumerate() {
        let Some(name) = name else {
            return Err(device_unavailable(
                "Não foi possível ler o nome de todos os dispositivos disponíveis.",
            ));
        };
        if *name == parsed.name {
            if found.is_some() {
                return Err(device_unavailable(
                    "Existem vários dispositivos com o mesmo nome.",
                ));
            }
            found = Some(index);
        }
    }
    found.ok_or_else(|| device_unavailable("O dispositivo salvo não foi encontrado."))
}

#[cfg(any(feature = "native-audio", test))]
fn unique_device_names(names: &[Option<String>]) -> Vec<String> {
    let mut unique = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let Some(name) = name else {
            continue;
        };
        let duplicated = names.iter().enumerate().any(|(other, candidate)| {
            other != index && candidate.as_deref() == Some(name.as_str())
        });
        if !duplicated {
            unique.push(name.clone());
        }
    }
    unique
}

#[cfg(feature = "native-audio")]
mod native {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::{collections::VecDeque, sync::mpsc, time::Duration};
    use support_core::audio::{Resampler, VadSegmenter};

    pub fn devices() -> Result<Vec<AudioDevice>> {
        let host = cpal::default_host();
        let mut result = Vec::new();
        let default_in = host.default_input_device().and_then(|d| d.name().ok());
        let default_out = host.default_output_device().and_then(|d| d.name().ok());
        for (direction, list, default) in [
            ("input", host.input_devices(), default_in),
            ("output", host.output_devices(), default_out),
        ] {
            let mut names = Vec::new();
            for device in list.map_err(|_| {
                AppError::new(
                    "DEVICE_UNAVAILABLE",
                    "Não foi possível listar os dispositivos",
                )
            })? {
                names.push(device.name().ok());
            }
            for name in unique_device_names(&names) {
                result.push(AudioDevice {
                    id: device_id(direction, &name),
                    default: default.as_ref() == Some(&name),
                    name,
                    direction: direction.into(),
                });
            }
        }
        Ok(result)
    }
    fn device(input: bool, id: Option<&str>) -> Result<cpal::Device> {
        let host = cpal::default_host();
        let Some(id) = id else {
            return if input {
                host.default_input_device()
            } else {
                host.default_output_device()
            }
            .ok_or_else(|| device_unavailable("O dispositivo padrão do sistema não existe."));
        };
        let direction = if input { "input" } else { "output" };
        let listed = if input {
            host.input_devices()
        } else {
            host.output_devices()
        }
        .map_err(|_| {
            AppError::new(
                "DEVICE_UNAVAILABLE",
                "Não foi possível listar os dispositivos",
            )
        })?;
        let devices = listed
            .map(|device| {
                let name = device.name().ok();
                (device, name)
            })
            .collect::<Vec<_>>();
        let names = devices
            .iter()
            .map(|(_, name)| name.as_deref())
            .collect::<Vec<_>>();
        let index = resolve_device(direction, id, &names)?;
        devices
            .into_iter()
            .nth(index)
            .map(|(device, _)| device)
            .ok_or_else(|| device_unavailable("O dispositivo salvo não foi encontrado."))
    }
    fn input_stream<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        tx: mpsc::SyncSender<Vec<f32>>,
        failure: Arc<AtomicU8>,
    ) -> Result<cpal::Stream>
    where
        T: cpal::SizedSample + cpal::Sample,
        f32: cpal::FromSample<T>,
    {
        let channels = usize::from(config.channels);
        let errors = failure.clone();
        device
            .build_input_stream(
                config,
                move |data: &[T], _| {
                    let mono = data
                        .chunks_exact(channels)
                        .map(|frame| {
                            frame
                                .iter()
                                .map(|sample| sample.to_sample::<f32>())
                                .sum::<f32>()
                                / channels as f32
                        })
                        .collect();
                    if matches!(tx.try_send(mono), Err(mpsc::TrySendError::Full(_))) {
                        failure.store(2, Ordering::Relaxed);
                    }
                },
                move |_| {
                    errors.store(1, Ordering::Relaxed);
                },
                None,
            )
            .map_err(|error| classify_input_error(&error))
    }
    pub(super) fn classify_input_error(error: &cpal::BuildStreamError) -> AppError {
        match error {
            cpal::BuildStreamError::DeviceNotAvailable => AppError::new(
                "DEVICE_UNAVAILABLE",
                "Microfone indisponível. Conecte ou selecione outro dispositivo de entrada em Configurações.",
            ),
            cpal::BuildStreamError::StreamConfigNotSupported
            | cpal::BuildStreamError::InvalidArgument => AppError::new(
                "AUDIO_FORMAT",
                "O microfone não aceita o formato necessário. Selecione outro dispositivo de entrada.",
            ),
            _ => AppError::new(
                "MIC_OPEN_FAILED",
                "Não foi possível abrir o microfone. Verifique se o sistema permite o acesso, se outro aplicativo não o está usando e se o dispositivo de entrada está correto.",
            ),
        }
    }
    pub fn capture(
        worker: &SpeechBackend,
        input: Option<&str>,
        cancel: &CancellationToken,
        finish: &AtomicBool,
        mut level: impl FnMut(f32),
        ready: impl FnOnce(),
    ) -> Result<Vec<f32>> {
        worker.vad(&[0.; 512])?;
        worker.vad_reset()?;
        if cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        let device = device(true, input)?;
        let supported = device.default_input_config().map_err(|_| {
            AppError::new("DEVICE_UNAVAILABLE", "Formato do microfone indisponível")
        })?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        if !(8000..=192000).contains(&config.sample_rate.0) || config.channels == 0 {
            return Err(AppError::new(
                "AUDIO_FORMAT",
                "Formato de microfone não suportado",
            ));
        }
        let (tx, rx) = mpsc::sync_channel(32);
        let failure = Arc::new(AtomicU8::new(0));
        let stream = match format {
            cpal::SampleFormat::F32 => input_stream::<f32>(&device, &config, tx, failure.clone())?,
            cpal::SampleFormat::I16 => input_stream::<i16>(&device, &config, tx, failure.clone())?,
            cpal::SampleFormat::U16 => input_stream::<u16>(&device, &config, tx, failure.clone())?,
            _ => {
                return Err(AppError::new(
                    "AUDIO_FORMAT",
                    "O microfone precisa suportar f32, i16 ou u16",
                ));
            }
        };
        stream.play().map_err(|_| {
            AppError::new("DEVICE_UNAVAILABLE", "Não foi possível iniciar o microfone")
        })?;
        ready();
        let mut resample = Resampler::new(config.sample_rate.0, 16000);
        let mut frames = VecDeque::new();
        let mut segmenter = VadSegmenter::default();
        loop {
            if cancel.is_cancelled() {
                return Err(AppError::cancelled());
            }
            if finish.swap(false, Ordering::Relaxed) {
                return segmenter.finish().ok_or_else(|| {
                    AppError::new(
                        "EMPTY_SPEECH",
                        "Nenhuma fala detectada. Tente falar um pouco mais.",
                    )
                });
            }
            match failure.load(Ordering::Relaxed) {
                1 => {
                    return Err(AppError::new(
                        "DEVICE_UNAVAILABLE",
                        "Microfone desconectado ou indisponível",
                    ));
                }
                2 => {
                    return Err(AppError::new(
                        "AUDIO_OVERRUN",
                        "O processamento não acompanhou a captura. Feche tarefas pesadas e tente novamente.",
                    ));
                }
                _ => {}
            }
            let data = match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(data) => data,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => {
                    return Err(AppError::new(
                        "DEVICE_UNAVAILABLE",
                        "A captura foi encerrada",
                    ));
                }
            };
            level(rms(&data));
            frames.extend(resample.push(&data));
            while frames.len() >= 512 {
                let frame = frames.drain(..512).collect::<Vec<_>>();
                let probability = worker.vad(&frame)?;
                if cancel.is_cancelled() {
                    return Err(AppError::cancelled());
                }
                if let Some(audio) = segmenter.push(&frame, probability) {
                    drop(stream);
                    level(0.);
                    return Ok(audio);
                }
            }
        }
    }
    struct Playback {
        position: AtomicUsize,
        tail_us: AtomicU64,
        failed: AtomicBool,
        origin: std::time::Instant,
        levels: mpsc::SyncSender<(u64, f32)>,
    }
    fn output_stream<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        samples: Arc<Vec<f32>>,
        state: Arc<Playback>,
    ) -> Result<cpal::Stream>
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        let channels = usize::from(config.channels);
        let rate = f64::from(config.sample_rate.0);
        let errors = state.clone();
        device
            .build_output_stream(
                config,
                move |data: &mut [T], info| {
                    let mut index = state.position.load(Ordering::Relaxed);
                    let mut power = 0.;
                    let mut count = 0;
                    for frame in data.chunks_mut(channels) {
                        let value = samples.get(index).copied().unwrap_or(0.).clamp(-1., 1.);
                        for sample in frame {
                            *sample = T::from_sample(value);
                        }
                        if index < samples.len() {
                            power += value * value;
                            count += 1;
                        }
                        index += 1;
                    }
                    let stamp = info.timestamp();
                    let delay = stamp
                        .playback
                        .duration_since(&stamp.callback)
                        .unwrap_or_default();
                    let when = state.origin.elapsed().as_micros() as u64 + delay.as_micros() as u64;
                    let level = if count > 0 {
                        (power / count as f32).sqrt()
                    } else {
                        0.
                    };
                    let _ = state.levels.try_send((when, level));
                    if count > 0 {
                        state.tail_us.store(
                            when + (count as f64 / rate * 1_000_000.) as u64,
                            Ordering::Release,
                        );
                    }
                    state.position.store(index, Ordering::Release);
                },
                move |_| {
                    errors.failed.store(true, Ordering::Relaxed);
                },
                None,
            )
            .map_err(|_| {
                AppError::new(
                    "DEVICE_UNAVAILABLE",
                    "Não foi possível abrir a saída de áudio",
                )
            })
    }
    pub fn play(
        pcm: Pcm,
        output: Option<&str>,
        cancel: &CancellationToken,
        mut level: impl FnMut(f32),
    ) -> Result<()> {
        if cancel.is_cancelled() {
            return Err(AppError::cancelled());
        }
        let device = device(false, output)?;
        let supported = device
            .default_output_config()
            .map_err(|_| AppError::new("DEVICE_UNAVAILABLE", "Saída indisponível"))?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        if config.channels == 0 || config.sample_rate.0 == 0 {
            return Err(AppError::new("AUDIO_FORMAT", "Formato de saída inválido"));
        }
        let mut resampler = Resampler::new(pcm.sample_rate, config.sample_rate.0);
        let mut samples = resampler.push(&pcm.samples);
        samples.extend(resampler.push(&[0.; 32]));
        let samples = Arc::new(samples);
        let (levels_tx, levels_rx) = mpsc::sync_channel(64);
        let state = Arc::new(Playback {
            position: AtomicUsize::new(0),
            tail_us: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            origin: std::time::Instant::now(),
            levels: levels_tx,
        });
        let stream = match format {
            cpal::SampleFormat::F32 => {
                output_stream::<f32>(&device, &config, samples.clone(), state.clone())?
            }
            cpal::SampleFormat::I16 => {
                output_stream::<i16>(&device, &config, samples.clone(), state.clone())?
            }
            cpal::SampleFormat::U16 => {
                output_stream::<u16>(&device, &config, samples.clone(), state.clone())?
            }
            _ => {
                return Err(AppError::new(
                    "AUDIO_FORMAT",
                    "Formato da saída não suportado",
                ));
            }
        };
        stream.play().map_err(|_| {
            AppError::new("DEVICE_UNAVAILABLE", "Não foi possível reproduzir o áudio")
        })?;
        let mut pending = VecDeque::new();
        let mut last_publish = std::time::Instant::now();
        while state.position.load(Ordering::Acquire) < samples.len()
            || (state.origin.elapsed().as_micros() as u64) < state.tail_us.load(Ordering::Acquire)
        {
            if cancel.is_cancelled() {
                drop(stream);
                level(0.);
                return Err(AppError::cancelled());
            }
            if state.failed.load(Ordering::Relaxed) {
                return Err(AppError::new(
                    "DEVICE_UNAVAILABLE",
                    "A saída de áudio foi desconectada",
                ));
            }
            pending.extend(levels_rx.try_iter());
            let now = state.origin.elapsed().as_micros() as u64;
            let mut current = None;
            while pending.front().is_some_and(|(when, _)| *when <= now) {
                current = pending.pop_front().map(|(_, v)| v);
            }
            if last_publish.elapsed() >= Duration::from_millis(30) {
                if let Some(value) = current {
                    level(value);
                    last_publish = std::time::Instant::now();
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(stream);
        level(0.);
        if cancel.is_cancelled() {
            Err(AppError::cancelled())
        } else {
            Ok(())
        }
    }
}
const WAV_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Decodes an embedded mono PCM16 RIFF/WAVE asset into native `Pcm`.
pub fn decode_wav_pcm16(bytes: &[u8], expected_rate: u32) -> Result<Pcm> {
    fn bad(message: &str) -> AppError {
        AppError::new(
            "AUDIO_FORMAT",
            format!("Áudio embutido inválido: {message}"),
        )
    }
    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }
    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }
    if bytes.len() < 12 || bytes.len() > WAV_MAX_BYTES {
        return Err(bad("tamanho"));
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad("cabeçalho RIFF"));
    }
    if u32_at(bytes, 4) as usize != bytes.len() - 8 {
        return Err(bad("tamanho RIFF"));
    }
    let mut offset = 12;
    let mut format_seen = false;
    let mut data: Option<&[u8]> = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32_at(bytes, offset + 4) as usize;
        let start = offset + 8;
        let end = start.checked_add(size).filter(|end| *end <= bytes.len());
        let Some(end) = end else {
            return Err(bad("bloco truncado"));
        };
        match id {
            b"fmt " => {
                if format_seen || size < 16 {
                    return Err(bad("bloco fmt"));
                }
                let tag = u16_at(bytes, start);
                let channels = u16_at(bytes, start + 2);
                let rate = u32_at(bytes, start + 4);
                let byte_rate = u32_at(bytes, start + 8);
                let align = u16_at(bytes, start + 12);
                let bits = u16_at(bytes, start + 14);
                if tag != 1 || bits != 16 {
                    return Err(bad("esperado PCM de 16 bits"));
                }
                if channels != 1 {
                    return Err(bad("esperado áudio mono"));
                }
                if rate != expected_rate {
                    return Err(bad("taxa de amostragem inesperada"));
                }
                if align != 2 || byte_rate != rate * 2 {
                    return Err(bad("alinhamento"));
                }
                format_seen = true;
            }
            b"data" => {
                if !format_seen || data.is_some() {
                    return Err(bad("bloco data"));
                }
                data = Some(&bytes[start..end]);
            }
            _ => {}
        }
        offset = end + (size & 1);
    }
    let data = data.ok_or_else(|| bad("sem dados"))?;
    if data.is_empty() || data.len() % 2 != 0 {
        return Err(bad("dados desalinhados"));
    }
    let samples = data
        .chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.)
        .collect();
    Ok(Pcm {
        sample_rate: expected_rate,
        samples,
    })
}
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        0.
    } else {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }
}
#[cfg(feature = "native-audio")]
pub use native::{capture, devices, play};
#[cfg(not(feature = "native-audio"))]
pub fn devices() -> Result<Vec<AudioDevice>> {
    Ok(Vec::new())
}
#[cfg(not(feature = "native-audio"))]
pub fn capture(
    _: &SpeechBackend,
    _: Option<&str>,
    _: &CancellationToken,
    _: &AtomicBool,
    _: impl FnMut(f32),
    _: impl FnOnce(),
) -> Result<Vec<f32>> {
    Err(AppError::new("AUDIO_UNAVAILABLE", "Build sem áudio nativo"))
}
#[cfg(not(feature = "native-audio"))]
pub fn play(_: Pcm, _: Option<&str>, _: &CancellationToken, _: impl FnMut(f32)) -> Result<()> {
    Err(AppError::new("AUDIO_UNAVAILABLE", "Build sem áudio nativo"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(rate: u32, channels: u16, bits: u16, tag: u16, data: &[u8]) -> Vec<u8> {
        let mut out = b"RIFF".to_vec();
        out.extend(((36 + data.len()) as u32).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(tag.to_le_bytes());
        out.extend(channels.to_le_bytes());
        out.extend(rate.to_le_bytes());
        out.extend((rate * u32::from(channels) * u32::from(bits / 8)).to_le_bytes());
        out.extend((channels * (bits / 8)).to_le_bytes());
        out.extend(bits.to_le_bytes());
        out.extend(b"data");
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(data);
        out
    }

    #[test]
    fn decodes_valid_pcm16() {
        let pcm =
            decode_wav_pcm16(&wav(24000, 1, 16, 1, &[0, 0, 0, 0x40, 0, 0xc0]), 24000).unwrap();
        assert_eq!(pcm.sample_rate, 24000);
        assert_eq!(pcm.samples, vec![0., 0.5, -0.5]);
    }

    #[test]
    fn rejects_malformed_wav() {
        let good = wav(24000, 1, 16, 1, &[1, 0, 2, 0]);
        assert!(decode_wav_pcm16(&good, 24000).is_ok());
        assert!(decode_wav_pcm16(&good, 16000).is_err());
        assert!(decode_wav_pcm16(&[], 24000).is_err());
        assert!(decode_wav_pcm16(&good[..20], 24000).is_err());
        assert!(decode_wav_pcm16(&good[..good.len() - 1], 24000).is_err());
        assert!(decode_wav_pcm16(&wav(24000, 2, 16, 1, &[0; 8]), 24000).is_err());
        assert!(decode_wav_pcm16(&wav(24000, 1, 8, 1, &[0; 4]), 24000).is_err());
        assert!(decode_wav_pcm16(&wav(24000, 1, 16, 3, &[0; 4]), 24000).is_err());
        assert!(decode_wav_pcm16(&wav(24000, 1, 16, 1, &[]), 24000).is_err());
        assert!(decode_wav_pcm16(&wav(24000, 1, 16, 1, &[0; 3]), 24000).is_err());
        let mut bad_magic = good.clone();
        bad_magic[0] = b'X';
        assert!(decode_wav_pcm16(&bad_magic, 24000).is_err());
        let mut oversized = good.clone();
        oversized[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_wav_pcm16(&oversized, 24000).is_err());
        let mut missing_fmt = b"RIFF".to_vec();
        missing_fmt.extend(20u32.to_le_bytes());
        missing_fmt.extend(b"WAVEdata");
        missing_fmt.extend(4u32.to_le_bytes());
        missing_fmt.extend([0; 4]);
        assert!(decode_wav_pcm16(&missing_fmt, 24000).is_err());
    }

    fn name_list<'a>(names: &'a [&'a str]) -> Vec<Option<&'a str>> {
        names.iter().map(|name| Some(*name)).collect()
    }

    #[test]
    fn ids_roundtrip_escaped_and_unicode_names_in_both_directions() {
        for direction in ["input", "output"] {
            for name in [
                "Microfone \"USB\" C3",
                "Backslash \\ interno",
                "Caixa d'água · Fone — ação",
                "v1:[\"input\",\"Fone\"]",
                "日本語マイク",
                "Fone, com vírgulas",
            ] {
                let id = device_id(direction, name);
                assert_eq!(
                    parse_device_id(&id).unwrap(),
                    DeviceId {
                        direction: direction.into(),
                        name: name.into(),
                    }
                );
                let listed = ["Outro", name, "Terceiro"];
                assert_eq!(
                    resolve_device(direction, &id, &name_list(&listed)).unwrap(),
                    1
                );
            }
        }
    }

    #[test]
    fn explicit_ids_survive_enumeration_reorder() {
        for direction in ["input", "output"] {
            let first = ["Fone USB", "Microfone interno"];
            let id = device_id(direction, "Microfone interno");
            assert_eq!(
                resolve_device(direction, &id, &name_list(&first)).unwrap(),
                1
            );
            let reordered = ["Microfone interno", "Fone USB"];
            assert_eq!(
                resolve_device(direction, &id, &name_list(&reordered)).unwrap(),
                0
            );
            assert!(resolve_device(direction, "input:1", &name_list(&reordered)).is_err());
            assert!(resolve_device(direction, "output:0", &name_list(&reordered)).is_err());
        }
    }

    #[test]
    fn missing_duplicate_and_unreadable_names_fail_closed() {
        for direction in ["input", "output"] {
            let id = device_id(direction, "Fone");
            for listed in [&[][..], &["Outro"][..]] {
                let error = resolve_device(direction, &id, &name_list(listed)).unwrap_err();
                assert_eq!(error.code, "DEVICE_UNAVAILABLE");
                assert!(error.message.contains("Selecione novamente"));
            }
            let duplicate =
                resolve_device(direction, &id, &name_list(&["Fone", "Fone"])).unwrap_err();
            assert_eq!(duplicate.code, "DEVICE_UNAVAILABLE");
            assert!(duplicate.message.contains("Selecione novamente"));
            assert!(resolve_device(direction, &id, &[None]).is_err());
            assert!(resolve_device(direction, &id, &[Some("Fone"), None]).is_err());
        }
    }

    #[test]
    fn unreadable_and_duplicated_names_are_not_offered() {
        let names = [
            Some("Fone".to_string()),
            None,
            Some("Caixa".to_string()),
            Some("Caixa".to_string()),
            Some("Teclado".to_string()),
        ];
        assert_eq!(unique_device_names(&names), vec!["Fone", "Teclado"]);
        assert!(unique_device_names(&[None, None]).is_empty());
        assert_eq!(
            unique_device_names(&[Some("A".into()), Some("B".into())]),
            vec!["A", "B"]
        );
    }

    #[test]
    fn wrong_direction_is_rejected() {
        let listed = name_list(&["Fone"]);
        let output = device_id("output", "Fone");
        let error = resolve_device("input", &output, &listed).unwrap_err();
        assert_eq!(error.code, "DEVICE_UNAVAILABLE");
        assert!(error.message.contains("Selecione novamente"));
        assert_eq!(resolve_device("output", &output, &listed).unwrap(), 0);
        let input = device_id("input", "Fone");
        assert!(resolve_device("output", &input, &listed).is_err());
    }

    #[test]
    fn malformed_and_legacy_ids_are_rejected_with_reselection_guidance() {
        let listed = name_list(&["Fone"]);
        for id in [
            "",
            "input:0",
            "output:1",
            "v1:",
            "v1:[]",
            "v1:{}",
            "v1:\"Fone\"",
            "v1:[\"input\"]",
            "v1:[\"input\",\"Fone\",\"extra\"]",
            "v1:[1,2]",
            "v1:[\"input\",null]",
            "v1:[\"\",\"Fone\"]",
            "v1:[\"input\",\"\"]",
            "v2:[\"input\",\"Fone\"]",
        ] {
            for direction in ["input", "output"] {
                let error = resolve_device(direction, id, &listed).unwrap_err();
                assert_eq!(error.code, "DEVICE_UNAVAILABLE", "{id}");
                assert!(error.message.contains("Selecione novamente"), "{id}");
            }
        }
        assert_eq!(
            parse_device_id("v1:[\"input\",\"Fone\"]").unwrap().name,
            "Fone"
        );
    }

    #[test]
    fn no_saved_value_ever_falls_back_to_the_system_default() {
        let listed = name_list(&["Fone"]);
        for id in ["", "input:0", "v1:[\"input\",\"Outro\"]"] {
            for direction in ["input", "output"] {
                assert!(resolve_device(direction, id, &listed).is_err(), "{id}");
            }
        }
        let output = "v1:[\"output\",\"Fone\"]";
        assert!(resolve_device("input", output, &listed).is_err());
        assert_eq!(resolve_device("output", output, &listed).unwrap(), 0);
    }

    #[cfg(feature = "native-audio")]
    #[test]
    fn input_errors_are_not_all_permission_denials() {
        use cpal::BuildStreamError as E;
        let unavailable = native::classify_input_error(&E::DeviceNotAvailable);
        assert_eq!(unavailable.code, "DEVICE_UNAVAILABLE");
        let format = native::classify_input_error(&E::StreamConfigNotSupported);
        assert_eq!(format.code, "AUDIO_FORMAT");
        let other = native::classify_input_error(&E::StreamIdOverflow);
        assert_eq!(other.code, "MIC_OPEN_FAILED");
        assert_ne!(other.code, "MIC_PERMISSION_DENIED");
        assert!(other.message.contains("permite o acesso"));
        assert!(other.message.contains("dispositivo de entrada"));
    }
}
