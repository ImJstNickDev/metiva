"""Black-box acceptance checks, run through the test HTTPS proxy."""

import hashlib
import http.client
import json
import os
from pathlib import Path
import secrets
import ssl
import sys
import time

ROOT = Path("/fixture")
ORIGIN = "http://93.184.216.34:8081"
SESSION = ROOT / ".test-session"
state = json.loads(SESSION.read_text()) if SESSION.exists() else {}
tls = ssl.create_default_context(cafile="/tls/cert")


def request(
    path,
    method="GET",
    body=None,
    *,
    admin=False,
    key=None,
    headers=None,
    status=(200,),
    raw=False,
):
    hdr = dict(headers or {})
    if admin:
        hdr.update({"Cookie": state["cookie"], "X-CSRF-Token": state["csrf"]})
    if key:
        hdr["Authorization"] = "Bearer " + key
    if body is not None:
        hdr["Content-Type"] = "application/json"
    conn = http.client.HTTPSConnection("mirror.test", context=tls, timeout=15)
    conn.request(
        method, path, body=json.dumps(body) if body is not None else None, headers=hdr
    )
    res = conn.getresponse()
    data = res.read()
    cookie = res.getheader("set-cookie")
    conn.close()
    assert res.status in status, (method, path, res.status, data[:400])
    if cookie:
        state["cookie"] = cookie.split(";")[0]
    if raw:
        return data
    if data:
        return json.loads(data)


def admin(path, method="GET", body=None):
    return request(
        "/api/admin/" + path, method, body, admin=True, status=(200, 201, 202, 204)
    )


def manifest(names, broken=False):
    artifacts = []
    for name in names:
        data = (ROOT / name).read_bytes()
        artifacts.append(
            {
                "filename": name,
                "url": ORIGIN + "/" + name,
                "size": len(data),
                "sha256": "0" * 64 if broken else hashlib.sha256(data).hexdigest(),
            }
        )
    doc = {"releases": [{"version": "winter-01", "artifacts": artifacts}]}
    (ROOT / "source.json").write_text(json.dumps(doc))
    return doc


def wait(check, msg, timeout=100):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = check()
        if value:
            return value
        time.sleep(0.25)
    raise AssertionError(msg)


def sync(expected="completed", runtime=None):
    runtime = runtime or state["runtime"]
    job = admin(f"runtimes/{runtime}/sync", "POST", {})

    wait_job(runtime, job["id"], {expected})


def wait_job(runtime, job, expected, timeout=100):
    def done():
        records = admin(f"runtimes/{runtime}")["history"]
        row = next(r for r in records if r["id"] == job)
        if row["state"] in {"failed", "completed", "cancelled"}:
            assert row["state"] in expected, row
            return row

    return wait(done, "sync never reached its expected terminal state", timeout)


def wait_transfer(job, predicate):
    def matching():
        return next(
            (
                row
                for row in admin("transfers")
                if row["job_id"] == job and predicate(row)
            ),
            None,
        )

    return wait(matching, f"transfer for job {job} never reached expected state")


def control_feed(filename):
    doc = {
        "releases": [{"version": "v1", "artifacts": [{"url": ORIGIN + "/" + filename}]}]
    }
    (ROOT / "controls.json").write_text(json.dumps(doc))


def transfer_controls():
    admin(
        "settings",
        "PATCH",
        {
            "engine": "native",
            "concurrency": 2,
            "per_host": 2,
            "retries": 0,
            "sync_interval": 3600,
        },
    )
    runtime = admin(
        f"projects/{state['project']}/runtimes",
        "POST",
        {"name": "Transfer controls", "slug": "controls", "visibility": "public"},
    )["id"]
    rt = f"runtimes/{runtime}"
    base = "/p/atlas/r/controls"
    control_feed("beta.bin")
    admin(rt + "/sources", "POST", {"url": ORIGIN + "/controls.json"})
    sync(runtime=runtime)

    def start(filename):
        control_feed(filename)
        return admin(rt + "/sync", "POST", {})["id"]

    def downloading(t):
        return t["state"] == "downloading" and t["bytes"] > 0

    # Neither manifest advertises a checksum. The origin validators alone identify the
    # change while paused, so a mixed old prefix and new suffix would fail this check.
    old = b"A" * (8 << 20)
    new = b"B" * len(old)
    (ROOT / "mutable.bin").write_bytes(old)
    job = start("mutable.bin")
    t = wait_transfer(job, downloading)
    endpoint = f"transfers/{t['id']}"
    admin(endpoint + "/pause", "POST", {})
    paused = wait_transfer(job, lambda t: t["state"] == "paused")
    time.sleep(0.6)
    still = wait_transfer(job, lambda t: t["state"] == "paused")
    assert paused["bytes"] == still["bytes"] and 0 < paused["bytes"] < len(old)
    (ROOT / "mutable.bin").write_bytes(new)
    admin(endpoint + "/resume", "POST", {})
    wait_job(runtime, job, {"completed"})
    seen = json.loads((ROOT / "mutable-range.json").read_text())
    assert seen["if_range"] == '"' + hashlib.sha256(old).hexdigest() + '"'
    assert seen["status"] == 200 and seen["current"] != seen["if_range"]
    assert request(base + "/files/v1/mutable.bin", raw=True) == new
    last_good = admin(rt)["active_publication"]

    payload = b"C" * (4 << 20)
    (ROOT / "cancel.bin").write_bytes(payload)
    job = start("cancel.bin")
    t = wait_transfer(job, downloading)
    endpoint = f"transfers/{t['id']}"
    admin(endpoint + "/cancel", "POST", {})
    wait_transfer(job, lambda t: t["state"] == "cancelled")
    wait_job(runtime, job, {"cancelled", "failed"})
    assert admin(rt)["active_publication"] == last_good
    admin(endpoint + "/retry", "POST", {})
    replacement = wait(
        lambda: next((j for j in admin(rt)["history"] if j["id"] > job), None),
        "Retry did not create a new sync",
    )
    wait_job(runtime, replacement["id"], {"completed"})
    assert request(base + "/files/v1/cancel.bin", raw=True) == payload
    last_good = admin(rt)["active_publication"]

    # With automatic retries off, a read timeout must fail this candidate while the
    # previous publication and its verified bytes continue to be served.
    (ROOT / "stall.bin").write_bytes(b"D" * 65536)
    (ROOT / "behavior.json").write_text(json.dumps({"/stall.bin": "stall"}))
    job = start("stall.bin")
    wait(
        lambda: (ROOT / "stall-started").exists(),
        "Fixture never began the stalled response",
    )
    wait_job(runtime, job, {"failed"}, timeout=70)
    failure = wait_transfer(job, lambda t: t["state"] == "failed")
    error = (failure["error"] or "").lower()
    assert "timeout" in error or "timed out" in error, failure
    assert failure["retries"] == 0, failure
    assert admin(rt)["active_publication"] == last_good
    assert request(base + "/files/v1/cancel.bin", raw=True) == payload


def nested_manifests():
    def write(name, value):
        data = json.dumps(value).encode()
        (ROOT / name).write_bytes(data)
        return {
            "url": "/" + name,
            "size": len(data),
            "sha1": hashlib.sha1(data).hexdigest(),
        }

    def artifact(name):
        data = (ROOT / name).read_bytes()
        return {
            "url": "/" + name,
            "size": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
        }

    payload = write("payload.json", {"translation": "Document contents"})
    objects = {
        name: {"hash": payload["sha1"], "size": payload["size"]}
        for name in ["labels/a/entry.json", "labels/b/entry.json"]
    }
    resources = write("resources.json", {"objects": objects})
    descriptor = write(
        "package.json",
        {"packages": {"portable": artifact("beta.bin")}, "inventory": resources},
    )
    records = [
        {"version": "candidate-1", "track": "early", **descriptor},
        {"version": "candidate-2", "track": "early", **descriptor},
        {"version": "unselected", "track": "stable", "url": "/missing.json"},
    ]
    write("records.json", {"records": records})
    runtime = admin(
        f"projects/{state['project']}/runtimes",
        "POST",
        {"name": "Documents", "slug": "documents", "visibility": "private"},
    )["id"]
    rt = f"runtimes/{runtime}"
    source = {"url": ORIGIN + "/records.json"}

    def inspect(**options):
        return admin(rt + "/inspect-document", "POST", {**source, **options})

    collection = next(c for c in inspect()["collections"] if c["path"] == "/records")
    assert collection["count"] == 3 and collection["version_path"] == "/version"
    filters = [{"path": "/track", "values": ["early"]}]
    sample = inspect(collection="/records", filters=filters)
    assert sample["sample"]["id"] == "candidate-1"
    pointer = next(b for b in sample["branches"] if b["path"] == "")["sample_pointer"]
    branches = inspect(trail=[pointer])["branches"]
    edge = next(b for b in branches if b["path"] == "/inventory")
    leaves = inspect(trail=[pointer, edge["sample_pointer"]])["branches"]
    assert any(b["kind"] == "digest" and b["path"] == "/objects/*" for b in leaves)
    rules = [
        {
            "kind": "document",
            "path": "",
            "rules": [
                {"kind": "file", "path": "/packages/portable"},
                {
                    "kind": "document",
                    "path": "/inventory",
                    "rules": [
                        {
                            "kind": "file",
                            "path": "/objects/*",
                            "name_path": "@key",
                            "digest_path": "/hash",
                            "algorithm": "sha1",
                            "size_path": "/size",
                            "url_template": ORIGIN + "/pool/{prefix}/{digest}",
                        }
                    ],
                },
            ],
        }
    ]
    source["mapping"] = {
        "nested": {
            "path": "/records",
            "version_path": "/version",
            "filters": filters,
            "limit": 5,
            "rules": rules,
        }
    }
    preview = admin(rt + "/preview", "POST", source)
    assert [len(r["artifacts"]) for r in preview["releases"]] == [3, 3], preview
    admin(rt + "/sources", "POST", source)
    sync(runtime=runtime)
    view = admin(rt)
    assert len(view["originals"]) == 3, view["originals"]
    key = admin(
        "keys", "POST", {"name": "Documents", "scope": "runtime", "runtimeId": runtime}
    )["token"]
    endpoint = "/p/atlas/r/documents/manifest.json"
    request(endpoint, status=(401,))
    manifest = request(endpoint, key=key)
    files = manifest["releases"][0]["artifacts"]
    logical = [f for f in files if f.get("logical_path")]
    assert {f["logical_path"] for f in logical} == set(objects)
    assert len({f["filename"] for f in logical}) == 2
    for f in logical:
        url = f["url"].removeprefix("https://mirror.test")
        request(url, status=(401,))
        assert request(url, key=key, raw=True) == (ROOT / "payload.json").read_bytes()
    # A broken pointer holds back its release while a sibling can publish.
    records[0]["sha1"] = "0" * 40
    records[1]["version"] = "candidate-3"
    write("records.json", {"records": records})
    sync("failed", runtime=runtime)
    assert {r["version"] for r in request(endpoint, key=key)["releases"]} == {
        "candidate-1",
        "candidate-2",
        "candidate-3",
    }
    view = admin(rt)
    assert view["history"][0]["verified_releases"] == 1
    assert view["history"][0]["failed_releases"] == 1


def save():
    SESSION.write_text(json.dumps(state))
    os.chmod(SESSION, 0o600)


def full_history_before_restart():
    releases = []
    for i in range(925):
        number = f"{i:04}"
        payload = ("build-" + number).encode()
        (ROOT / f"history-{number}.bin").write_bytes(payload)
        document = {
            "file": {
                "url": f"{number}.bin",
                "sha256": hashlib.sha256(payload).hexdigest(),
            },
            "metadata": "x" * 39000,
        }
        (ROOT / f"history-{number}.json").write_text(json.dumps(document))
        releases.append({"id": number, "url": f"/history/{number}.json"})
    (ROOT / "history.json").write_text(json.dumps({"releases": releases}))
    (ROOT / "history-delay").touch()
    runtime = admin(
        f"projects/{state['project']}/runtimes",
        "POST",
        {"name": "Full history", "slug": "history", "visibility": "private"},
    )["id"]
    rt = f"runtimes/{runtime}"
    source = {
        "url": ORIGIN + "/history.json",
        "mapping": {
            "nested": {
                "path": "/releases",
                "version_path": "/id",
                "rules": [
                    {
                        "path": "",
                        "kind": "document",
                        "rules": [{"path": "/file", "kind": "file"}],
                    }
                ],
            }
        },
    }
    preview = admin(rt + "/preview", "POST", source)
    assert preview["selection"] == {"matched": 925, "previewed": 5}
    assert [r["version"] for r in preview["releases"]] == [f"{i:04}" for i in range(5)]
    assert len((ROOT / "history-reads").read_text().splitlines()) == 5
    admin(rt + "/sources", "POST", source)
    job = admin(rt + "/sync", "POST", {})["id"]
    state["history_runtime"] = runtime
    state["history_job"] = job
    save()
    wait(
        lambda: "0340" in (ROOT / "history-reads").read_text().splitlines(),
        "Worker never reached the middle of the catalogue",
    )
    record = next(j for j in admin(rt)["history"] if j["id"] == job)
    assert record["documents_read"] >= 340, record


def full_history_after_restart():
    runtime = state["history_runtime"]
    record = wait_job(runtime, state["history_job"], {"completed"}, timeout=240)
    assert record["verified_releases"] == 925 and record["failed_releases"] == 0, record
    # Each of these descriptors was recorded before interruption, outside both previews.
    reads = (ROOT / "history-reads").read_text().splitlines()
    assert all(reads.count(f"{i:04}") == 1 for i in range(5, 340))
    key = admin(
        "keys", "POST", {"name": "History", "scope": "runtime", "runtimeId": runtime}
    )["token"]
    base = "/p/atlas/r/history"
    releases = request(base + "/manifest.json", key=key)["releases"]
    assert len(releases) == 925
    for i, release in enumerate(releases):
        number = f"{i:04}"
        [artifact] = release["artifacts"]
        payload = ("build-" + number).encode()
        assert release["version"] == number
        assert artifact["sha256"] == hashlib.sha256(payload).hexdigest()
        if i in (0, 127, 128, 340, 924):
            assert (
                request(f"{base}/files/{number}/{number}.bin", key=key, raw=True)
                == payload
            )


base = "/p/atlas/r/linux"
published = base + "/manifest.json"
alpha = base + "/files/winter-01/alpha.bin"
if sys.argv[1] == "bootstrap":
    assert request("/api/admin/session")["setupRequired"]
    setup = request(
        "/api/admin/setup",
        "POST",
        {
            "username": "operator",
            "password": secrets.token_urlsafe(24),
            "bootstrapSecret": sys.stdin.read().strip(),
        },
        status=(200, 201),
    )
    state["csrf"] = setup["csrfToken"]
    project = admin(
        "projects", "POST", {"name": "Atlas", "slug": "atlas", "visibility": "public"}
    )
    state["project"] = project["id"]
    runtime = admin(
        f"projects/{project['id']}/runtimes", "POST", {"name": "Linux", "slug": "linux"}
    )
    state["runtime"] = runtime["id"]
    (ROOT / "alpha.bin").write_bytes(bytes(range(256)) * 32768)
    (ROOT / "beta.bin").write_bytes(b"Portable package\n")
    (ROOT / "gamma.bin").write_bytes(b"Bad checksum fixture\n")
    (ROOT / "behavior.json").write_text(
        json.dumps({"/alpha.bin": 429, "/beta.bin": 503})
    )
    manifest(["alpha.bin", "beta.bin"])
    src = {"url": ORIGIN + "/source.json"}
    rt = f"runtimes/{runtime['id']}"
    assert admin(rt + "/preview", "POST", src)["releases"][0]["version"] == "winter-01"
    admin(rt + "/sources", "POST", src)
    admin(rt + "/sync", "POST", {})
    wait(
        lambda: any(
            t["state"] == "downloading"
            and t["filename"] == "alpha.bin"
            and t["bytes"] > 0
            for t in admin("transfers")
        ),
        "fixture transfer did not start",
    )
    save()
elif sys.argv[1] == "mirror":
    wait(
        lambda: admin(f"runtimes/{state['runtime']}")["last_success"],
        "restart did not finish publication",
    )
    assert (ROOT / "resumed").exists(), "interrupted download did not resume"
    current = request(published)
    urls = [a["url"] for a in current["releases"][0]["artifacts"]]
    assert "https://mirror.test" + alpha in urls, urls
    assert request(
        alpha, headers={"Range": "bytes=4-18"}, status=(206,), raw=True
    ) == bytes(range(4, 19))
    original = request(base + "/upstream.json")
    assert all(
        a["url"].startswith("https://mirror.test/")
        for a in original["releases"][0]["artifacts"]
    )
    admin(f"runtimes/{state['runtime']}", "PATCH", {"visibility": "private"})
    request(published, status=(401,))

    def key(scope, **target):
        return admin("keys", "POST", dict(name=scope, scope=scope, **target))

    runtime = key("runtime", runtimeId=state["runtime"])
    state["key"] = runtime["token"]
    request(published, key=runtime["token"])
    request(alpha, headers={"Range": "bytes=1-2"}, status=(401,))
    assert request(
        alpha,
        key=runtime["token"],
        headers={"Range": "bytes=1-2"},
        status=(206,),
        raw=True,
    ) == bytes([1, 2])
    project = key("project", projectId=state["project"])
    glob = key("global")
    admin(
        f"projects/{state['project']}/runtimes",
        "POST",
        {"slug": "arm", "name": "ARM", "visibility": "private"},
    )
    info = "/api/v1/projects/atlas/runtimes/arm/availability"
    request(info, key=runtime["token"], status=(403,))
    request(info, key=project["token"])
    other = admin("projects", "POST", {"name": "Index", "slug": "index"})
    admin(f"projects/{other['id']}/runtimes", "POST", {"slug": "core", "name": "Core"})
    info = "/api/v1/projects/index/runtimes/core/availability"
    request(info, key=project["token"], status=(403,))
    request(info, key=glob["token"])
    new = admin(f"keys/{runtime['id']}/rotate", "POST", {})
    request(published, key=runtime["token"])
    admin(f"keys/{runtime['id']}", "DELETE")
    request(published, key=runtime["token"], status=(401,))
    state["key"] = new["token"]
    request("/api/admin/projects", key=new["token"], status=(401,))
    # Conditional 304 pages must reconstruct the complete active publication.
    runtime_view = lambda: admin(f"runtimes/{state['runtime']}")
    original_id = runtime_view()["active_publication"]
    sync()
    assert runtime_view()["active_publication"] == original_id
    (ROOT / "source.json").rename(ROOT / "outage.json")
    sync("failed")
    assert runtime_view()["active_publication"] == original_id
    (ROOT / "outage.json").rename(ROOT / "source.json")
    # Exercise the alternate downloader on a new physical object.
    admin(
        "settings",
        "PATCH",
        {
            "engine": "curl",
            "concurrency": 2,
            "per_host": 2,
            "retries": 0,
            "sync_interval": 3600,
        },
    )
    manifest(["gamma.bin"], broken=True)
    sync("failed")
    assert request(published, key=state["key"]) == current
    manifest(["gamma.bin", "beta.bin"])
    sync()
    assert any(
        t["backend"] == "curl"
        and t["filename"] == "gamma.bin"
        and t["state"] == "completed"
        for t in admin("transfers")
    )
    manifest(["beta.bin"])
    sync()
    assert {f["filename"] for f in runtime_view()["current_files"]} == {
        "alpha.bin",
        "beta.bin",
        "gamma.bin",
    }
    assert admin("archive") == []
    old = next(
        f for f in runtime_view()["current_files"] if f["filename"] == "alpha.bin"
    )
    assert (
        hashlib.sha256(request(alpha, key=state["key"], raw=True)).hexdigest()
        == old["digest"]
    )
    request("/_objects/" + old["digest"], status=(404,), raw=True)
    beta = base + "/files/winter-01/beta.bin"
    previous_bytes = request(beta, key=state["key"], raw=True)
    replacement = b"Corrected portable package"
    (ROOT / "beta.bin").write_bytes(replacement)
    manifest(["beta.bin"])
    sync()
    assert request(beta, key=state["key"], raw=True) == replacement
    file = next(
        f for f in runtime_view()["current_files"] if f["filename"] == "beta.bin"
    )
    revisions = admin(f"files/{file['id']}/history")["revisions"]
    assert [r["current"] for r in revisions] == [True, False], revisions
    assert request(revisions[1]["download_url"], admin=True, raw=True) == previous_bytes
    request(revisions[1]["download_url"], key=glob["token"], status=(401,))
    # Parking cuts off new requests immediately for every credential scope.
    proj = f"projects/{state['project']}"
    arm = next(r for r in admin(proj + "/runtimes") if r["slug"] == "arm")
    admin(f"runtimes/{arm['id']}/park", "POST", {})
    admin(proj + "/park", "POST", {})
    assert not any(
        p["slug"] == "atlas" for p in request("/api/v1/projects", key=glob["token"])
    )
    for token in (state["key"], project["token"], glob["token"]):
        request(published, key=token, status=(404,))
        request(beta, key=token, headers={"Range": "bytes=0-1"}, status=(404,))
    assert request(revisions[1]["download_url"], admin=True, raw=True) == previous_bytes
    admin(proj + "/restore", "POST", {})
    assert request(beta, key=state["key"], raw=True) == replacement
    request(
        "/api/v1/projects/atlas/runtimes/arm/availability",
        key=glob["token"],
        status=(404,),
    )
    assert [(row["kind"], row["id"]) for row in admin("archive")] == [
        ("runtime", arm["id"])
    ]
    # Paginated and uploaded sources retain independent provenance after merging.
    combined = admin(
        f"projects/{state['project']}/runtimes",
        "POST",
        {"slug": "combined", "name": "Combined", "visibility": "private"},
    )["id"]
    part1 = {
        "version": "a",
        "artifacts": [{"url": ORIGIN + "/alpha.bin", "sha256": old["digest"]}],
    }
    part2 = {"version": "b", "artifacts": [{"url": ORIGIN + "/beta.bin"}]}
    page_one = {"releases": [part1], "next": "/second.json"}
    page_two = {"releases": [part2]}
    (ROOT / "first.json").write_text(json.dumps(page_one))
    (ROOT / "second.json").write_text(json.dumps(page_two))
    admin(f"runtimes/{combined}/sources", "POST", {"url": ORIGIN + "/first.json"})
    sync(runtime=combined)
    admin(f"runtimes/{combined}/sources", "POST", {"upload": json.dumps(page_two)})
    sync(runtime=combined)
    combined_view = admin(f"runtimes/{combined}")
    assert len(combined_view["originals"]) == 3
    assert len(combined_view["manifest"]["releases"]) == 2
    (ROOT / "second.json").unlink()
    sync("failed", runtime=combined)
    assert (
        admin(f"runtimes/{combined}")["active_publication"]
        == combined_view["active_publication"]
    )
    # Discover an index and select one of two branches and one of two targets.
    guided = admin(proj + "/runtimes", "POST", {"slug": "guided", "name": "Guided"})[
        "id"
    ]
    guide = f"runtimes/{guided}"

    def entry(version):
        return {
            "version": version,
            "packages": {
                "portable": {"url": ORIGIN + "/beta.bin"},
                "desktop": {"url": ORIGIN + "/alpha.bin"},
            },
        }

    root = {
        "streams": {
            "early": {"first_page": "/first.json", "count": 1, "pages": 1},
            "daily": {"first_page": "/second.json", "count": 1, "pages": 1},
        }
    }
    (ROOT / "index.json").write_text(json.dumps(root))
    (ROOT / "first.json").write_text(json.dumps({"versions": [entry("early-01")]}))
    (ROOT / "second.json").write_text(json.dumps({"versions": [entry("daily-01")]}))
    src = {"url": ORIGIN + "/index.json"}
    discovered = admin(guide + "/discover", "POST", src)
    index = next(i for i in discovered["indexes"] if i["path"] == "/streams")
    index = {**index, "keys": ["daily"]}
    collections = admin(guide + "/discover", "POST", {**src, "index": index})[
        "collections"
    ]
    collection = next(c for c in collections if c["path"] == "/versions")
    assert (
        "portable"
        in next(g for g in collection["groups"] if g["path"] == "/packages")["keys"]
    )
    select = {
        "index": index,
        "path": collection["path"],
        "version_path": collection["version_path"],
        "groups": [{"path": "/packages", "keys": ["portable"]}],
    }
    source = {**src, "mapping": {"selection": select}}
    preview = admin(guide + "/preview", "POST", source)
    assert [r["version"] for r in preview["releases"]] == ["daily-01"]
    admin(guide + "/sources", "POST", source)
    sync(runtime=guided)
    assert [f["filename"] for f in admin(guide)["current_files"]] == ["beta.bin"]
    (ROOT / "second.json").write_text(json.dumps({"versions": [entry("daily-02")]}))
    sync(runtime=guided)
    assert {f["release"] for f in admin(guide)["current_files"]} == {
        "daily-01",
        "daily-02",
    }
    # A newly advertised channel is opt-in. If a selected channel disappears
    # entirely, its existing files stay available until the mirror is parked.
    root["streams"]["next"] = root["streams"].pop("daily")
    (ROOT / "index.json").write_text(json.dumps(root))
    sync(runtime=guided)
    assert {f["release"] for f in admin(guide)["current_files"]} == {
        "daily-01",
        "daily-02",
    }
    root["streams"]["daily"] = root["streams"].pop("next")
    root["streams"]["daily"]["count"] = 2
    (ROOT / "index.json").write_text(json.dumps(root))
    rejected = entry("daily-bad")
    rejected["packages"]["portable"]["sha256"] = "0" * 64
    (ROOT / "second.json").write_text(
        json.dumps({"versions": [entry("daily-03"), rejected]})
    )
    sync("failed", runtime=guided)
    view = admin(guide)
    assert {f["release"] for f in view["current_files"]} == {
        "daily-01",
        "daily-02",
        "daily-03",
    }
    assert (
        view["history"][0]["verified_releases"],
        view["history"][0]["failed_releases"],
    ) == (1, 1)
    save()
elif sys.argv[1] == "nested":
    nested_manifests()
elif sys.argv[1] == "history-start":
    full_history_before_restart()
elif sys.argv[1] == "history-change":
    (ROOT / "history.json").write_text('{"releases":[]}')
    (ROOT / "history-delay").unlink()
elif sys.argv[1] == "history-finish":
    full_history_after_restart()
elif sys.argv[1] == "controls":
    transfer_controls()
elif sys.argv[1] == "restart":

    def healthy():
        try:
            return request("/api/admin/session", admin=True)["authenticated"]
        except (OSError, AssertionError, http.client.HTTPException):
            return False

    wait(healthy, "deployment failed to recover")
    assert len(request(published, key=state["key"])["releases"][0]["artifacts"]) == 3
    assert admin("archive")
