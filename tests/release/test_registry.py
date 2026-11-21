import argparse
from email.message import Message
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError

from tools import release_registry as registry

INGRESS = "sha256:" + "1" * 64
SERVICE = "sha256:" + "2" * 64
TAG = "v1.2.3"
GHCR = "ghcr.io/example/metiva"
HUB = "docker.io/example/metiva"


class RegistryLookup(unittest.TestCase):
    def lookup(self, response):
        with patch.object(registry, "docker_auth", return_value="test"), patch.object(
            registry, "request", side_effect=[io.BytesIO(b'{"token":"fixture"}'), response]
        ):
            return registry.lookup(GHCR, TAG)

    def test_digest_covers_the_raw_index_including_attestation(self):
        descriptors = [
            {"digest": INGRESS, "platform": {"os": "linux", "architecture": "amd64"}},
            {"digest": SERVICE, "platform": {"os": "unknown", "architecture": "unknown"}},
        ]
        data = json.dumps({"schemaVersion": 2, "manifests": descriptors}, indent=2).encode()
        response = io.BytesIO(data)
        response.headers = Message()
        response.headers["Content-Type"] = registry.MEDIA_TYPES[0]
        expected = "sha256:" + hashlib.sha256(data).hexdigest()
        self.assertEqual(self.lookup(response), expected)

    def test_mismatching_registry_digest_fails(self):
        response = io.BytesIO(b"{}")
        response.headers = Message()
        response.headers["Content-Type"] = registry.MEDIA_TYPES[0]
        response.headers["Docker-Content-Digest"] = INGRESS
        with self.assertRaisesRegex(ValueError, "manifest bytes"):
            self.lookup(response)

    def test_only_explicit_registry_not_found_is_absent(self):
        for status, code in (
            (404, "MANIFEST_UNKNOWN"),
            (404, "NAME_UNKNOWN"),
            (401, "UNAUTHORIZED"),
            (403, "DENIED"),
            (429, "TOOMANYREQUESTS"),
            (404, "DENIED"),
            (500, "MANIFEST_UNKNOWN"),
        ):
            with self.subTest(status=status, code=code):
                body = io.BytesIO(json.dumps({"errors": [{"code": code}]}).encode())
                error = HTTPError("fixture", status, "fixture", {}, body)
                if status == 404 and code.endswith("_UNKNOWN"):
                    self.assertIsNone(self.lookup(error))
                else:
                    with self.assertRaises(HTTPError):
                        self.lookup(error)
        with self.assertRaises(ValueError):
            self.lookup(HTTPError("fixture", 404, "fixture", {}, io.BytesIO(b"<html>404</html>")))


class Promotion(unittest.TestCase):
    def setUp(self):
        self.pair = registry.images(
            argparse.Namespace(
                tag=TAG, ghcr=GHCR, hub=HUB, ingress_digest=INGRESS, service_digest=SERVICE
            )
        )
        self.remote = {(i.ghcr, i.digest): i.digest for i in self.pair}
        self.copies = []

    def copy(self, command, *, check):
        self.assertTrue(check)
        self.assertEqual(command[:4], ["skopeo", "copy", "--all", "--preserve-digests"])
        source, digest = command[4].removeprefix("docker://").split("@")
        dest, tag = command[5].removeprefix("docker://").rsplit(":", 1)
        self.assertEqual(tag, TAG)
        self.remote[dest, tag] = self.remote[source, digest]
        self.copies.append(dest)

    def run_promote(self, copy):
        with patch.object(
            registry, "lookup", side_effect=lambda r, t: self.remote.get((r, t))
        ), patch.object(registry.subprocess, "run", side_effect=copy):
            registry.promote(self.pair, TAG)

    def test_conflict_in_last_destination_prevents_every_copy(self):
        self.remote[self.pair[-1].hub, TAG] = INGRESS
        with self.assertRaisesRegex(ValueError, "expected"):
            self.run_promote(self.copy)
        self.assertEqual(self.copies, [])

    def test_partial_failure_can_resume_and_full_retry_is_read_only(self):
        def fail_after_first(cmd, *, check):
            if self.copies:
                raise subprocess.CalledProcessError(1, cmd)
            self.copy(cmd, check=check)

        with self.assertRaises(subprocess.CalledProcessError):
            self.run_promote(fail_after_first)
        self.run_promote(self.copy)
        self.run_promote(self.copy)
        self.assertEqual(self.copies, [GHCR, HUB, GHCR + "-service", HUB + "-service"])

    def test_verify_after_copy_rejects_registry_rewrite(self):
        def rewrite(cmd, *, check):
            self.copy(cmd, check=check)
            self.remote[self.copies[-1], TAG] = "sha256:" + "f" * 64

        with self.assertRaisesRegex(ValueError, "expected"):
            self.run_promote(rewrite)


class Packaging(unittest.TestCase):
    def test_verified_inputs_canonical_archive_and_checksums(self):
        with tempfile.TemporaryDirectory() as tmp:
            args = argparse.Namespace(
                tag=TAG,
                ghcr=GHCR,
                hub=HUB,
                ingress_digest=INGRESS,
                service_digest=SERVICE,
                compose=Path(tmp) / "compose.yaml",
                output=Path(tmp) / "out",
            )
            args.compose.write_text("services: {}\n")
            pair = registry.images(args)
            published = {(repo, TAG): i.digest for i in pair for repo in (i.ghcr, i.hub)}
            tags = [f"{i.ghcr}:{TAG}" for i in pair]
            commands = []
            clock = 0

            def docker(command, *, check):
                self.assertTrue(check)
                self.assertEqual(command[:2], ["docker", "image"])
                commands.append(command)
                if command[2] != "save":
                    return
                self.assertEqual(command[3:6], ["--platform", "linux/amd64", "--output"])
                self.assertEqual(command[7:], tags)
                manifests = [
                    {"Config": f"config-{i}", "RepoTags": [tag], "Layers": ["shared"]}
                    for i, tag in enumerate(tags)
                ]
                entries = {
                    "config-0": b'{"os":"linux","architecture":"amd64"}',
                    "config-1": b'{"architecture":"amd64","os":"linux"}',
                    "shared": b"layer",
                }
                entries["manifest.json"] = json.dumps(
                    manifests[:: 1 if clock == 0 else -1]
                ).encode()
                with tarfile.open(command[6], "w") as tar:
                    for name, body in entries.items():
                        info = tarfile.TarInfo(name)
                        info.mtime = clock
                        info.size = len(body)
                        tar.addfile(info, io.BytesIO(body))

            with patch.object(
                registry, "lookup", side_effect=lambda r, t: published.get((r, t))
            ), patch.object(registry.subprocess, "run", side_effect=docker):
                registry.package(args, pair)
                expected = {f.name: f.read_bytes() for f in args.output.iterdir()}
                clock += 1
                registry.package(args, pair)
                self.assertEqual(expected, {f.name: f.read_bytes() for f in args.output.iterdir()})
                published[HUB, TAG] = SERVICE
                with self.assertRaises(ValueError):
                    registry.package(args, pair)

            expected_commands = []
            for image, tag in zip(pair, tags):
                pin = f"{image.ghcr}@{image.digest}"
                expected_commands.extend(
                    [
                        ["docker", "image", "pull", "--platform", "linux/amd64", pin],
                        ["docker", "image", "tag", pin, tag],
                    ]
                )
                self.assertIn(
                    f"METIVA_{image.role}_REF={pin}", expected["image-digests.txt"].decode()
                )
                self.assertIn(f"{image.hub}@{image.digest}", expected["image-digests.txt"].decode())
            self.assertEqual([c for c in commands if c[2] != "save"], expected_commands * 2)
            name = f"metiva-{TAG}-linux-amd64.tar.gz"
            self.assertEqual(
                set(expected), {name, "image-digests.txt", "SHA256SUMS", "compose-prod.yaml"}
            )
            self.assertEqual(expected["compose-prod.yaml"], args.compose.read_bytes())
            with tarfile.open(args.output / name) as tar:
                manifest = json.load(tar.extractfile("manifest.json"))
                self.assertEqual(sorted(m["RepoTags"][0] for m in manifest), sorted(tags))
                self.assertEqual(tar.getnames().count("shared"), 1)
            sums = expected["SHA256SUMS"].decode().splitlines()
            self.assertEqual(
                sums,
                [
                    f"{hashlib.sha256(expected[n]).hexdigest()}  {n}"
                    for n in sorted(expected)
                    if n != "SHA256SUMS"
                ],
            )


if __name__ == "__main__":
    unittest.main()
