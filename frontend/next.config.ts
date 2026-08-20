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
const backend = process.env.SYLPH_WEB_BACKEND ?? "http://localhost:4317";

const config: NextConfig = {
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
