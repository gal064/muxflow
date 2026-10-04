"""Loopback-only SSH fixture with none or Ed25519 auth and real exec channels."""
import argparse
import base64
import hashlib
import json
import os
import socket
import struct
import subprocess
import threading
import time
from pathlib import Path

import paramiko
from cryptography.hazmat.primitives import serialization

parser = argparse.ArgumentParser()
parser.add_argument("--directory", required=True)
parser.add_argument("--host-binary")
args = parser.parse_args()
directory = Path(args.directory)
directory.mkdir(parents=True, exist_ok=True)
host_key = paramiko.RSAKey.generate(2048)
key_path = directory / "client.pem"
subprocess.run(["openssl", "genpkey", "-algorithm", "ED25519", "-out", str(key_path)], check=True, capture_output=True)
key_path.chmod(0o600)
private = serialization.load_pem_private_key(key_path.read_bytes(), password=None)
public = private.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
algorithm = b"ssh-ed25519"
wire_key = struct.pack(">I", len(algorithm)) + algorithm + struct.pack(">I", len(public)) + public
public_base64 = base64.b64encode(wire_key).decode()


class Server(paramiko.ServerInterface):
    def __init__(self, mode):
        self.mode = mode
        self.silent = False

    def get_allowed_auths(self, username):
        return "publickey"

    def check_auth_none(self, username):
        if self.mode == "delayed-none":
            time.sleep(12)
        return paramiko.AUTH_SUCCESSFUL if self.mode != "publickey" and username == "fixture" else paramiko.AUTH_FAILED

    def check_global_request(self, kind, message):
        if self.mode == "silent" and self.silent:
            # Keep TCP open but withhold SSH replies, modeling a blackholed link.
            time.sleep(120)
        return False

    def check_auth_publickey(self, username, key):
        authorized = directory / "authorized_keys"
        phone_keys = authorized.read_text().splitlines() if authorized.exists() else []
        accepted = key.get_base64() == public_base64 or any(len(line.split()) >= 2 and line.split()[1] == key.get_base64() for line in phone_keys)
        return paramiko.AUTH_SUCCESSFUL if username == "fixture" and accepted else paramiko.AUTH_FAILED

    def check_channel_request(self, kind, channel_id):
        return paramiko.OPEN_SUCCEEDED if kind == "session" else paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED

    def check_channel_exec_request(self, channel, command):
        if self.mode == "silent":
            self.silent = True
        def execute():
            if args.host_binary and command == b"$HOME/.local/bin/muxflow-host bridge --stdio":
                process = subprocess.Popen([args.host_binary, "bridge", "--stdio"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=os.environ.copy())
                def input_bytes():
                    try:
                        while data := channel.recv(65536):
                            process.stdin.write(data)
                            process.stdin.flush()
                    finally:
                        process.stdin.close()
                def output_bytes(stream, send):
                    try:
                        while data := stream.read1(65536):
                            send(data)
                    except (OSError, EOFError):
                        pass
                threading.Thread(target=input_bytes, daemon=True).start()
                output = threading.Thread(target=output_bytes, args=(process.stdout, channel.sendall), daemon=True)
                diagnostic = threading.Thread(target=output_bytes, args=(process.stderr, channel.sendall_stderr), daemon=True)
                output.start(); diagnostic.start()
                try:
                    status = process.wait(timeout=300)
                except subprocess.TimeoutExpired:
                    process.kill(); status = process.wait()
                output.join(timeout=5); diagnostic.join(timeout=5)
                if not channel.closed:
                    channel.send_exit_status(status)
            elif command in [b"rekey-same", b"rekey-changed"]:
                time.sleep(0.25)
                if command == b"rekey-changed":
                    channel.transport.add_server_key(paramiko.RSAKey.generate(2048))
                channel.transport.renegotiate_keys()
                while data := channel.recv(65536):
                    channel.sendall(data)
            elif command == b"eof-only":
                channel.shutdown_write()
                time.sleep(30)
            elif command == b"no-status":
                pass
            elif command == b"cat":
                while data := channel.recv(65536):
                    channel.sendall(data)
                channel.send_exit_status(0)
            else:
                process = subprocess.run(command.decode(), shell=True, capture_output=True)
                channel.sendall(process.stdout)
                channel.sendall_stderr(process.stderr)
                channel.send_exit_status(process.returncode)
            channel.close()
        threading.Thread(target=execute, daemon=True).start()
        return True


listeners = []
ports = {}
active_transports = set()
transport_lock = threading.Lock()
for mode in ["none", "publickey", "delayed-none", "silent", "delayed-open"]:
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen()
    ports[mode] = listener.getsockname()[1]
    listeners.append(listener)

    def serve(listener=listener, mode=mode):
        def session(client):
            class FixtureTransport(paramiko.Transport):
                confirmations = 0
                def _send_message(self, message):
                    if mode == "delayed-open" and bytes(message)[0] == 91:
                        self.confirmations += 1
                        if self.confirmations == 2:
                            # Withhold one open reply while still servicing the
                            # control channel and SSH keepalives on this session.
                            threading.Timer(20, lambda: super(FixtureTransport, self)._send_message(message)).start()
                            return
                    super()._send_message(message)
            transport = FixtureTransport(client)
            transport.add_server_key(host_key)
            with transport_lock:
                active_transports.add(transport)
            channels = []
            try:
                transport.start_server(server=Server(mode))
                while transport.is_active():
                    channel = transport.accept(1)
                    if channel is not None:
                        channels.append(channel)
                    channels = [item for item in channels if not item.closed]
            finally:
                transport.close()
                with transport_lock:
                    active_transports.discard(transport)
        while True:
            client, _ = listener.accept()
            threading.Thread(target=session, args=(client,), daemon=True).start()
    threading.Thread(target=serve, daemon=True).start()

# Reserve a port without listening, so connection-refusal tests avoid a race.
refused = socket.socket()
refused.bind(("127.0.0.1", 0))
ports["refused"] = refused.getsockname()[1]
info = dict(ports, fingerprint=hashlib.sha256(host_key.asbytes()).hexdigest(), key=str(key_path), publicKey="ssh-ed25519 " + public_base64 + " muxflow-mobile")
(directory / "fixture.json").write_text(json.dumps(info))
print("SSH fixture ready", flush=True)
while True:
    drop = directory / "drop-connections"
    if drop.exists():
        drop.unlink()
        with transport_lock:
            current = list(active_transports)
        for transport in current:
            transport.close()
        print("Forced SSH transport loss", flush=True)
    time.sleep(0.1)
