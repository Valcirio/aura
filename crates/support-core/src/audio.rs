use std::collections::VecDeque;

/// Segmentação com pre-roll, silêncio final e limite de 45 segundos a 16 kHz.
pub struct VadSegmenter {
    pre: VecDeque<f32>,
    active: Vec<f32>,
    voice_samples: usize,
    silence_samples: usize,
}
impl Default for VadSegmenter {
    fn default() -> Self {
        Self {
            pre: VecDeque::new(),
            active: Vec::new(),
            voice_samples: 0,
            silence_samples: 0,
        }
    }
}
impl VadSegmenter {
    pub fn push(&mut self, frame: &[f32], probability: f32) -> Option<Vec<f32>> {
        let speech = probability >= 0.55;
        if self.active.is_empty() {
            self.pre.extend(frame.iter().copied());
            while self.pre.len() > 4000 {
                self.pre.pop_front();
            }
            self.voice_samples = if speech {
                self.voice_samples + frame.len()
            } else {
                0
            };
            if self.voice_samples >= 3200 {
                self.active.extend(self.pre.drain(..));
            }
            return None;
        }
        self.active.extend_from_slice(frame);
        self.silence_samples = if speech {
            0
        } else {
            self.silence_samples + frame.len()
        };
        if self.silence_samples >= 9600 || self.active.len() >= 720_000 {
            self.voice_samples = 0;
            self.silence_samples = 0;
            let mut audio = std::mem::take(&mut self.active);
            audio.truncate(720_000);
            return Some(audio);
        }
        None
    }
    pub fn finish(&mut self) -> Option<Vec<f32>> {
        if self.active.is_empty() {
            return None;
        }
        self.voice_samples = 0;
        self.silence_samples = 0;
        Some(std::mem::take(&mut self.active))
    }
}

/// Resampler FIR por sinc janelada; preserva fase e histórico entre callbacks.
pub struct Resampler {
    samples: VecDeque<f32>,
    position: f64,
    step: f64,
    cutoff: f64,
}
impl Resampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        Self {
            samples: std::iter::repeat_n(0., 16).collect(),
            position: 16.,
            step: f64::from(input_rate) / f64::from(output_rate),
            cutoff: (f64::from(output_rate) / f64::from(input_rate)).min(1.) * 0.90,
        }
    }
    pub fn push(&mut self, input: &[f32]) -> Vec<f32> {
        self.samples.extend(input.iter().copied());
        let mut output = Vec::new();
        while self.position + 16. < self.samples.len() as f64 {
            let center = self.position.floor() as isize;
            let mut sum = 0.;
            let mut weight_sum = 0.;
            for offset in -15..=16 {
                let index = center + offset;
                if index < 0 {
                    continue;
                }
                let x = index as f64 - self.position;
                let sinc = if x.abs() < 1e-9 {
                    self.cutoff
                } else {
                    (std::f64::consts::PI * x * self.cutoff).sin() / (std::f64::consts::PI * x)
                };
                let window = 0.5 + 0.5 * (std::f64::consts::PI * x / 16.).cos();
                let weight = sinc * window;
                sum += f64::from(self.samples[index as usize]) * weight;
                weight_sum += weight;
            }
            output.push((sum / weight_sum) as f32);
            self.position += self.step;
        }
        let remove = (self.position.floor() as usize)
            .saturating_sub(16)
            .min(self.samples.len());
        self.samples.drain(..remove);
        self.position -= remove as f64;
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_never_creates_turn() {
        let mut v = VadSegmenter::default();
        for _ in 0..200 {
            assert!(v.push(&[0.; 512], 0.1).is_none());
        }
        assert!(v.finish().is_none());
    }
    #[test]
    fn vad_keeps_onset_and_finishes_after_silence() {
        let mut v = VadSegmenter::default();
        for _ in 0..10 {
            assert!(v.push(&[1.; 512], 0.9).is_none());
        }
        let mut result = None;
        for _ in 0..19 {
            result = v.push(&[0.; 512], 0.1).or(result);
        }
        let audio = result.unwrap();
        assert!(audio.iter().filter(|x| **x == 1.).count() >= 5000);
    }
    #[test]
    fn resampling_is_independent_of_callback_boundaries() {
        let input: Vec<f32> = (0..48000).map(|n| (n as f32 * 0.03).sin()).collect();
        let all = Resampler::new(48000, 16000).push(&input);
        let mut r = Resampler::new(48000, 16000);
        let chunked: Vec<f32> = input.chunks(127).flat_map(|c| r.push(c)).collect();
        assert_eq!(all, chunked);
        assert!((all.len() as i32 - 16000).abs() < 10);
    }
}
