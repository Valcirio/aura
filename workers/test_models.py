import hashlib
import json
import io
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from models import verify_file, artifact_path, install
from speech_worker import Speech


class ModelTests(unittest.TestCase):
    def test_hash_and_length_must_both_match(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "model"
            path.write_bytes(b"weights")
            digest = hashlib.sha256(b"weights").hexdigest()
            self.assertEqual(verify_file(path, digest, "sha256", 7), digest)
            with self.assertRaises(ValueError):
                verify_file(path, digest, "sha256", 6)
            with self.assertRaises(ValueError):
                verify_file(path, "0" * 64, "sha256", 7)

    def test_git_blob_verification_for_small_config(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "config"
            path.write_bytes(b"{}")
            expected = hashlib.sha1(b"blob 2\0{}").hexdigest()
            verify_file(path, expected, "sha1", 2)

    def test_installed_pointer_cannot_escape_model_directory(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root) / "whisper-base"
            directory.mkdir()
            (directory / "current.json").write_text(json.dumps({"revision": "../../other", "files": []}))
            with self.assertRaises(ValueError):
                artifact_path(root, "whisper-base", ".bin")

    def test_unknown_artifact_never_downloads(self):
        with patch("models.request") as request:
            with self.assertRaises(ValueError):
                install("/tmp", "https://untrusted.test/anything")
            request.assert_not_called()

    def test_status_does_not_require_models_or_heavy_imports(self):
        with tempfile.TemporaryDirectory() as root:
            status = Speech(root).dispatch({"command": "status"})
            self.assertEqual(len(status["artifacts"]), 4)
            self.assertTrue(all(not artifact["installed"] for artifact in status["artifacts"]))

    def test_worker_does_not_accept_arbitrary_commands(self):
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaises(ValueError):
                Speech(root).dispatch({"command": "shell", "text": "anything"})

    def test_failed_download_does_not_replace_working_revision(self):
        with tempfile.TemporaryDirectory() as root:
            directory = Path(root) / "whisper-base"
            directory.mkdir()
            pointer = directory / "current.json"
            pointer.write_text('{"revision":"old"}')
            entry = {"path": "ggml-base.bin", "size": 7, "lfs": {"oid": "0" * 64}}
            with patch("models.metadata", side_effect=[{"sha": "b" * 40}, [entry]]), patch("models.request", return_value=io.BytesIO(b"corrupt")):
                with self.assertRaises(ValueError):
                    install(root, "whisper-base")
            self.assertEqual(json.loads(pointer.read_text())["revision"], "old")
            self.assertFalse(list(directory.glob(".partial-*")))

    def test_protocol_is_json_only_and_reuses_one_worker(self):
        with tempfile.TemporaryDirectory() as root:
            process = subprocess.run([sys.executable, str(Path(__file__).parent / "speech_worker.py"), "--models", root],
                                     input='{"command":"status"}\n{"command":"vad_reset"}\n', text=True, capture_output=True, timeout=10)
            self.assertEqual(process.returncode, 0)
            responses = [json.loads(line) for line in process.stdout.splitlines()]
            self.assertEqual(len(responses), 2)
            self.assertTrue(responses[1]["result"]["ready"])


if __name__ == "__main__":
    unittest.main()
