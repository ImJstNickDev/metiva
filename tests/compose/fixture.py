"""Independent HTTP origin. Its address exists only inside the test network namespace."""

import fcntl
import json
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
            "/source.json": "source.json",
            "/alpha.bin": "alpha.bin",
            "/beta.bin": "beta.bin",
            "/gamma.bin": "gamma.bin",
        }.get(self.path)
        if not filename or not (root / filename).exists():
            self.send_error(404)
            return
        with config_lock:
            config = json.loads((root / "behavior.json").read_text())
            behavior = config.pop(self.path, None)
            if behavior:
                (root / "behavior.json").write_text(json.dumps(config))
        if behavior:
            self.send_response(behavior)
            self.send_header("Retry-After", "1")
            self.end_headers()
            return
        body = (root / filename).read_bytes()
        import hashlib

        tag = '"' + hashlib.sha256(body).hexdigest() + '"'
        if self.headers.get("If-None-Match") == tag:
            self.send_response(304)
            self.end_headers()
            return
        offset = 0
        byte_range = self.headers.get("Range")
        if byte_range and self.headers.get("If-Range", tag) == tag:
            offset = int(byte_range.removeprefix("bytes=").split("-")[0])
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
                if filename == "alpha.bin":
                    time.sleep(0.025)
        except (BrokenPipeError, ConnectionResetError):
            pass


ThreadingHTTPServer((ADDRESS, 8081), Origin).serve_forever()
