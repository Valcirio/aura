use crate::{AppError, Result, config::Paths};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::Mutex,
};

struct WorkerIo {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}
pub struct SpeechWorker {
    paths: Paths,
    io: Mutex<Option<WorkerIo>>,
    child: Mutex<Option<Child>>,
}
#[derive(Clone)]
pub struct Pcm {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}
impl SpeechWorker {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            io: Mutex::new(None),
            child: Mutex::new(None),
        }
    }
    fn start(&self) -> Result<WorkerIo> {
        let script = self.paths.resources.join("workers/speech_worker.py");
        let script = if script.exists() {
            script
        } else {
            self.paths.resources.join("speech_worker.py")
        };
        if !script.exists() {
            return Err(AppError::new(
                "AUDIO_WORKER_MISSING",
                "Worker de áudio não encontrado no pacote",
            ));
        }
        let mut child = Command::new(&self.paths.python)
            .arg("-u")
            .arg(script)
            .arg("--models")
            .arg(self.paths.models())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|_| {
                AppError::new(
                    "AUDIO_DEPENDENCIES",
                    "Python de áudio indisponível. Execute scripts/setup-audio.sh",
                )
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::new("AUDIO_PROTOCOL", "Entrada do worker indisponível"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::new("AUDIO_PROTOCOL", "Saída do worker indisponível"))?;
        *self
            .child
            .lock()
            .map_err(|_| AppError::new("AUDIO_PROTOCOL", "Worker indisponível"))? = Some(child);
        Ok(WorkerIo {
            stdin,
            stdout: BufReader::new(stdout),
        })
    }
    pub fn request(&self, command: Value, mut progress: impl FnMut(Value)) -> Result<Value> {
        let mut io = self
            .io
            .lock()
            .map_err(|_| AppError::new("AUDIO_PROTOCOL", "Worker indisponível"))?;
        let dead = self
            .child
            .lock()
            .map_err(|_| AppError::new("AUDIO_PROTOCOL", "Worker indisponível"))?
            .as_mut()
            .is_none_or(|c| c.try_wait().ok().flatten().is_some());
        if dead {
            *io = None;
        }
        if io.is_none() {
            *io = Some(self.start()?);
        }
        let operation = (|| {
            let worker = io.as_mut().expect("worker inicializado");
            serde_json::to_writer(&mut worker.stdin, &command)?;
            worker.stdin.write_all(b"\n")?;
            worker.stdin.flush()?;
            loop {
                let mut line = String::new();
                let bytes = (&mut worker.stdout).take(12_000_001).read_line(&mut line)?;
                if bytes == 0 || bytes > 12_000_000 || !line.ends_with('\n') {
                    return Err(AppError::new(
                        "AUDIO_PROTOCOL",
                        "Worker de áudio encerrou a comunicação",
                    ));
                }
                let value: Value = serde_json::from_str(&line)?;
                if let Some(event) = value.get("progress") {
                    progress(event.clone());
                    continue;
                }
                if let Some(error) = value.get("error") {
                    return Err(AppError::new(
                        error["code"].as_str().unwrap_or("AUDIO_ERROR"),
                        error["message"].as_str().unwrap_or("Falha no áudio"),
                    ));
                }
                return value
                    .get("result")
                    .cloned()
                    .ok_or_else(|| AppError::new("AUDIO_PROTOCOL", "Resposta de áudio inválida"));
            }
        })();
        if operation
            .as_ref()
            .is_err_and(|e| e.code == "AUDIO_PROTOCOL")
        {
            *io = None;
            self.cancel();
        }
        operation
    }
    pub fn cancel(&self) {
        if let Ok(mut child) = self.child.lock() {
            if let Some(mut child) = child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    pub fn status(&self) -> Result<Value> {
        self.request(json!({"command":"status"}), |_| {})
    }
    pub fn vad_reset(&self) -> Result<()> {
        self.request(json!({"command":"vad_reset"}), |_| {})?;
        Ok(())
    }
    pub fn vad(&self, samples: &[f32]) -> Result<f32> {
        let value = self.request(
            json!({"command":"vad","pcm_f32le":encode_f32(samples)}),
            |_| {},
        )?;
        Ok(value["probability"].as_f64().unwrap_or(0.) as f32)
    }
    pub fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let value = self.request(
            json!({"command":"transcribe","pcm_f32le":encode_f32(samples)}),
            |_| {},
        )?;
        Ok(value["text"].as_str().unwrap_or("").to_string())
    }
    pub fn synthesize(&self, text: &str, voice: &str, speed: f32) -> Result<Pcm> {
        let value = self.request(
            json!({"command":"synthesize","text":text,"voice":voice,"speed":speed}),
            |_| {},
        )?;
        let bytes = STANDARD
            .decode(
                value["pcm_s16le"]
                    .as_str()
                    .ok_or_else(|| AppError::new("AUDIO_PROTOCOL", "PCM ausente"))?,
            )
            .map_err(|_| AppError::new("AUDIO_PROTOCOL", "PCM inválido"))?;
        let rate = value["sample_rate"]
            .as_u64()
            .filter(|n| (8000..=96000).contains(n))
            .ok_or_else(|| AppError::new("AUDIO_PROTOCOL", "Frequência inválida"))?
            as u32;
        if bytes.len() % 2 != 0 {
            return Err(AppError::new("AUDIO_PROTOCOL", "PCM inválido"));
        }
        Ok(Pcm {
            sample_rate: rate,
            samples: bytes
                .chunks_exact(2)
                .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.)
                .collect(),
        })
    }
}
impl Drop for SpeechWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}
fn encode_f32(samples: &[f32]) -> String {
    STANDARD.encode(
        samples
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )
}

#[derive(Default)]
pub struct Sentences {
    buffer: String,
}
impl Sentences {
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.buffer.push_str(text);
        let mut out = Vec::new();
        loop {
            let end = self
                .buffer
                .char_indices()
                .find(|(i, c)| *i > 45 && ['.', '!', '?', '\n'].contains(c))
                .map(|(i, c)| i + c.len_utf8())
                .or_else(|| {
                    if self.buffer.chars().count() > 280 {
                        self.buffer
                            .char_indices()
                            .take(280)
                            .filter(|(_, c)| c.is_whitespace())
                            .map(|(i, c)| i + c.len_utf8())
                            .last()
                    } else {
                        None
                    }
                });
            let Some(end) = end else {
                break;
            };
            let sentence = self.buffer.drain(..end).collect::<String>();
            if !sentence.trim().is_empty() {
                out.push(sentence);
            }
        }
        out
    }
    pub fn finish(&mut self) -> Option<String> {
        let text = std::mem::take(&mut self.buffer);
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sentence_queue_preserves_text_and_utf8() {
        let mut s = Sentences::default();
        let input = "Olá! Vou buscar a conexão anterior para entender o que já tentamos. Depois vamos testar a solução.";
        let mut output = Vec::new();
        for c in input.chars() {
            output.extend(s.push(&c.to_string()));
        }
        output.extend(s.finish());
        assert_eq!(output.concat(), input);
    }
}
