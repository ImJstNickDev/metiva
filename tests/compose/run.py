"""Build and exercise the Compose deployment with temporary data and an isolated TLS proxy."""

import json
import os
from pathlib import Path
import secrets
import subprocess
import tempfile
import sys

ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "tests/compose"


def run(args, **kw):
    return subprocess.run(args, check=True, cwd=ROOT, text=True, **kw)


def main():
    name = "metiva-check-" + secrets.token_hex(4)
    with tempfile.TemporaryDirectory(prefix=name) as tmp:
        tmp = Path(tmp)
        data = tmp / "data"
        data.mkdir()
        (data / "artifacts").mkdir()
        fixture = tmp / "fixture"
        fixture.mkdir()
        tls = tmp / "tls"
        tls.mkdir()
        env = {
            **os.environ,
            "METIVA_PUBLIC_URL": "https://mirror.test",
            "METIVA_DATA_DIR": str(data),
            "METIVA_UID": str(os.getuid()),
            "METIVA_GID": str(os.getgid()),
        }
        cfg = json.loads(
            run(
                ["docker", "compose", "config", "--format", "json"],
                env=env,
                capture_output=True,
            ).stdout
        )
        assert cfg["networks"]["prod-frontend"]["external"]
        assert all(not item.get("ports") for item in cfg["services"].values())
        assert "prod-frontend" not in cfg["services"]["metiva-api"]["networks"]
        assert "prod-frontend" not in cfg["services"]["metiva-worker"]["networks"]
        cfg["name"] = name
        test_subnet = os.environ.get(
            "METIVA_TEST_SUBNET", f"10.244.{secrets.randbelow(256)}.0/28"
        )
        cfg["networks"] = {
            "backend": {"internal": True, "ipam": {"config": [{"subnet": test_subnet}]}}
        }
        services = cfg["services"]
        services["metiva"].pop("container_name")
        services["metiva"]["networks"] = ["backend"]
        # The fixture, API, and worker share a test-only namespace so SSRF checks
        # exercise a public IP without creating a route on the host.
        for key in ["metiva-api", "metiva-worker"]:
            services[key].pop("networks")
            services[key]["network_mode"] = "service:fixture"
            services[key].setdefault("depends_on", {})["fixture"] = {
                "condition": "service_healthy"
            }
        services["metiva-worker"]["environment"]["METIVA_IDLE_TIMEOUT_SECS"] = "3"
        for service in services.values():
            service["restart"] = "no"
            service["healthcheck"].update(interval="2s", start_period="4s", retries=15)
        run(
            [
                "openssl",
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=mirror.test",
                "-addext",
                "subjectAltName=DNS:mirror.test",
                "-keyout",
                str(tls / "key"),
                "-out",
                str(tls / "cert"),
            ],
            stderr=subprocess.DEVNULL,
        )
        (
            tls / "default.conf"
        ).write_text("""server { listen 443 ssl; ssl_certificate /tls/cert;
ssl_certificate_key /tls/key; location / {proxy_pass http://metiva:80; proxy_http_version 1.1;} }
""")
        services["fixture"] = {
            "image": "python:3.12-slim",
            "command": ["python", "/tests/fixture.py"],
            "cap_add": ["NET_ADMIN"],
            "networks": {"backend": {"aliases": ["metiva-api"]}},
            "volumes": [
                f"{SCRIPTS}:/tests:ro",
                f"{fixture}:/fixture",
                f"{tls}:/tls:ro",
            ],
            "healthcheck": {
                "test": [
                    "CMD",
                    "python",
                    "-c",
                    "import socket;socket.create_connection(('93.184.216.34',8081),1).close()",
                ],
                "interval": "2s",
                "timeout": "2s",
                "retries": 10,
            },
        }
        services["tls-proxy"] = {
            "image": "nginx:1.28-alpine",
            "networks": {"backend": {"aliases": ["mirror.test"]}},
            "depends_on": ["metiva"],
            "volumes": [
                f"{tls}:/tls:ro",
                f"{tls}/default.conf:/etc/nginx/conf.d/default.conf:ro",
            ],
        }
        file = tmp / "compose.json"
        file.write_text(json.dumps(cfg))
        cmd = ["docker", "compose", "-p", name, "-f", str(file)]

        def compose(*args, **kw):
            return run([*cmd, *args], env=env, **kw)

        try:
            compose(
                "up",
                "-d",
                "--wait",
                "--wait-timeout",
                "120",
                "--no-build" if "--no-build" in sys.argv else "--build",
            )

            def client(*args, secret=None):
                compose(
                    "exec",
                    "-T",
                    "fixture",
                    "python",
                    "/tests/acceptance.py",
                    *args,
                    input=secret,
                )

            client("bootstrap", secret=(data / "bootstrap-secret").read_text())
            compose("kill", "-s", "SIGKILL", "metiva-worker")
            compose("up", "-d", "--wait", "metiva-worker")
            client("mirror")
            client("nested")
            client("controls")
            client("history-start")
            compose("kill", "-s", "SIGKILL", "metiva-worker")
            client("history-change")
            compose("up", "-d", "--wait", "metiva-worker")
            client("history-finish")
            compose("restart", "metiva-api", "metiva-worker", "metiva", "tls-proxy")
            client("restart")
        except BaseException:
            compose(
                "logs",
                "--tail",
                "45",
                "metiva-api",
                "metiva-worker",
                "metiva",
                "fixture",
            )
            raise
        finally:
            compose("down", "--remove-orphans", stdout=subprocess.DEVNULL)
        print("Compose acceptance passed.")


if __name__ == "__main__":
    main()
