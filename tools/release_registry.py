#!/usr/bin/env python3
"""Verify and promote OCI manifests, then package the two verified release images."""

import argparse
import base64
from dataclasses import dataclass
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import Request, urlopen

MEDIA_TYPES = (
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
)
# Only release registries receive credentials from the runner's Docker login.
REGISTRIES = {
    "ghcr.io": ("ghcr.io", "https://ghcr.io/token", "ghcr.io", "ghcr.io"),
    "docker.io": (
        "registry-1.docker.io",
        "https://auth.docker.io/token",
        "registry.docker.io",
        "https://index.docker.io/v1/",
    ),
}
DIGEST = r"sha256:[a-f0-9]{64}"
TAG = r"[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}"
VERSION = r"v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"


def docker_auth(key):
    path = Path(os.environ.get("DOCKER_CONFIG", Path.home() / ".docker")) / "config.json"
    config = json.loads(path.read_text())
    helper = config.get("credHelpers", {}).get(key) or config.get("credsStore")
    if helper:
        result = subprocess.run(
            ["docker-credential-" + helper, "get"],
            input=key,
            text=True,
            capture_output=True,
            check=True,
        )
        auth = json.loads(result.stdout)
        return base64.b64encode(f"{auth['Username']}:{auth['Secret']}".encode()).decode()
    auth = config.get("auths", {}).get(key, {}).get("auth")
    if not auth:
        raise ValueError(f"Missing Docker login for {key}")
    return auth


def request(url, authorization, accept="application/json"):
    req = Request(url, headers={"Accept": accept})
    req.add_unredirected_header("Authorization", authorization)
    return urlopen(req, timeout=45)


def lookup(repo, reference):
    host, _, name = repo.partition("/")
    if host not in REGISTRIES or not re.fullmatch(
        r"[a-z0-9][a-z0-9._-]*/[a-z0-9][a-z0-9._-]*", name
    ):
        raise ValueError(f"Invalid registry repository {repo!r}")
    if not re.fullmatch(f"{DIGEST}|{TAG}", reference):
        raise ValueError(f"Invalid image reference {reference!r}")
    registry, token_url, service, login_key = REGISTRIES[host]
    query = urlencode({"service": service, "scope": f"repository:{name}:pull"})
    with request(f"{token_url}?{query}", "Basic " + docker_auth(login_key)) as response:
        token = json.load(response)["token"]
    try:
        response = request(
            f"https://{registry}/v2/{name}/manifests/{reference}",
            "Bearer " + token,
            ", ".join(MEDIA_TYPES),
        )
    except HTTPError as error:
        if error.code == 404:
            errors = json.load(error).get("errors", [])
            if errors and all(
                e.get("code") in ("MANIFEST_UNKNOWN", "NAME_UNKNOWN") for e in errors
            ):
                return None
        raise
    with response:
        if response.headers.get_content_type() not in MEDIA_TYPES:
            raise ValueError(f"Unexpected manifest media type for {repo}:{reference}")
        digest = "sha256:" + hashlib.sha256(response.read()).hexdigest()
        reported = response.headers.get("Docker-Content-Digest")
        if reported and reported != digest:
            raise ValueError(
                f"Registry digest for {repo}:{reference} disagrees with its manifest bytes"
            )
        return digest


@dataclass(frozen=True)
class Image:
    role: str
    ghcr: str
    hub: str
    digest: str


def images(args):
    if not re.fullmatch(VERSION, args.tag):
        raise ValueError(f"Invalid release tag {args.tag!r}")
    if not args.ghcr.startswith("ghcr.io/") or not args.hub.startswith("docker.io/"):
        raise ValueError("Expected GHCR source and Docker Hub destination repositories")
    pair = [
        Image("INGRESS", args.ghcr, args.hub, args.ingress_digest),
        Image("SERVICE", args.ghcr + "-service", args.hub + "-service", args.service_digest),
    ]
    for image in pair:
        if not re.fullmatch(DIGEST, image.digest):
            raise ValueError(f"Invalid {image.role} digest {image.digest!r}")
    return pair


def verified(repo, reference, expected, allow_missing=False):
    actual = lookup(repo, reference)
    if actual is None and allow_missing:
        return False
    if actual != expected:
        raise ValueError(
            f'{repo} {reference} resolves to {actual or "nothing"}, expected {expected}'
        )
    return True


def release_tags(pair, tag, allow_missing=False):
    missing = []
    for image in pair:
        for repo in (image.ghcr, image.hub):
            if not verified(repo, tag, image.digest, allow_missing):
                missing.append((image, repo))
    return missing


def promote(pair, tag):
    for image in pair:
        verified(image.ghcr, image.digest, image.digest)
    # Check every version tag before the first write. A retry can finish a partial copy.
    for image, repo in release_tags(pair, tag, allow_missing=True):
        if not verified(repo, tag, image.digest, allow_missing=True):
            subprocess.run(
                [
                    "skopeo",
                    "copy",
                    "--all",
                    "--preserve-digests",
                    f"docker://{image.ghcr}@{image.digest}",
                    f"docker://{repo}:{tag}",
                ],
                check=True,
            )
    release_tags(pair, tag)


def docker_archive(path, tags):
    """Write Docker's portable manifest/config/layers format with canonical tar/gzip metadata."""
    with tempfile.TemporaryDirectory(dir=path.parent) as temp:
        saved = Path(temp) / "docker.tar"
        subprocess.run(
            ["docker", "image", "save", "--platform", "linux/amd64", "--output", saved, *tags],
            check=True,
        )
        with tarfile.open(saved, "r:") as source:
            manifests = json.load(source.extractfile("manifest.json"))
            if sorted(tag for m in manifests for tag in m["RepoTags"]) != sorted(tags):
                raise ValueError("Docker archive tags disagree with the verified release pair")
            manifests.sort(key=lambda m: sorted(m["RepoTags"]))
            blobs = set()
            for m in manifests:
                m["RepoTags"].sort()
                config = json.load(source.extractfile(m["Config"]))
                if (config["os"], config["architecture"]) != ("linux", "amd64"):
                    raise ValueError(f"Unexpected platform for {m['RepoTags']}")
                blobs.update([m["Config"], *m["Layers"]])
            manifest = json.dumps(manifests, sort_keys=True, separators=(",", ":")).encode()
            with path.open("wb") as output, gzip.GzipFile(
                filename="", mode="wb", fileobj=output, mtime=0
            ) as zipped:
                with tarfile.open(fileobj=zipped, mode="w|") as archive:
                    entry = tarfile.TarInfo("manifest.json")
                    entry.size = len(manifest)
                    archive.addfile(entry, io.BytesIO(manifest))
                    for name in sorted(blobs):
                        entry = tarfile.TarInfo(name)
                        entry.size = source.getmember(name).size
                        with source.extractfile(name) as data:
                            archive.addfile(entry, data)


def package(args, pair):
    release_tags(pair, args.tag)
    args.output.mkdir(parents=True, exist_ok=True)
    tags = []
    for image in pair:
        pin = f"{image.ghcr}@{image.digest}"
        tag = f"{image.ghcr}:{args.tag}"
        subprocess.run(["docker", "image", "pull", "--platform", "linux/amd64", pin], check=True)
        subprocess.run(["docker", "image", "tag", pin, tag], check=True)
        tags.append(tag)
    with tempfile.TemporaryDirectory(dir=args.output) as temp:
        work = Path(temp)
        docker_archive(work / f"metiva-{args.tag}-linux-amd64.tar.gz", tags)
        shutil.copyfile(args.compose, work / "compose-prod.yaml")
        pins = [f"# Metiva {args.tag} verified registry manifests"]
        for image in pair:
            pins.extend(
                [
                    f"METIVA_{image.role}_REF={image.ghcr}@{image.digest}",
                    f"# Docker Hub: {image.hub}@{image.digest}",
                ]
            )
        (work / "image-digests.txt").write_text("\n".join(pins) + "\n")
        sums = []
        for asset in sorted(work.iterdir()):
            with asset.open("rb") as data:
                checksum = hashlib.file_digest(data, "sha256").hexdigest()
            sums.append(f"{checksum}  {asset.name}")
        (work / "SHA256SUMS").write_text("\n".join(sums) + "\n")
        for asset in work.iterdir():
            asset.replace(args.output / asset.name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("lookup").add_argument("image")
    for command in ("promote", "package"):
        action = commands.add_parser(command)
        for name in ("ghcr", "hub", "tag", "ingress-digest", "service-digest"):
            action.add_argument("--" + name, required=True)
        if command == "package":
            action.add_argument("--compose", type=Path, required=True)
            action.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "lookup":
        repo, tag = args.image.rsplit(":", 1)
        print(lookup(repo, tag) or "")
    elif args.command == "promote":
        promote(images(args), args.tag)
    else:
        package(args, images(args))


if __name__ == "__main__":
    try:
        main()
    except (
        OSError,
        ValueError,
        KeyError,
        subprocess.CalledProcessError,
        URLError,
        tarfile.TarError,
    ) as error:
        sys.exit(str(error))
