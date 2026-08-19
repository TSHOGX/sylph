import type { NextConfig } from "next";

const config: NextConfig = {
  async rewrites() {
    return [
      {
        source: "/api/v1/:path*",
        destination: "http://localhost:4317/api/v1/:path*",
      },
    ];
  },
};

export default config;
