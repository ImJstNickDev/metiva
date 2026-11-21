"""Start the production Compose file from an empty working directory."""

import argparse
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import tempfile
import urllib.request


def ghcr_digest_ref(value):
    if not re.fullmatch(
        r"ghcr\.io/[a-z0-9._-]+/[a-z0-9._/-]+@sha256:[a-f0-9]{64}", value
    ):
        raise argparse.ArgumentTypeError(
            "expected a GHCR image reference pinned by SHA-256 digest"
        )
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--compose-file",
        type=Path,
        default=os.environ.get(
            "METIVA_TEST_COMPOSE_FILE",
            Path(__file__).resolve().parents[2] / "compose-prod.yaml",
        ),
        help="production Compose file to test",
    )
    parser.add_argument(
        "--pull", action="store_true", help="pull the published Compose images"
    )
    parser.add_argument(
        "--ingress-image", type=ghcr_digest_ref, help="staged ingress image digest reference"
    )
    parser.add_argument(
        "--service-image", type=ghcr_digest_ref, help="staged API and worker image digest reference"
    )
    args = parser.parse_args()
    if bool(args.ingress_image) != bool(args.service_image):
        parser.error("--ingress-image and --service-image must be provided together")

    local_images = {
        "METIVA_IMAGE_TAG": "local",
        "METIVA_INGRESS_IMAGE": "metiva-ingress",
        "METIVA_SERVICE_IMAGE": "metiva-service",
    }
    with tempfile.TemporaryDirectory(prefix="metiva-standalone-") as tmp:
        root = Path(tmp)
        file = root / args.compose_file.name
        shutil.copyfile(args.compose_file, file)
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
            "COMPOSE_ENV_FILES",
            "METIVA_PUBLIC_URL",
            "METIVA_DATA_DIR",
            "METIVA_PORT",
            "METIVA_BIND_ADDRESS",
            "METIVA_SUBNET",
            "METIVA_INGRESS_REF",
            "METIVA_SERVICE_REF",
            *local_images,
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
        assert not config["networks"]["default"].get("ipam", {}).get("config"), (
            "Docker must allocate the default subnet without an explicit CIDR"
        )
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

        # Bind a random loopback port, leaving every production port and network untouched.
        env.update(
            METIVA_BIND_ADDRESS="127.0.0.1",
            METIVA_PORT="0",
            METIVA_PUBLIC_URL="http://localhost:8088",
        )
        # Render the opt-in subnet on every run. Use Docker's normal allocation
        # for the live deployment unless the local fixture needs a specific CIDR.
        subnet = os.environ.get("METIVA_TEST_SUBNET", "10.244.91.0/28")
        env["METIVA_SUBNET"] = subnet
        explicit = json.loads(compose("config", "--format", "json", capture=True))
        (network,) = explicit["networks"]
        assert explicit["networks"][network]["ipam"]["config"][0]["subnet"] == subnet
        assert all(set(s["networks"]) == {network} for s in explicit["services"].values())
        if not os.environ.get("METIVA_TEST_SUBNET"):
            del env["METIVA_SUBNET"]
        if args.ingress_image:
            env.update(
                METIVA_INGRESS_REF=args.ingress_image,
                METIVA_SERVICE_REF=args.service_image,
            )
            staged_config = json.loads(
                compose("config", "--format", "json", capture=True)
            )
            selected_images = {
                name: service["image"] for name, service in staged_config["services"].items()
            }
            expected_images = {
                "metiva": args.ingress_image,
                "metiva-api": args.service_image,
                "metiva-worker": args.service_image,
            }
            if selected_images != expected_images:
                raise RuntimeError(
                    f"Compose did not select the staged images: {selected_images}"
                )
        elif not args.pull:
            # The PR build runs before release images exist.
            env.update(
                {key: os.environ.get(key, default) for key, default in local_images.items()}
            )
        if args.pull or args.ingress_image:
            compose("pull")
        data = root / "data"
        data.mkdir(mode=0o700)
        (data / "artifacts").mkdir(mode=0o755)
        try:
            compose("up", "-d", "--pull", "never", "metiva")
            assert compose("ps", "--services", "--status", "running", capture=True).split() == ["metiva"]
            compose("up", "-d", "--wait", "--wait-timeout", "120", "--pull", "never")

            def url(path):
                address = compose("port", "metiva", "80", capture=True).strip()
                return "http://" + address + path

            def read(path):
                with urllib.request.urlopen(url(path), timeout=10) as r:
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

            setup = urllib.request.Request(
                url("/api/admin/setup"),
                data=json.dumps({
                    "bootstrapSecret": secret.decode().strip(),
                    "username": "operator",
                    "password": secrets.token_urlsafe(24),
                }).encode(),
                headers={"Content-Type": "application/json"},
            )
            with urllib.request.urlopen(setup, timeout=10) as session:
                cookie = session.headers["Set-Cookie"].split(";", 1)[0]
            lifecycle = urllib.request.Request(
                url("/api/admin/lifecycle"), headers={"Cookie": cookie}
            )
            with urllib.request.urlopen(lifecycle, timeout=10) as stream:
                assert stream.readline() == b"event: heartbeat\n"
                compose("down")
                assert stream.read().endswith(b"event: server-stopping\ndata: {}\n\n")
        finally:
            compose("down", "--remove-orphans")
        print("Standalone Compose acceptance passed.")


if __name__ == "__main__":
    main()
