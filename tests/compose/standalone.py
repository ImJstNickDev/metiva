"""Start the production Compose file from an empty working directory."""

import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import sys
import tempfile
import urllib.request


def main():
    source = Path(
        os.environ.get(
            "METIVA_TEST_COMPOSE_FILE",
            Path(__file__).resolve().parents[2] / "compose-prod.yaml",
        )
    )
    with tempfile.TemporaryDirectory(prefix="metiva-standalone-") as tmp:
        root = Path(tmp)
        file = root / source.name
        shutil.copyfile(source, file)
        command = [
            "docker",
            "compose",
            "-p",
            "metiva-prod-check-" + secrets.token_hex(4),
            "-f",
            str(file),
        ]
        env = {
            **os.environ,
            "METIVA_UID": str(os.getuid()),
            "METIVA_GID": str(os.getgid()),
        }
        for key in [
            "METIVA_PUBLIC_URL",
            "METIVA_DATA_DIR",
            "METIVA_PORT",
            "METIVA_BIND_ADDRESS",
        ]:
            env.pop(key, None)

        def compose(*args, capture=False):
            return subprocess.run(
                command + list(args),
                cwd=root,
                env=env,
                text=True,
                stdout=subprocess.PIPE if capture else None,
                check=True,
            ).stdout

        config = json.loads(compose("config", "--format", "json", capture=True))
        services = config["services"]
        assert set(services) == {"metiva", "metiva-api", "metiva-worker"}
        assert all("build" not in s for s in services.values())
        assert not any(n.get("external") for n in config["networks"].values())
        assert not any(n.get("internal") for n in config["networks"].values())
        assert set(config["networks"]) == {"default"}
        assert not services["metiva-api"].get("ports") and not services[
            "metiva-worker"
        ].get("ports")
        ports = services["metiva"]["ports"]
        assert (
            len(ports) == 1
            and ports[0]["published"] == "8088"
            and ports[0]["target"] == 80
        )
        assert all(v["type"] == "bind" for s in services.values() for v in s["volumes"])
        assert services["metiva"]["volumes"][0]["read_only"]
        if "--pull" in sys.argv:
            assert all(s["image"].endswith(":v0.1.4") for s in services.values())

        # Bind a random loopback port, leaving every production port and network untouched.
        env.update(
            METIVA_BIND_ADDRESS="127.0.0.1",
            METIVA_PORT="0",
            METIVA_PUBLIC_URL="http://localhost:8088",
        )
        if subnet := os.environ.get("METIVA_TEST_SUBNET"):
            env["METIVA_SUBNET"] = subnet
        if "--pull" in sys.argv:
            compose("pull")
        else:
            # The PR build runs before release images exist.
            env.setdefault("METIVA_IMAGE_TAG", "local")
            env.setdefault("METIVA_INGRESS_IMAGE", "metiva-ingress")
            env.setdefault("METIVA_SERVICE_IMAGE", "metiva-service")
        data = root / "data"
        data.mkdir(mode=0o700)
        (data / "artifacts").mkdir(mode=0o755)
        try:
            compose("up", "-d", "--wait", "--wait-timeout", "120", "--pull", "never")

            def read(path):
                address = compose("port", "metiva", "80", capture=True).strip()
                with urllib.request.urlopen(
                    "http://" + address + path, timeout=10
                ) as r:
                    return r.read()

            assert read("/healthz") == b""
            assert b"metiva" in read("/").lower()
            assert json.loads(read("/api/admin/session"))["setupRequired"]
            secret = (data / "bootstrap-secret").read_bytes()
            assert len(secret.strip()) >= 32
            compose("down")
            compose("up", "-d", "--wait", "--wait-timeout", "120", "--pull", "never")
            assert json.loads(read("/api/admin/session"))["setupRequired"]
            assert (data / "bootstrap-secret").read_bytes() == secret
        finally:
            compose("down", "--remove-orphans")
        print("Standalone Compose acceptance passed.")


if __name__ == "__main__":
    main()
