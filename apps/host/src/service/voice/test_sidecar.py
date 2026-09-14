import array
import fcntl
import functools
import hashlib
import http.server
import importlib.util
import sys
import tempfile
import threading
import types
import unittest
from pathlib import Path


SIDECAR_PATH = Path(__file__).with_name("sidecar.py")
SPEC = importlib.util.spec_from_file_location("muxflow_voice_sidecar", SIDECAR_PATH)
sidecar = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sidecar)


class _QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, _format, *_args):
        pass


class _ProgressProvisioner(sidecar.Provisioner):
    def __init__(self):
        super().__init__()
        self.events = []

    def progress(self, _request_id, phase, transferred, total):
        self.events.append((phase, transferred, total))


class _CancellingProvisioner(_ProgressProvisioner):
    def progress(self, request_id, phase, transferred, total):
        super().progress(request_id, phase, transferred, total)
        if phase == "downloading" and transferred > 0:
            self.cancel.set()


class SidecarTests(unittest.TestCase):
    def test_recognizer_keeps_one_english_session_warm(self):
        calls = []

        class FakeSession:
            def run(self, samples, **options):
                calls.append((list(samples), options))
                return types.SimpleNamespace(text="  hello world  ")

            def close(self):
                pass

        class FakeModel:
            def __init__(self, path, backend):
                calls.append((path, backend))
                self.capabilities = types.SimpleNamespace(
                    native_sample_rate=16_000, languages=("en",)
                )

            def session(self, n_threads):
                calls.append(("threads", n_threads))
                return FakeSession()

            def close(self):
                pass

        previous = sys.modules.get("transcribe_cpp")
        sys.modules["transcribe_cpp"] = types.SimpleNamespace(Model=FakeModel)
        try:
            with tempfile.NamedTemporaryFile() as model:
                model.write(b"gguf")
                model.flush()
                recognizer = sidecar.Recognizer()
                self.assertGreaterEqual(recognizer.load({"model": model.name}), 0)
                self.assertEqual(recognizer.load({"model": model.name}), 0)
                pcm = array.array("f", [0.25, -0.25]).tobytes()
                text, _millis = recognizer.transcribe(16_000, pcm)
                self.assertEqual(text, "hello world")
                self.assertEqual(
                    calls[-1][1], {"language": "en", "timestamps": "none"}
                )
        finally:
            if previous is None:
                sys.modules.pop("transcribe_cpp", None)
            else:
                sys.modules["transcribe_cpp"] = previous

    def test_recognizer_keeps_verified_model_on_backend_failure(self):
        class FakeModelFileNotFound(Exception):
            pass

        class FakeModelLoadError(Exception):
            pass

        class FakeBackendError(Exception):
            pass

        class FailingModel:
            def __init__(self, _path, *, backend):
                self.backend = backend
                raise FakeBackendError("GPU unavailable")

        fake_module = types.SimpleNamespace(
            Model=FailingModel,
            ModelFileNotFound=FakeModelFileNotFound,
            ModelLoadError=FakeModelLoadError,
        )
        previous = sys.modules.get("transcribe_cpp")
        sys.modules["transcribe_cpp"] = fake_module
        try:
            with tempfile.NamedTemporaryFile() as model:
                model.write(b"gguf")
                model.flush()
                with self.assertRaises(sidecar.Refusal) as raised:
                    sidecar.Recognizer().load({"model": model.name})
                self.assertEqual(raised.exception.cls, "internal")
                self.assertIn("runtime failed to load", str(raised.exception))
        finally:
            if previous is None:
                sys.modules.pop("transcribe_cpp", None)
            else:
                sys.modules["transcribe_cpp"] = previous

    def test_provision_downloads_verifies_and_installs_one_gguf(self):
        payload = b"unified-model"
        with tempfile.TemporaryDirectory() as root:
            root_path = Path(root)
            served = root_path / "served"
            served.mkdir()
            (served / "model.gguf").write_bytes(payload)
            handler = functools.partial(_QuietHandler, directory=served)
            server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                model_dir = root_path / "models" / "unified"
                marker = "url\nrevision\n13\ndigest\n"
                provisioner = _ProgressProvisioner()
                provisioner.run(
                    {
                        "model_dir": str(model_dir),
                        "url": f"http://127.0.0.1:{server.server_port}/model.gguf",
                        "filename": "model.gguf",
                        "size": len(payload),
                        "sha256": hashlib.sha256(payload).hexdigest(),
                        "marker": marker,
                    },
                    7,
                )
                self.assertEqual((model_dir / "model.gguf").read_bytes(), payload)
                self.assertEqual((model_dir / ".complete").read_text(), marker)
                self.assertEqual(provisioner.events[-1][0], "verifying")
            finally:
                server.shutdown()
                thread.join()
                server.server_close()

    def test_provision_rejects_bad_digest_and_removes_partial(self):
        payload = b"wrong-model"
        with tempfile.TemporaryDirectory() as root:
            root_path = Path(root)
            served = root_path / "served"
            served.mkdir()
            (served / "model.gguf").write_bytes(payload)
            handler = functools.partial(_QuietHandler, directory=served)
            server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                provisioner = _ProgressProvisioner()
                with self.assertRaisesRegex(sidecar.Refusal, "checksum mismatch"):
                    provisioner.run(
                        {
                            "model_dir": str(root_path / "models" / "unified"),
                            "url": f"http://127.0.0.1:{server.server_port}/model.gguf",
                            "filename": "model.gguf",
                            "size": len(payload),
                            "sha256": "0" * 64,
                            "marker": "unused",
                        },
                        8,
                    )
                self.assertEqual(list((root_path / "models").glob(".partial-*")), [])
            finally:
                server.shutdown()
                thread.join()
                server.server_close()

    def test_provision_rejects_changed_content_length_before_writing(self):
        payload = b"short-model"
        with tempfile.TemporaryDirectory() as root:
            root_path = Path(root)
            served = root_path / "served"
            served.mkdir()
            (served / "model.gguf").write_bytes(payload)
            handler = functools.partial(_QuietHandler, directory=served)
            server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                provisioner = _ProgressProvisioner()
                with self.assertRaisesRegex(sidecar.Refusal, "download size changed"):
                    provisioner.run(
                        {
                            "model_dir": str(root_path / "models" / "unified"),
                            "url": f"http://127.0.0.1:{server.server_port}/model.gguf",
                            "filename": "model.gguf",
                            "size": len(payload) + 1,
                            "sha256": hashlib.sha256(payload).hexdigest(),
                            "marker": "unused",
                        },
                        9,
                    )
                self.assertEqual(list((root_path / "models").glob(".partial-*")), [])
            finally:
                server.shutdown()
                thread.join()
                server.server_close()

    def test_cancelled_provision_removes_partial_and_never_marks_complete(self):
        payload = b"x" * (2 * 1024 * 1024)
        with tempfile.TemporaryDirectory() as root:
            root_path = Path(root)
            served = root_path / "served"
            served.mkdir()
            (served / "model.gguf").write_bytes(payload)
            handler = functools.partial(_QuietHandler, directory=served)
            server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                model_dir = root_path / "models" / "unified"
                provisioner = _CancellingProvisioner()
                with self.assertRaisesRegex(sidecar.Refusal, "provision cancelled"):
                    provisioner.run(
                        {
                            "model_dir": str(model_dir),
                            "url": f"http://127.0.0.1:{server.server_port}/model.gguf",
                            "filename": "model.gguf",
                            "size": len(payload),
                            "sha256": hashlib.sha256(payload).hexdigest(),
                            "marker": "unused",
                        },
                        10,
                    )
                self.assertFalse((model_dir / ".complete").exists())
                self.assertEqual(list((root_path / "models").glob(".partial-*")), [])
            finally:
                server.shutdown()
                thread.join()
                server.server_close()

    def test_provision_lock_prevents_cross_process_partial_cleanup(self):
        with tempfile.TemporaryDirectory() as root:
            root_path = Path(root)
            models = root_path / "models"
            models.mkdir()
            active_partial = models / ".partial-other.gguf"
            active_partial.write_bytes(b"active")
            with (models / ".provision.lock").open("a+b") as lock:
                fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                provisioner = _ProgressProvisioner()
                with self.assertRaises(sidecar.Refusal) as raised:
                    provisioner.run(
                        {
                            "model_dir": str(models / "unified"),
                            "url": "http://127.0.0.1:1/not-used",
                            "filename": "model.gguf",
                            "size": 1,
                            "sha256": "0" * 64,
                            "marker": "unused",
                        },
                        11,
                    )
                self.assertEqual(raised.exception.cls, "busy")
                self.assertEqual(active_partial.read_bytes(), b"active")


if __name__ == "__main__":
    unittest.main()
