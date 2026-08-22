/**
 * Service worker.
 *
 * # What this does, and deliberately does not do
 *
 * It exists for two reasons, neither of which is offline capability:
 *
 * 1. **Installability.** Chrome on Android requires a service worker with a `fetch`
 *    handler before it will offer "Install app". Without one, the manifest and
 *    icons produce a bookmark, not an installed app.
 * 2. **An honest offline screen.** A navigation with the backend down otherwise
 *    lands on the browser's own dinosaur, which tells a user nothing about *this*
 *    app. The fallback below says the backend is not running, which is the actual
 *    problem and the actual fix.
 *
 * It caches almost nothing on purpose. Sylph Web is local-first but not
 * backend-independent: every screen needs `localhost:4317`, so a cached shell
 * without it is a shell that cannot do anything. Aggressive precaching would buy no
 * working offline mode and cost a well-known failure — a stale HTML document
 * pinning an old JS chunk hash after an upgrade, which presents as a blank page
 * that a reload does not fix.
 *
 * # `/api/v1/*` is never intercepted
 *
 * Two independent reasons, and either alone is sufficient:
 *
 * - **SSE.** The turn event stream is a long-lived `text/event-stream`. Passing a
 *   streaming response through `fetch()` inside a worker risks buffering it, and a
 *   buffered SSE stream delivers a whole turn at once — the exact class of bug the
 *   `no-transform` header elsewhere exists to prevent, and one that presents as a
 *   rendering fault rather than a transport one.
 * - **Auth.** The session cookie is `SameSite=Strict` and its token rotates on every
 *   backend restart. A cached API response could outlive the token that authorised
 *   it and be served to a client that is no longer authenticated.
 *
 * Requests this worker does not handle fall through to the network untouched, which
 * is what "no `respondWith`" means — not a pass-through `fetch()`.
 */

/**
 * Bumping this drops the old cache. It only holds the offline document, so the cost
 * of a bump is one fetch.
 */
const CACHE = "sylph-shell-v1";
const OFFLINE_URL = "/offline.html";

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((cache) => cache.add(new Request(OFFLINE_URL, { cache: "reload" })))
      // `skipWaiting` so an updated worker takes over on the next navigation rather
      // than waiting for every tab to close. Safe here precisely because nothing
      // versioned is cached: there is no old-chunk/new-document mismatch to create.
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(keys.filter((key) => key !== CACHE).map((key) => caches.delete(key))),
      )
      // Claim existing tabs, so the offline fallback works in the tab that
      // registered the worker without a reload.
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const { request } = event;

  // Only document navigations are handled. Everything else — assets, and every
  // `/api/v1/*` call including the SSE stream — is left entirely alone.
  if (request.mode !== "navigate") return;

  event.respondWith(
    (async () => {
      try {
        // Network-first, always. The document is what pins asset hashes, so serving
        // a cached one is how a PWA gets stuck on a previous deploy.
        return await fetch(request);
      } catch {
        // Reached only when the network itself failed, which for a loopback app
        // means the server is not running.
        const cache = await caches.open(CACHE);
        const offline = await cache.match(OFFLINE_URL);
        return (
          offline ??
          new Response("Sylph Web is offline.", {
            status: 503,
            headers: { "Content-Type": "text/plain; charset=utf-8" },
          })
        );
      }
    })(),
  );
});
