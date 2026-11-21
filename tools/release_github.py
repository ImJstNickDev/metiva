"""Gate a release on the exact main commit and publish its verified draft."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time
import tomllib


class ReleaseError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise ReleaseError(message)


def api(repo, path, method="GET", body=None):
    args = ["gh", "api", "--method", method, f"repos/{repo}/{path}"]
    if body is not None:
        args.extend(["--input", "-"])
    output = subprocess.check_output(
        args, input=json.dumps(body) if body is not None else None, text=True
    )
    return json.loads(output) if output else None


def verify_ci(repo, sha):
    path = f"actions/workflows/check.yml/runs?head_sha={sha}&branch=main&event=push&per_page=1"
    deadline = time.monotonic() + 900
    while True:
        runs = api(repo, path)["workflow_runs"]
        if runs:
            run = runs[0]
            require(
                (run["event"], run["head_branch"], run["head_sha"]) == ("push", "main", sha),
                "Verify belongs to a different event, branch or commit",
            )
            if run["status"] == "completed":
                require(run["conclusion"] == "success", f"Verify did not pass: {run['html_url']}")
                print(f"Verified {sha}: {run['html_url']}", flush=True)
                return
        require(time.monotonic() < deadline, "Timed out waiting for Verify on this commit")
        time.sleep(15)


def release_notes(tag):
    changelog = Path("CHANGELOG.md").read_text()
    section = re.search(rf"^## {re.escape(tag)}\s*\n(.*?)(?=^## |\Z)", changelog, re.M | re.S)
    require(section and section[1].strip(), f"Missing CHANGELOG.md section for {tag}")
    return section[1].strip()


def verify_checkout(repo, sha, tag):
    require(
        re.fullmatch(r"v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)", tag), "Expected vX.Y.Z"
    )
    require(re.fullmatch("[0-9a-f]{40}", sha), "Expected a full commit SHA")
    require(
        os.environ.get("GITHUB_REF") == "refs/heads/main"
        and os.environ.get("GITHUB_EVENT_NAME") == "workflow_dispatch",
        "Dispatch this workflow on main",
    )
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    require(sha == head, "Checkout differs from the dispatched commit")
    rust = tomllib.loads(Path("crates/metiva/Cargo.toml").read_text())
    web = json.loads(Path("apps/web/package.json").read_text())
    require(
        rust["package"]["version"] == web["version"] == tag[1:],
        "Package versions differ from requested tag",
    )
    release_notes(tag)
    env = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith("METIVA_") and not k.startswith("COMPOSE_")
    }
    raw = subprocess.check_output(
        [
            "docker",
            "compose",
            "--env-file",
            os.devnull,
            "-f",
            "compose-prod.yaml",
            "config",
            "--format",
            "json",
        ],
        env=env,
        text=True,
    )
    images = {k: v["image"] for k, v in json.loads(raw)["services"].items()}
    base = f"ghcr.io/{repo.lower()}"
    require(
        images
        == {
            "metiva": f"{base}:{tag}",
            "metiva-api": f"{base}-service:{tag}",
            "metiva-worker": f"{base}-service:{tag}",
        },
        "Compose defaults do not match the release",
    )


def existing_draft(repo, sha, tag):
    page = 1
    while True:
        releases = api(repo, f"releases?per_page=100&page={page}")
        for release in releases:
            if release["tag_name"] == tag:
                require(release["draft"], f"{tag} is already published")
                require(
                    release["target_commitish"] == sha
                    and release["author"]["login"] == "github-actions[bot]",
                    "Existing draft belongs to another commit or publisher",
                )
                return release
        if len(releases) < 100:
            return None
        page += 1


def check_tag(repo, sha, tag, draft, *, preflight=False):
    refs = api(repo, f"git/matching-refs/tags/{tag}")
    ref = next((r["object"] for r in refs if r["ref"] == f"refs/tags/{tag}"), None)
    if ref:
        require(ref["type"] == "tag", f"Tag {tag} is not annotated")
        obj = api(repo, f"git/tags/{ref['sha']}")
        # A read-only job may see the ref but not the associated private draft.
        recognizable = obj.get("message", "").strip() == f"Metiva {tag} from verified {sha}"
        require(
            draft or (preflight and recognizable), f"Tag {tag} already exists outside this draft"
        )
        require(
            obj["object"]["type"] == "commit" and obj["object"]["sha"] == sha,
            "Existing release tag targets another commit",
        )
    return bool(ref)


def assets(directory, tag):
    files = ["compose-prod.yaml", f"metiva-{tag}-linux-amd64.tar.gz", "image-digests.txt"]
    require(
        {p.name for p in directory.iterdir()} == {*files, "SHA256SUMS"},
        "Unexpected release file set",
    )
    require(
        (directory / "compose-prod.yaml").read_bytes() == Path("compose-prod.yaml").read_bytes(),
        "Release Compose file differs from the verified checkout",
    )
    hashes = {}
    for name in [*files, "SHA256SUMS"]:
        path = directory / name
        require(
            path.is_file() and not path.is_symlink() and path.stat().st_size,
            f"Invalid release asset {name}",
        )
        with path.open("rb") as stream:
            hashes[name] = hashlib.file_digest(stream, "sha256").hexdigest()
    expected = sorted(f"{hashes[name]}  {name}" for name in files)
    require(
        sorted((directory / "SHA256SUMS").read_text().splitlines()) == expected,
        "SHA256SUMS mismatch",
    )
    return hashes


def publish(repo, sha, tag, directory, draft, has_tag):
    hashes = assets(directory, tag)
    if not draft:
        draft = api(
            repo,
            "releases",
            "POST",
            {
                "tag_name": tag,
                "target_commitish": sha,
                "name": tag,
                "body": release_notes(tag),
                "draft": True,
                "prerelease": tag.startswith("v0."),
            },
        )
    path = f"releases/{draft['id']}"
    uploaded = {a["name"]: a for a in draft["assets"]}
    require(set(uploaded) <= set(hashes), "Draft contains unrecognized files")
    for name, digest in hashes.items():
        prior = uploaded.get(name)
        if prior and prior["state"] == "uploaded" and prior.get("digest") == f"sha256:{digest}":
            continue
        # Only this workflow's draft for this exact commit can reach this point.
        cmd = ["gh", "release", "upload", tag, str(directory / name), "--repo", repo]
        if prior:
            cmd.append("--clobber")
        subprocess.run(cmd, check=True)
    fresh = api(repo, path)
    require(
        fresh["draft"] and fresh["target_commitish"] == sha and fresh["tag_name"] == tag,
        "Draft changed",
    )
    remote = {a["name"]: a.get("digest") for a in fresh["assets"] if a["state"] == "uploaded"}
    require(
        len(fresh["assets"]) == len(hashes)
        and remote == {k: f"sha256:{v}" for k, v in hashes.items()},
        "Uploaded asset digests differ from verified files",
    )
    if not has_tag:
        obj = api(
            repo,
            "git/tags",
            "POST",
            {
                "tag": tag,
                "message": f"Metiva {tag} from verified {sha}",
                "object": sha,
                "type": "commit",
            },
        )
        api(repo, "git/refs", "POST", {"ref": "refs/tags/" + tag, "sha": obj["sha"]})
    require(check_tag(repo, sha, tag, fresh), "Release tag missing before publication")
    release = api(repo, path, "PATCH", {"draft": False})
    require(release.get("immutable"), f"Published {tag} without GitHub release immutability")
    print(release["html_url"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_subparsers(dest="mode", required=True)
    modes.add_parser("check").add_argument("tag")
    final = modes.add_parser("publish")
    final.add_argument("tag")
    final.add_argument("directory", type=Path)
    args = parser.parse_args()
    repo, sha = os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_SHA"]
    verify_checkout(repo, sha, args.tag)
    draft = existing_draft(repo, sha, args.tag)
    has_tag = check_tag(repo, sha, args.tag, draft, preflight=args.mode == "check")
    verify_ci(repo, sha)
    if args.mode == "publish":
        publish(repo, sha, args.tag, args.directory, draft, has_tag)


if __name__ == "__main__":
    try:
        main()
    except (ReleaseError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error))
