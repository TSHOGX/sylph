import type { NextConfig } from "next";

/**
 * Proxy `/api/v1/*` to the backend so the browser sees one origin.
 *
 * Same-origin matters beyond convenience: auth is a `SameSite=Strict` cookie,
 * which the browser would not send on a cross-origin request to another port.
 *
 * The target is configurable because the backend takes its port from
 * `SYLPH_WEB_PORT`; hardcoding 4317 makes the frontend silently unable to reach
 * a backend started on any other port.
 */
/**
 * Read at build time, not at request time: Next compiles rewrites into the
 * route manifest, so `SYLPH_WEB_BACKEND` must be set for `next build` and not
 * only for `next start`. Setting it only at start time leaves a production
 * build proxying to the default port.
 */
const backend = process.env.SYLPH_WEB_BACKEND ?? "http://localhost:4317";

/**
 * Dev origins from which `/_next/*` requests are allowed. Next 16 blocks
 * cross-site requests to dev resources as DNS-rebinding defense; this permits
 * requests from LAN IPs that reach the dev server directly.
 *
 * Comma-separated list of **hostnames only** (no protocol, no port):
 *   "100.125.12.35,192.168.1.100"
 *
 * Omit when `NODE_ENV=production`, for localhost-only development, and for
 * cpolar access that routes through a reverse proxy rewriting `Origin` to
 * `localhost`. localhost is always allowed.
 */
const allowedDevOrigins = process.env.SYLPH_WEB_DEV_ORIGINS
  ? process.env.SYLPH_WEB_DEV_ORIGINS.split(",")
      .map((s) => s.trim())
      .map((s) => {
        // Strip protocol and port if accidentally included
        try {
          const url = new URL(s.startsWith("http") ? s : `http://${s}`);
          return url.hostname;
        } catch {
          return s;
        }
      })
  : [];

const config: NextConfig = {
  /**
   * Emit a self-contained server bundle under `.next/standalone`, so an
   * install needs neither the repo nor `npm install` at runtime. The bundle
   * excludes `.next/static` and `public`, which `scripts/install.sh` copies in.
   */
  output: "standalone",
  // Next's proxy-request implementation defaults to 30s even in production.
  // Let the backend's native-read deadline return its typed error before the
  // proxy closes the socket; navigation still cancels obsolete reads immediately.
  experimental: { proxyTimeout: 330_000 },
  ...(allowedDevOrigins.length > 0 && { allowedDevOrigins }),
  async rewrites() {
    return [
      {
        source: "/api/v1/:path*",
        destination: `${backend}/api/v1/:path*`,
      },
    ];
  },
};

export default config;
