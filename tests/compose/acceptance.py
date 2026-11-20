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


def sync(expected="completed"):
    job = admin(f"runtimes/{state['runtime']}/sync", "POST", {})

    def done():
        records = admin(f"runtimes/{state['runtime']}")["history"]
        row = next(r for r in records if r["id"] == job["id"])
        if row["state"] in ["failed", "completed", "cancelled"]:
            assert row["state"] == expected, row
            return True

    wait(done, "sync never reached terminal state")


def save():
    SESSION.write_text(json.dumps(state))
    os.chmod(SESSION, 0o600)


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
    request(alpha, key=state["key"], status=(404,))
    archives = admin("archive")
    old = next(
        f for pub in archives for f in pub["files"] if f["filename"] == "alpha.bin"
    )
    archived = request(f"/api/admin/archive/files/{old['id']}", admin=True, raw=True)
    assert hashlib.sha256(archived).hexdigest() == old["digest"]
    request("/_objects/" + old["digest"], status=(404,), raw=True)
    save()
elif sys.argv[1] == "restart":

    def healthy():
        try:
            return request("/api/admin/session", admin=True)["authenticated"]
        except (OSError, AssertionError, http.client.HTTPException):
            return False

    wait(healthy, "deployment failed to recover")
    assert len(request(published, key=state["key"])["releases"][0]["artifacts"]) == 1
    assert admin("archive")
