use crate::{
    AppError, Result,
    speech::{Pcm, SpeechWorker},
};
use serde::Serialize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub direction: String,
    pub default: bool,
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
            for (index, device) in list
                .map_err(|_| {
                    AppError::new(
                        "DEVICE_UNAVAILABLE",
                        "Não foi possível listar os dispositivos",
                    )
                })?
                .enumerate()
            {
                let name = device
                    .name()
                    .unwrap_or_else(|_| format!("Dispositivo {index}"));
                result.push(AudioDevice {
                    id: format!("{direction}:{index}"),
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
        let selected = if let Some(id) = id {
            let (kind, index) = id
                .split_once(':')
                .ok_or_else(|| AppError::invalid("Dispositivo inválido"))?;
            if kind != if input { "input" } else { "output" } {
                return Err(AppError::invalid("Direção de dispositivo inválida"));
            }
            let index = index
                .parse::<usize>()
                .map_err(|_| AppError::invalid("Dispositivo inválido"))?;
            if input {
                host.input_devices()
            } else {
                host.output_devices()
            }
            .ok()
            .and_then(|mut list| list.nth(index))
        } else if input {
            host.default_input_device()
        } else {
            host.default_output_device()
        };
        selected.ok_or_else(|| {
            AppError::new(
                "DEVICE_UNAVAILABLE",
                "Dispositivo de áudio não encontrado. Atualize a lista em Configurações.",
            )
        })
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
            .map_err(|_| {
                AppError::new(
                    "MIC_PERMISSION_DENIED",
                    "Não foi possível abrir o microfone. Verifique permissão e dispositivo.",
                )
            })
    }
    pub fn capture(
        worker: &SpeechWorker,
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
    _: &SpeechWorker,
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
