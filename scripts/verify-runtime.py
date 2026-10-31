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
]

with tempfile.TemporaryDirectory(prefix="aura-smoke-") as data:
    environment = dict(os.environ, AURA_DATA_DIR=data)
    payload = "".join(json.dumps({"id": index, "command": name, "args": args}) + "\n" for index, (name, args) in enumerate(REQUESTS))
    process = subprocess.run([str(BINARY)], input=payload, text=True, capture_output=True, env=environment, timeout=25)
    assert process.returncode == 0, process.stderr
    responses = {row["id"]: row for line in process.stdout.splitlines() if "id" in (row := json.loads(line))}
    assert responses[0]["result"]["service_id"] == "google_calendar"
    assert responses[1]["result"][0]["id"] == "google_calendar"
    assert responses[2]["result"]["id"] == 1
    assert len(responses[3]["result"]) == 1
    assert responses[4]["result"][0]["url"].startswith("https://developers.google.com/workspace/calendar")
    assert responses[5]["result"]["google_calendar"]["connection"] is None
    assert len(responses[6]["result"]["artifacts"]) == 4
    assert responses[7]["error"]["code"] == "AUTH_REQUIRED"
    assert responses[8]["error"]["code"] == "COMMAND_NOT_ALLOWED"
    second = subprocess.run([str(BINARY)], input='{"id":0,"command":"session_list","args":{}}\n', text=True, capture_output=True, env=environment, timeout=10)
    assert json.loads(second.stdout)["result"][0]["title"] == "Smoke Calendar"
print("PASS: 9 comandos reais, worker Python e persistência após reiniciar o backend")
