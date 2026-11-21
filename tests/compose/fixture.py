"""Independent HTTP origin. Its address exists only inside the test network namespace."""

import fcntl
import hashlib
import json
import re
import socket
import struct
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ADDRESS = "93.184.216.34"
with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
    packed = struct.pack(
        "16sH2x4s8x", b"lo:fixture", socket.AF_INET, socket.inet_aton(ADDRESS)
    )
    fcntl.ioctl(sock, 0x8916, packed)  # SIOCSIFADDR
    mask = packed[:20] + b"\xff" * 4 + packed[24:]
    fcntl.ioctl(sock, 0x891C, mask)  # SIOCSIFNETMASK

config_lock = threading.Lock()


class Origin(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        root = Path("/fixture")
        filename = {
            "/history.json": "history.json",
            "/source.json": "source.json",
            "/first.json": "first.json",
            "/second.json": "second.json",
            "/index.json": "index.json",
            "/records.json": "records.json",
            "/package.json": "package.json",
            "/resources.json": "resources.json",
            "/controls.json": "controls.json",
            "/mutable.bin": "mutable.bin",
            "/cancel.bin": "cancel.bin",
            "/stall.bin": "stall.bin",
            "/alpha.bin": "alpha.bin",
            "/beta.bin": "beta.bin",
            "/bundle.bin": "bundle.bin",
            "/gamma.bin": "gamma.bin",
        }.get(self.path)
        if history := re.fullmatch(r"/history/(\d{4})\.(json|bin)", self.path):
            filename = "history-" + history[1] + "." + history[2]
            if history[2] == "json":
                with config_lock, (root / "history-reads").open("a") as reads:
                    reads.write(history[1] + "\n")
                if int(history[1]) >= 340 and (root / "history-delay").exists():
                    time.sleep(0.12)
        if self.path.startswith("/pool/") and (root / "payload.json").exists():
            digest = hashlib.sha1((root / "payload.json").read_bytes()).hexdigest()
            if self.path == f"/pool/{digest[:2]}/{digest}":
                filename = "payload.json"
        if not filename or not (root / filename).exists():
            self.send_error(404)
            return
        with config_lock:
            config = json.loads((root / "behavior.json").read_text())
            behavior = config.pop(self.path, None)
            if behavior:
                (root / "behavior.json").write_text(json.dumps(config))
        if isinstance(behavior, int):
            self.send_response(behavior)
            self.send_header("Retry-After", "1")
            self.end_headers()
            return
        body = (root / filename).read_bytes()
        tag = '"' + hashlib.sha256(body).hexdigest() + '"'
        if self.headers.get("If-None-Match") == tag:
            self.send_response(304)
            self.end_headers()
            return
        offset = 0
        byte_range = self.headers.get("Range")
        if byte_range and self.headers.get("If-Range", tag) == tag:
            offset = int(byte_range.removeprefix("bytes=").split("-")[0])
        if filename == "mutable.bin" and byte_range:
            (root / "mutable-range.json").write_text(
                json.dumps(
                    {
                        "range": byte_range,
                        "if_range": self.headers.get("If-Range"),
                        "current": tag,
                        "status": 206 if offset else 200,
                    }
                )
            )
        self.send_response(206 if offset else 200)
        if offset:
            self.send_header(
                "Content-Range", f"bytes {offset}-{len(body) - 1}/{len(body)}"
            )
            (root / "resumed").touch()
        self.send_header("ETag", tag)
        self.send_header("Content-Length", str(len(body) - offset))
        self.end_headers()
        try:
            for start in range(offset, len(body), 32768):
                self.wfile.write(body[start : start + 32768])
                if behavior == "stall":
                    # Deliver a real prefix and keep the socket open beyond the read deadline.
                    (root / "stall-started").touch()
                    time.sleep(90)
                    return
                if filename in {"alpha.bin", "mutable.bin", "cancel.bin"}:
                    time.sleep(0.025)
        except (BrokenPipeError, ConnectionResetError):
            pass


ThreadingHTTPServer((ADDRESS, 8081), Origin).serve_forever()
