"""Worker CPU persistente. stdin/stdout NDJSON; logs nativos somente em stderr."""
import argparse
import base64
import importlib.util
import json
import math
import os
from pathlib import Path
import sys

# O pacote de distribuição pode trazer os wheels ao lado do worker.
sys.path.insert(0, str(Path(__file__).resolve().parent / "python-packages"))
from models import CATALOG, artifact_path, install


class Speech:
    def __init__(self, root):
        self.root = root
        self.whisper = None
        self.voice = None
        self.voice_id = None
        self.vad = None
        self.vad_state = None
        self.vad_context = None

    def dispatch(self, command, progress=lambda **event: None):
        kind = command.get("command")
        if kind == "status":
            return {"packages": {name: importlib.util.find_spec(name) is not None for name in ["numpy", "onnxruntime", "piper", "pywhispercpp"]},
                    "artifacts": [{"id": key, "installed": (Path(self.root) / key / "current.json").is_file(), "license": info[2]} for key, info in CATALOG.items()]}
        if kind == "install":
            return install(self.root, command["artifact"], progress)
        if kind == "vad_reset":
            self.vad_state = None
            self.vad_context = None
            return {"ready": True}
        if kind == "vad":
            import numpy as np
            import onnxruntime as ort
            audio = pcm(command, 512)
            if len(audio) != 512:
                raise ValueError("VAD requer exatamente 512 amostras a 16 kHz")
            if self.vad is None:
                options = ort.SessionOptions()
                options.intra_op_num_threads = 1
                options.inter_op_num_threads = 1
                self.vad = ort.InferenceSession(str(artifact_path(self.root, "silero-vad", ".onnx")), sess_options=options, providers=["CPUExecutionProvider"])
            if self.vad_state is None:
                self.vad_state = np.zeros((2, 1, 128), dtype=np.float32)
                self.vad_context = np.zeros((1, 64), dtype=np.float32)
            joined = np.concatenate((self.vad_context, audio.reshape(1, -1)), axis=1)
            output, self.vad_state = self.vad.run(None, {"input": joined, "state": self.vad_state, "sr": np.array(16000, dtype=np.int64)})
            self.vad_context = joined[:, -64:]
            return {"probability": float(output.reshape(-1)[0])}
        if kind == "transcribe":
            from pywhispercpp.model import Model
            audio = pcm(command, 720000)
            if self.whisper is None:
                self.whisper = Model(str(artifact_path(self.root, "whisper-base", ".bin")), n_threads=min(6, os.cpu_count() or 2), print_realtime=False, print_progress=False, print_timestamps=False)
            segments = self.whisper.transcribe(audio, language="pt", no_context=True, suppress_blank=True)
            return {"text": " ".join(segment.text.strip() for segment in segments).strip()}
        if kind == "synthesize":
            from piper import PiperVoice, SynthesisConfig
            text = command.get("text", "")
            speed = command.get("speed", 1.0)
            voice = command.get("voice", "pt_BR-cadu-medium")
            if not text.strip() or len(text) > 2000 or not isinstance(speed, (float, int)) or not math.isfinite(speed) or not 0.85 <= speed <= 1.15:
                raise ValueError("Texto ou velocidade de síntese inválidos")
            if voice not in {"pt_BR-cadu-medium", "pt_BR-faber-medium"}:
                raise ValueError("Voz desconhecida")
            if self.voice_id != voice:
                self.voice = PiperVoice.load(str(artifact_path(self.root, voice, ".onnx")), use_cuda=False)
                self.voice_id = voice
            raw = bytearray()
            rate = None
            for chunk in self.voice.synthesize(text, syn_config=SynthesisConfig(length_scale=1 / speed)):
                if chunk.sample_channels != 1 or chunk.sample_width != 2:
                    raise ValueError("Piper retornou formato inesperado")
                if rate is not None and rate != chunk.sample_rate:
                    raise ValueError("Frequência mudou durante a síntese")
                rate = chunk.sample_rate
                raw.extend(chunk.audio_int16_bytes)
                if len(raw) > 8_000_000:
                    raise ValueError("Áudio sintetizado muito longo")
            return {"sample_rate": rate, "pcm_s16le": base64.b64encode(raw).decode("ascii")}
        raise ValueError("Comando de áudio desconhecido")


def pcm(command, limit):
    import numpy as np
    raw = base64.b64decode(command["pcm_f32le"], validate=True)
    if not raw or len(raw) % 4 or len(raw) > limit * 4:
        raise ValueError("PCM inválido ou muito longo")
    audio = np.frombuffer(raw, dtype="<f4")
    if not np.isfinite(audio).all():
        raise ValueError("PCM contém valores inválidos")
    return audio


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--models", required=True)
    args = parser.parse_args()
    protocol = os.fdopen(os.dup(1), "w", buffering=1, encoding="utf-8")
    os.dup2(2, 1)  # bibliotecas C também não podem escrever no canal NDJSON
    speech = Speech(args.models)
    def send(value):
        protocol.write(json.dumps(value, ensure_ascii=False, allow_nan=False) + "\n")
        protocol.flush()
    while line := sys.stdin.buffer.readline(5_000_001):
        if len(line) > 5_000_000 or not line.endswith(b"\n"):
            send({"error": {"code": "AUDIO_PROTOCOL", "message": "Requisição de áudio muito grande"}})
            break
        try:
            command = json.loads(line)
            result = speech.dispatch(command, lambda **event: send({"progress": event}))
            send({"result": result})
        except ModuleNotFoundError:
            send({"error": {"code": "AUDIO_DEPENDENCIES", "message": "Instale as dependências de áudio com scripts/setup-audio.sh"}})
        except FileNotFoundError as error:
            send({"error": {"code": "MODEL_NOT_INSTALLED", "message": str(error)}})
        except Exception as error:
            print(f"worker: {type(error).__name__}", file=sys.stderr)
            send({"error": {"code": "AUDIO_ERROR", "message": "Não foi possível processar áudio ou instalar o modelo. Verifique as dependências e a conexão."}})


if __name__ == "__main__":
    main()
