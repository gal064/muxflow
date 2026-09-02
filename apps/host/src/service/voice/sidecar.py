"""Muxflow voice sidecar (docs/mobile/voice-mode-plan.md §4.2).

Spawned by the host with `uv run`; speaks one JSON header line per message,
optionally followed by exactly `body_bytes` raw bytes, in both directions on
stdin/stdout. Exits on stdin EOF, so a daemon that dies takes the sidecar with
it; on Linux the parent-death signal covers the case where the pipe outlives
the daemon.
"""

import asyncio
import ctypes
import json
import os
import queue
import shutil
import signal
import sys
import tarfile
import threading
import time
import urllib.error
import urllib.request

PROGRESS_INTERVAL = 0.25
MODEL_FILES = ("encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt")


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
        self.recognizer = None
        self.key = None

    def load(self, request):
        import sherpa_onnx

        key = (request["encoder"], request["decoder"], request["joiner"], request["tokens"])
        for path in key:
            if not os.path.isfile(path) or os.path.getsize(path) == 0:
                raise Refusal("model", f"model file missing or empty: {os.path.basename(path)}")
        if self.recognizer is not None and self.key == key:
            return 0
        started = time.monotonic()
        try:
            self.recognizer = sherpa_onnx.OfflineRecognizer.from_transducer(
                encoder=key[0],
                decoder=key[1],
                joiner=key[2],
                tokens=key[3],
                num_threads=min(4, os.cpu_count() or 1),
                model_type="nemo_transducer",
            )
        except Exception as error:  # noqa: BLE001 - reported to the host
            self.recognizer = None
            raise Refusal("model", f"model failed to load: {error}") from error
        self.key = key
        return int((time.monotonic() - started) * 1000)

    def transcribe(self, sample_rate, body):
        if self.recognizer is None:
            raise Refusal("model", "model is not loaded")
        if len(body) % 4 or not body:
            raise Refusal("input", "pcm body must be non-empty f32 little-endian")
        import array

        samples = array.array("f")
        samples.frombytes(body)
        if sys.byteorder != "little":
            samples.byteswap()
        started = time.monotonic()
        stream = self.recognizer.create_stream()
        stream.accept_waveform(sample_rate, samples.tolist())
        self.recognizer.decode_stream(stream)
        text = stream.result.text.strip()
        return text, int((time.monotonic() - started) * 1000)


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
        parent = os.path.dirname(model_dir)
        os.makedirs(parent, exist_ok=True)
        partial = os.path.join(parent, f".partial-{os.getpid()}.tar.bz2")
        extract = os.path.join(parent, f".extract-{os.getpid()}")
        try:
            self.download(url, partial, request_id)
            self.extract(partial, extract, model_dir, request_id)
        finally:
            if os.path.exists(partial):
                os.remove(partial)
            for directory in (extract, extract + ".model"):
                if os.path.isdir(directory):
                    shutil.rmtree(directory, ignore_errors=True)
        with open(os.path.join(model_dir, ".complete"), "w", encoding="utf-8") as marker:
            marker.write(url)

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

    def download(self, url, partial, request_id):
        try:
            response = urllib.request.urlopen(url, timeout=60)
        except (urllib.error.URLError, OSError) as error:
            raise Refusal("network", f"download failed: {error}") from error
        total = int(response.headers.get("Content-Length") or 0)
        transferred = 0
        last = 0.0
        self.progress(request_id, "downloading", 0, total)
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
                transferred += len(chunk)
                now = time.monotonic()
                if now - last >= PROGRESS_INTERVAL:
                    last = now
                    self.progress(request_id, "downloading", transferred, total)
        if total and transferred != total:
            raise Refusal("network", f"download incomplete: {transferred} of {total} bytes")
        self.progress(request_id, "downloading", transferred, total)

    def extract(self, partial, extract, model_dir, request_id):
        self.progress(request_id, "extracting", 0, 0)
        if os.path.isdir(extract):
            shutil.rmtree(extract, ignore_errors=True)
        os.makedirs(extract)
        try:
            with tarfile.open(partial, "r:bz2") as archive:
                archive.extractall(extract, filter="data")
        except (tarfile.TarError, OSError, ValueError) as error:
            raise Refusal("model", f"archive could not be extracted: {error}") from error
        if self.cancel.is_set():
            raise Refusal("cancelled", "provision cancelled")
        found = {}
        for root, _dirs, files in os.walk(extract):
            for name in files:
                for wanted in MODEL_FILES:
                    if name == wanted or (
                        wanted.endswith(".onnx") and name.startswith(wanted.split(".")[0]) and name.endswith("int8.onnx")
                    ):
                        found.setdefault(wanted, os.path.join(root, name))
        missing = [name for name in MODEL_FILES if name not in found]
        if missing:
            raise Refusal("model", f"archive lacks {', '.join(missing)}")
        staged = extract + ".model"
        if os.path.isdir(staged):
            shutil.rmtree(staged, ignore_errors=True)
        os.makedirs(staged)
        for wanted, path in found.items():
            shutil.move(path, os.path.join(staged, wanted))
        if os.path.isdir(model_dir):
            shutil.rmtree(model_dir, ignore_errors=True)
        os.rename(staged, model_dir)


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
