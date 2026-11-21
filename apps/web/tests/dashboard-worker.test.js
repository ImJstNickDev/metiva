import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import { expect, it, vi } from "vitest";

it("precaches public shell files without credentials and bypasses private requests", async () => {
  const origin = "https://mirror.example";
  const shellFiles = ["/index.html", "/assets/dashboard.js"];
  const handlers = new Map();
  const storage = { addAll: vi.fn(), match: vi.fn(async (file) => file) };
  const open = vi.fn(async () => storage);
  runInNewContext(
    readFileSync(new URL("../dashboard-worker.js", import.meta.url), "utf8"),
    {
      self: {
        location: { origin },
        addEventListener: (name, fn) => handlers.set(name, fn),
        skipWaiting: vi.fn(),
      },
      caches: { open },
      shellFiles,
      shellCache: "metiva-dashboard-test",
      URL,
      Request: class extends Request {
        constructor(path, options) {
          super(new URL(path, origin), options);
        }
      },
    },
  );
  const waitUntil = vi.fn();
  handlers.get("install")({ waitUntil });
  await waitUntil.mock.calls[0][0];
  const requests = storage.addAll.mock.calls[0][0];
  expect(requests.map((r) => new URL(r.url).pathname)).toEqual(shellFiles);
  expect(
    requests.every((r) => r.credentials === "omit" && r.redirect === "error"),
  ).toBe(true);
  const fetchEvent = (path, method = "GET") => {
    const respondWith = vi.fn();
    handlers.get("fetch")({
      request: { url: new URL(path, origin).href, method, mode: "navigate" },
      respondWith,
    });
    return respondWith.mock.calls[0]?.[0];
  };
  expect(await fetchEvent("/")).toBe("/index.html");
  expect(await fetchEvent("/assets/dashboard.js")).toBe("/assets/dashboard.js");
  for (const path of [
    "/api/admin/session",
    "/api/admin/events",
    "/api/v1/projects",
    "/p/private/file",
    "/?token=private",
    "https://other.example/index.html",
  ]) {
    expect(fetchEvent(path)).toBeUndefined();
  }
  expect(fetchEvent("/", "POST")).toBeUndefined();
  expect(storage.addAll).toHaveBeenCalledTimes(1);
});
