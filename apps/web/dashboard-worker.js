// The build supplies shellCache and shellFiles from the static distribution.
self.addEventListener("install", (event) => {
  event.waitUntil(
    (async () => {
      const cache = await caches.open(shellCache);
      await cache.addAll(
        shellFiles.map(
          (file) =>
            new Request(file, {
              credentials: "omit",
              cache: "reload",
              redirect: "error",
            }),
        ),
      );
      await self.skipWaiting();
    })(),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      const names = await caches.keys();
      await Promise.all(
        names
          .filter(
            (name) =>
              name.startsWith("metiva-dashboard-") && name !== shellCache,
          )
          .map((name) => caches.delete(name)),
      );
      await self.clients.claim();
    })(),
  );
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  const url = new URL(request.url);
  if (
    request.method !== "GET" ||
    url.origin !== self.location.origin ||
    url.search
  )
    return;
  const file =
    url.pathname === "/" && request.mode === "navigate"
      ? "/index.html"
      : url.pathname;
  if (!shellFiles.includes(file)) return;
  event.respondWith(
    (async () => {
      const cache = await caches.open(shellCache);
      const cached = await cache.match(file);
      if (!cached) return fetch(request);
      if (file === "/index.html" && request.mode === "navigate") {
        // A reachable ingress may serve a newer build or a rollback. Only
        // use the installed shell when the navigation cannot reach it.
        try {
          const response = await fetch(request, {
            cache: "no-store",
            signal: AbortSignal.timeout(4000),
          });
          if (response.status < 500) return response;
        } catch {
          // Offline, or the ingress did not respond in time.
        }
      }
      return cached;
    })(),
  );
});
