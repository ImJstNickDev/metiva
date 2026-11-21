import contextlib
import hashlib
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from tools import release_github as release

REPO = "test-owner/metiva"
SHA = "a" * 40
TAG = "v0.2.0"
TAG_OBJECT = "b" * 40
REF = f"refs/tags/{TAG}"


def completed_run(**overrides):
    return {
        "event": "push",
        "head_branch": "main",
        "head_sha": SHA,
        "status": "completed",
        "conclusion": "success",
        "html_url": "https://github.com/test-owner/metiva/actions/runs/1",
    } | overrides


class VerifyTests(unittest.TestCase):
    def test_rehearsal_requires_verified_code_and_performs_no_release_operations(self):
        with (
            patch.dict(os.environ, {"GITHUB_REPOSITORY": REPO, "GITHUB_SHA": SHA}),
            patch.object(sys, "argv", ["release_github.py", "rehearse", TAG]),
            patch.object(release, "verify_checkout") as checkout,
            patch.object(release, "api", return_value={"workflow_runs": [completed_run()]}) as api,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            release.main()
        checkout.assert_called_once_with(REPO, SHA, TAG)
        self.assertEqual(api.call_count, 1)
        self.assertIn("actions/workflows/check.yml/runs?", api.call_args.args[1])

    def test_waits_for_the_exact_successful_push(self):
        responses = [
            {"workflow_runs": []},
            {"workflow_runs": [completed_run(status="in_progress", conclusion=None)]},
            {"workflow_runs": [completed_run()]},
        ]
        with (
            patch.object(release, "api", side_effect=responses) as api,
            patch.object(release.time, "sleep") as sleep,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            release.verify_ci(REPO, SHA)
        self.assertEqual(sleep.call_count, 2)
        for call in api.call_args_list:
            repo, endpoint = call.args
            self.assertEqual(repo, REPO)
            self.assertIn("actions/workflows/check.yml/runs?", endpoint)
            for param in [f"head_sha={SHA}", "branch=main", "event=push"]:
                self.assertIn(param, endpoint)

    def test_rejects_failed_cancelled_or_unrelated_verify(self):
        for change in [
            {"conclusion": "failure"},
            {"conclusion": "cancelled"},
            {"head_sha": "c" * 40},
            {"head_branch": "feature"},
            {"event": "pull_request"},
        ]:
            with (
                self.subTest(change=change),
                patch.object(
                    release, "api", return_value={"workflow_runs": [completed_run(**change)]}
                ),
            ):
                with self.assertRaises(release.ReleaseError):
                    release.verify_ci(REPO, SHA)


class PublishTests(unittest.TestCase):
    def setUp(self):
        temp = self.enterContext(tempfile.TemporaryDirectory())
        self.enterContext(contextlib.chdir(temp))
        self.enterContext(contextlib.redirect_stdout(io.StringIO()))
        self.directory = Path("assets")
        self.directory.mkdir()
        self.compose = "services: {}\n"
        Path("compose-prod.yaml").write_text(self.compose)
        Path("CHANGELOG.md").write_text(
            f"## {TAG}\n\nExample change.\n\n## v0.1.0\n\nEarlier change.\n"
        )
        payloads = {
            "compose-prod.yaml": self.compose.encode(),
            "image-digests.txt": b"example manifest\n",
            f"metiva-{TAG}-linux-amd64.tar.gz": b"offline image fixture\n",
        }
        sums = []
        for name, data in payloads.items():
            (self.directory / name).write_bytes(data)
            sums.append(f"{hashlib.sha256(data).hexdigest()}  {name}\n")
        (self.directory / "SHA256SUMS").write_text("".join(sums))
        self.draft = None
        self.tag_ref = None
        self.tag_commit = SHA
        self.tag_message = ""
        self.operations = []
        self.fail_upload = None
        self.fail_publish = False
        self.corrupt_readback = False
        self.enterContext(patch.object(release, "api", side_effect=self.github_api))
        self.enterContext(patch.object(release.subprocess, "run", side_effect=self.upload))
        self.enterContext(patch.object(release, "verify_checkout"))
        self.enterContext(patch.dict(os.environ, {"GITHUB_REPOSITORY": REPO, "GITHUB_SHA": SHA}))
        self.enterContext(
            patch.object(sys, "argv", ["release_github.py", "publish", TAG, str(self.directory)])
        )

    def github_api(self, repo, path, method="GET", body=None):
        self.assertEqual(repo, REPO)
        self.operations.append((method, path))
        if method == "GET":
            if path.startswith("actions/workflows/check.yml/runs?"):
                return {"workflow_runs": [completed_run()]}
            if path.startswith("releases?"):
                return [self.draft] if self.draft else []
            if path == f"git/matching-refs/tags/{TAG}":
                return [self.tag_ref] if self.tag_ref else []
            if path == f"git/tags/{TAG_OBJECT}":
                return {
                    "object": {"type": "commit", "sha": self.tag_commit},
                    "tag": TAG,
                    "message": self.tag_message,
                }
            if path == "releases/1":
                if self.corrupt_readback:
                    wrong = [
                        asset | {"digest": "sha256:" + "0" * 64} for asset in self.draft["assets"]
                    ]
                    return self.draft | {"assets": wrong}
                return self.draft
        if (method, path) == ("POST", "releases"):
            self.assertIsNone(self.draft)
            self.assertTrue(body["draft"])
            self.assertEqual(body["target_commitish"], SHA)
            self.draft = body | {"id": 1, "assets": [], "author": {"login": "github-actions[bot]"}}
            return self.draft
        if (method, path) == ("POST", "git/tags"):
            self.assertEqual((body["object"], body["type"], body["tag"]), (SHA, "commit", TAG))
            self.tag_message = body["message"]
            return {"sha": TAG_OBJECT}
        if (method, path) == ("POST", "git/refs"):
            self.assertEqual(body, {"ref": REF, "sha": TAG_OBJECT})
            self.tag_ref = {"ref": REF, "object": {"type": "tag", "sha": TAG_OBJECT}}
            return self.tag_ref
        if (method, path) == ("PATCH", "releases/1"):
            self.assertEqual(body, {"draft": False})
            if self.fail_publish:
                raise subprocess.CalledProcessError(1, "gh api")
            self.draft.update(
                draft=False, immutable=True, html_url="https://example.invalid/release"
            )
            return self.draft
        self.fail(f"Unexpected GitHub call {method} {path}")

    def upload(self, cmd, check):
        self.assertTrue(check)
        self.assertEqual(cmd[:4], ["gh", "release", "upload", TAG])
        file = Path(cmd[4])
        self.operations.append(("UPLOAD", file.name))
        if file.name == self.fail_upload:
            raise subprocess.CalledProcessError(1, cmd)
        self.assertTrue(self.draft["draft"])
        self.assertFalse(any(asset["name"] == file.name for asset in self.draft["assets"]))
        digest = hashlib.sha256(file.read_bytes()).hexdigest()
        self.draft["assets"].append(
            {"name": file.name, "state": "uploaded", "digest": "sha256:" + digest}
        )

    def test_partial_upload_then_retry_publishes_last(self):
        self.fail_upload = "SHA256SUMS"
        with self.assertRaises(subprocess.CalledProcessError):
            release.main()
        self.assertTrue(self.draft["draft"])
        self.assertTrue(self.draft["assets"])
        already_uploaded = {asset["name"] for asset in self.draft["assets"]}

        self.operations.clear()
        self.fail_upload = None
        self.fail_publish = True
        with self.assertRaises(subprocess.CalledProcessError):
            release.main()
        self.assertTrue(self.draft["draft"])
        self.assertNotIn(("POST", "releases"), self.operations)
        uploaded_on_retry = {name for method, name in self.operations if method == "UPLOAD"}
        self.assertTrue(uploaded_on_retry.isdisjoint(already_uploaded))
        self.assertEqual(
            {asset["name"] for asset in self.draft["assets"]},
            {p.name for p in self.directory.iterdir()},
        )

        # The gate's read-only token can see the annotated tag while GitHub hides the draft.
        self.assertTrue(release.check_tag(REPO, SHA, TAG, draft=None, preflight=True))
        self.operations.clear()
        self.fail_publish = False
        release.main()
        self.assertEqual(self.operations[-1], ("PATCH", "releases/1"))
        self.assertTrue(all(method == "GET" for method, _ in self.operations[:-1]))
        self.assertFalse(self.draft["draft"])

        self.operations.clear()
        with self.assertRaises(release.ReleaseError):
            release.main()
        self.assertTrue(all(method == "GET" for method, _ in self.operations))

    def test_readback_digest_mismatch_blocks_publication(self):
        self.corrupt_readback = True
        with self.assertRaises(release.ReleaseError):
            release.main()
        self.assertTrue(self.draft["draft"])
        self.assertNotIn(("PATCH", "releases/1"), self.operations)
        self.assertIsNone(self.tag_ref)

    def test_modified_local_asset_blocks_draft_creation(self):
        (self.directory / "image-digests.txt").write_text("changed after checksumming")
        with self.assertRaises(release.ReleaseError):
            release.main()
        self.assertIsNone(self.draft)
        self.assertTrue(all(method == "GET" for method, _ in self.operations))

    def test_foreign_draft_and_conflicting_tags_block_mutation(self):
        self.draft = {
            "id": 1,
            "tag_name": TAG,
            "draft": True,
            "assets": [],
            "author": {"login": "github-actions[bot]"},
            "target_commitish": SHA,
        }
        for login, sha in [("maintainer", SHA), ("github-actions[bot]", "d" * 40)]:
            with self.subTest(login=login, sha=sha):
                self.draft.update(author={"login": login}, target_commitish=sha)
                with self.assertRaises(release.ReleaseError):
                    release.main()
        self.draft.update(author={"login": "github-actions[bot]"}, target_commitish=SHA)
        self.tag_ref = {"ref": REF, "object": {"type": "tag", "sha": TAG_OBJECT}}
        self.tag_commit = "c" * 40
        with self.assertRaises(release.ReleaseError):
            release.main()
        self.tag_ref["object"] = {"type": "commit", "sha": SHA}
        with self.assertRaises(release.ReleaseError):
            release.main()
        self.draft = None
        self.tag_ref["object"] = {"type": "tag", "sha": TAG_OBJECT}
        self.tag_commit = SHA
        with self.assertRaises(release.ReleaseError):
            release.main()
        self.assertTrue(all(method == "GET" for method, _ in self.operations))
