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
      return (await cache.match(file)) || fetch(request);
    })(),
  );
});
