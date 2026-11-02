"""Smoke de backend real por NDJSON, sem OAuth, rede nem microfone físico."""
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
BINARY = ROOT / "crates/support-runtime/target/debug/aura-runtime"
REQUESTS = [
    ("settings_get", {}),
    ("service_list", {}),
    ("session_create", {"title": "Smoke Calendar"}),
    ("session_list", {}),
    ("knowledge_search", {"query": "compromisso"}),
    ("service_status", {}),
    ("model_status", {}),
    ("action_confirm", {"id": "inexistente", "session_id": 1}),
    ("arbitrary_shell", {}),
    ("model_status", {"extra": 1}),
    ("model_install", {}),
    ("model_install", {"id": "inexistente"}),
    ("voice_preview", {}),
    ("voice_preview", {"id": "inexistente"}),
]
IDS = ["whisper-turbo", "whisper-small", "silero-vad", "kokoro-multi-lang-v1_0"]
CONFIG = json.loads((ROOT / "src-tauri/tauri.conf.json").read_text())
resources = json.dumps(CONFIG["bundle"].get("resources", {})).lower()
assert "workers" not in resources and "python" not in resources

with tempfile.TemporaryDirectory(prefix="aura-smoke-") as data:
    environment = dict(os.environ, AURA_DATA_DIR=data)
    payload = "".join(json.dumps({"id": index, "command": name, "args": args}) + "\n" for index, (name, args) in enumerate(REQUESTS))
    process = subprocess.run([str(BINARY)], input=payload, text=True, capture_output=True, env=environment, timeout=25)
    assert process.returncode == 0, process.stderr
    responses = {row["id"]: row for line in process.stdout.splitlines() if "id" in (row := json.loads(line))}
    assert responses[0]["result"]["service_id"] == "google_calendar"
    assert responses[0]["result"]["voice"] == "pf_dora"
    assert responses[0]["result"]["asr_model"] == "turbo"
    assert responses[1]["result"][0]["id"] == "google_calendar"
    assert responses[2]["result"]["id"] == 1
    assert len(responses[3]["result"]) == 1
    assert responses[4]["result"][0]["url"].startswith("https://developers.google.com/workspace/calendar")
    assert responses[5]["result"]["google_calendar"]["connection"] is None
    status = responses[6]["result"]
    assert status["packages"] == {}
    artifacts = {item["id"]: item for item in status["artifacts"]}
    assert [item["id"] for item in status["artifacts"]] == IDS
    for item in artifacts.values():
        assert item["installed"] is False and item["license"]
        assert item["label"] and item["description"]
        assert item["kind"] in ("asr", "vad", "tts")
    assert artifacts["whisper-turbo"]["asr_model"] == "turbo" and artifacts["whisper-turbo"]["recommended"] is True
    assert artifacts["whisper-small"]["asr_model"] == "small" and artifacts["whisper-small"]["recommended"] is False
    assert artifacts["whisper-small"]["warning"] == "Modelo mais leve; pode reduzir a precisão da transcrição."
    assert artifacts["whisper-turbo"]["warning"] is None
    assert artifacts["silero-vad"]["kind"] == "vad"
    assert artifacts["kokoro-multi-lang-v1_0"]["kind"] == "tts"
    assert artifacts["kokoro-multi-lang-v1_0"]["voice"] == "pf_dora"
    assert all(artifacts[i]["voice"] is None for i in IDS[:3])
    assert responses[7]["error"]["code"] == "AUTH_REQUIRED"
    assert responses[8]["error"]["code"] == "COMMAND_NOT_ALLOWED"
    for index in (9, 10, 11, 12):
        assert responses[index]["error"]["code"] == "INVALID_INPUT", responses[index]
    assert responses[13]["error"]["code"] == "INVALID_INPUT", responses[13]
    second = subprocess.run([str(BINARY)], input='{"id":0,"command":"session_list","args":{}}\n', text=True, capture_output=True, env=environment, timeout=10)
    assert json.loads(second.stdout)["result"][0]["title"] == "Smoke Calendar"
print("PASS: 14 comandos reais, catálogo nativo, esquemas, sem workers no bundle e persistência após reiniciar o backend")
