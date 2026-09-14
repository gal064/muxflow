"""Muxflow voice sidecar (docs/mobile/voice-mode-plan.md §4.2).

Spawned by the host with `uv run`; speaks one JSON header line per message,
optionally followed by exactly `body_bytes` raw bytes, in both directions on
stdin/stdout. Exits on stdin EOF, so a daemon that dies takes the sidecar with
it; on Linux the parent-death signal covers the case where the pipe outlives
the daemon.
"""

import asyncio
import ctypes
import fcntl
import hashlib
import json
import os
import queue
import shutil
import signal
import sys
import threading
import time
import urllib.error
import urllib.request

PROGRESS_INTERVAL = 0.25


class Refusal(Exception):
    def __init__(self, cls, message):
        super().__init__(message)
        self.cls = cls


def set_parent_death_signal():
    if sys.platform != "linux":
        return
    try:
        libc = ctypes.CDLL("libc.so.6", use_errno=True)
        libc.prctl(1, signal.SIGTERM)  # PR_SET_PDEATHSIG
    except OSError:
        pass


def write_frame(header, body=b""):
    if body:
        header["body_bytes"] = len(body)
    out = sys.stdout.buffer
    out.write(json.dumps(header, separators=(",", ":")).encode("utf-8"))
    out.write(b"\n")
    if body:
        out.write(body)
    out.flush()


def read_frame(stream):
    line = stream.readline()
    if not line:
        return None, None
    header = json.loads(line.decode("utf-8"))
    body = b""
    size = int(header.get("body_bytes", 0))
    if size:
        body = stream.read(size)
        if len(body) != size:
            return None, None
    return header, body


class Recognizer:
    def __init__(self):
        self.model = None
        self.session = None
        self.key = None

    def load(self, request):
        import transcribe_cpp

        key = request["model"]
        if not os.path.isfile(key) or os.path.getsize(key) == 0:
            raise Refusal("model", f"model file missing or empty: {os.path.basename(key)}")
        if self.session is not None and self.key == key:
            return 0
        self.close()
        started = time.monotonic()
        model = None
        session = None
        try:
            model = transcribe_cpp.Model(key, backend="auto")
            capabilities = model.capabilities
            if capabilities.native_sample_rate != 16_000 or "en" not in capabilities.languages:
                raise Refusal("model", "model is not the expected 16 kHz English recognizer")
            session = model.session(n_threads=min(4, os.cpu_count() or 1))
        except Exception as error:  # noqa: BLE001 - reported to the host
            if session is not None:
                session.close()
            if model is not None:
                model.close()
            if isinstance(error, Refusal):
                raise
            if isinstance(
                error,
                (transcribe_cpp.ModelFileNotFound, transcribe_cpp.ModelLoadError),
            ):
                raise Refusal("model", f"model failed to load: {error}") from error
            raise Refusal("internal", f"speech runtime failed to load: {error}") from error
        self.model = model
        self.session = session
        self.key = key
        return int((time.monotonic() - started) * 1000)

    def transcribe(self, sample_rate, body):
        if self.session is None:
            raise Refusal("model", "model is not loaded")
        if sample_rate != 16_000:
            raise Refusal("input", f"pcm sample rate must be 16000 Hz, got {sample_rate}")
        if len(body) % 4 or not body:
            raise Refusal("input", "pcm body must be non-empty f32 little-endian")
        import array

        samples = array.array("f")
        samples.frombytes(body)
        if sys.byteorder != "little":
            samples.byteswap()
        started = time.monotonic()
        try:
            text = self.session.run(samples, language="en", timestamps="none").text.strip()
        except Exception as error:  # noqa: BLE001 - reported to the host
            raise Refusal("internal", f"transcription failed: {error}") from error
        return text, int((time.monotonic() - started) * 1000)

    def close(self):
        if self.session is not None:
            self.session.close()
        if self.model is not None:
            self.model.close()
        self.session = None
        self.model = None
        self.key = None


def speak(text, voice):
    import edge_tts

    async def collect():
        chunks = []
        communicate = edge_tts.Communicate(text, voice)
        async for chunk in communicate.stream():
            if chunk["type"] == "audio":
                chunks.append(chunk["data"])
        return b"".join(chunks)

    try:
        audio = asyncio.run(collect())
    except Exception as error:  # noqa: BLE001 - classified below
        name = type(error).__name__
        if "NoAudioReceived" in name or "UnknownResponse" in name:
            raise Refusal("network", f"edge-tts returned no audio: {error}") from error
        if "Voice" in name or isinstance(error, ValueError):
            raise Refusal("input", f"edge-tts rejected the request: {error}") from error
        raise Refusal("network", f"edge-tts failed: {error}") from error
    if not audio:
        raise Refusal("network", "edge-tts returned no audio")
    return audio


class Provisioner:
    def __init__(self):
        self.cancel = threading.Event()

    def run(self, request, request_id):
        self.cancel.clear()
        model_dir = request["model_dir"]
        url = request["url"]
        filename = request["filename"]
        expected_size = int(request["size"])
        expected_sha256 = request["sha256"]
        marker_contents = request["marker"]
        parent = os.path.dirname(model_dir)
        os.makedirs(parent, exist_ok=True)
        partial = os.path.join(parent, f".partial-{os.getpid()}.gguf")
        marker_tmp = os.path.join(model_dir, f".complete.{os.getpid()}")
        lock = self.acquire_lock(parent)
        try:
            self.sweep_partials(parent)
            self.download(url, partial, request_id, expected_size, expected_sha256)
            os.makedirs(model_dir, exist_ok=True)
            final_path = os.path.join(model_dir, filename)
            os.replace(partial, final_path)

            with open(marker_tmp, "w", encoding="utf-8") as marker:
                marker.write(marker_contents)
                marker.flush()
                os.fsync(marker.fileno())
            os.replace(marker_tmp, os.path.join(model_dir, ".complete"))
            fsync_dir(model_dir)
            fsync_dir(parent)
        finally:
            if os.path.exists(partial):
                os.remove(partial)
            if os.path.exists(marker_tmp):
                os.remove(marker_tmp)
            fcntl.flock(lock.fileno(), fcntl.LOCK_UN)
            lock.close()

    def acquire_lock(self, parent):
        lock = open(os.path.join(parent, ".provision.lock"), "a+b")
        try:
            fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            lock.close()
            raise Refusal("busy", "another host process is provisioning voice") from error
        return lock

    def sweep_partials(self, parent):
        for name in os.listdir(parent):
            if not name.startswith((".partial-", ".extract-")):
                continue
            path = os.path.join(parent, name)
            if os.path.isfile(path) or os.path.islink(path):
                os.remove(path)
            elif os.path.isdir(path):
                shutil.rmtree(path)

    def progress(self, request_id, phase, transferred, total):
        write_frame(
            {
                "id": request_id,
                "event": "progress",
                "phase": phase,
                "transferred": transferred,
                "total": total,
            }
        )

    def download(self, url, partial, request_id, expected_size, expected_sha256):
        try:
            response = urllib.request.urlopen(url, timeout=60)
        except (urllib.error.URLError, OSError) as error:
            raise Refusal("network", f"download failed: {error}") from error
        announced_size = int(response.headers.get("Content-Length") or 0)
        if announced_size and announced_size != expected_size:
            response.close()
            raise Refusal(
                "model",
                f"download size changed: expected {expected_size}, server announced {announced_size}",
            )
        transferred = 0
        digest = hashlib.sha256()
        last = 0.0
        self.progress(request_id, "downloading", 0, expected_size)
        with response, open(partial, "wb") as out:
            while True:
                if self.cancel.is_set():
                    raise Refusal("cancelled", "provision cancelled")
                try:
                    chunk = response.read(1024 * 1024)
                except (urllib.error.URLError, OSError) as error:
                    raise Refusal("network", f"download interrupted: {error}") from error
                if not chunk:
                    break
                out.write(chunk)
                digest.update(chunk)
                transferred += len(chunk)
                now = time.monotonic()
                if now - last >= PROGRESS_INTERVAL:
                    last = now
                    self.progress(request_id, "downloading", transferred, expected_size)
            out.flush()
            os.fsync(out.fileno())

        self.progress(request_id, "downloading", transferred, expected_size)
        self.progress(request_id, "verifying", transferred, expected_size)
        if transferred != expected_size:
            raise Refusal(
                "model", f"download incomplete: {transferred} of {expected_size} bytes"
            )
        actual_sha256 = digest.hexdigest()
        if actual_sha256 != expected_sha256:
            raise Refusal(
                "model",
                f"download checksum mismatch: expected {expected_sha256}, got {actual_sha256}",
            )


def fsync_dir(path):
    if not hasattr(os, "O_DIRECTORY"):
        return
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def read_requests(stdin, requests, provisioner):
    """Reader thread: frames are parsed as they arrive, so a `cancel` written
    while a download is running is acted on now, not after it finishes."""
    while True:
        header, body = read_frame(stdin)
        if header is None:
            requests.put(None)
            return
        if header.get("op") == "cancel":
            provisioner.cancel.set()
            continue
        requests.put((header, body))


def serve():
    set_parent_death_signal()
    recognizer = Recognizer()
    provisioner = Provisioner()
    requests = queue.Queue()
    threading.Thread(
        target=read_requests, args=(sys.stdin.buffer, requests, provisioner), daemon=True
    ).start()
    while True:
        item = requests.get()
        if item is None:
            return
        header, body = item
        op = header.get("op")
        request_id = header.get("id")
        try:
            if op == "load":
                millis = recognizer.load(header)
                write_frame({"id": request_id, "ok": True, "load_millis": millis})
            elif op == "transcribe":
                text, millis = recognizer.transcribe(int(header["sample_rate"]), body)
                write_frame({"id": request_id, "ok": True, "text": text, "decode_millis": millis})
            elif op == "speak":
                audio = speak(header["text"], header["voice"])
                write_frame({"id": request_id, "ok": True, "mime": "audio/mpeg"}, audio)
            elif op == "provision":
                provisioner.run(header, request_id)
                write_frame({"id": request_id, "ok": True})
            elif op == "ping":
                write_frame({"id": request_id, "ok": True})
            else:
                raise Refusal("input", f"unknown op {op!r}")
        except Refusal as refusal:
            write_frame({"id": request_id, "ok": False, "class": refusal.cls, "error": str(refusal)})
        except Exception as error:  # noqa: BLE001 - the host restarts us
            write_frame({"id": request_id, "ok": False, "class": "internal", "error": f"{type(error).__name__}: {error}"})


if __name__ == "__main__":
    serve()
