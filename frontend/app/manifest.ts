import type { MetadataRoute } from "next";

/**
 * Web app manifest.
 *
 * This is what makes the app installable — "Add to Home Screen" on iOS, "Install
 * app" in Chrome — so a phone user gets an icon and a standalone window instead of
 * a browser tab they have to find again.
 *
 * A route handler rather than a static `manifest.json` so `theme_color` and
 * `background_color` can be kept in step with the `--canvas` token in
 * `globals.css` from one place in TypeScript. The referenced icons *are* static
 * files under `public/`, which matters for `output: "standalone"`: that bundle
 * omits `public/`, and `scripts/install.sh` copies it in.
 */
export default function manifest(): MetadataRoute.Manifest {
  return {
    name: "Sylph Web",
    /** Under ~12 characters, or the launcher truncates it. */
    short_name: "Sylph",
    description: "Local-first UI for multi-agent coding sessions",
    start_url: "/",
    /**
     * Bounds what opens inside the installed app. Loopback-only and single-origin,
     * so the whole origin is in scope.
     */
    scope: "/",
    /**
     * No browser chrome. `minimal-ui` would keep a URL bar, which for an app that
     * navigates entirely client-side is a row of pixels showing a path the user
     * cannot use — and it costs exactly the height the composer needs on a phone.
     */
    display: "standalone",
    /**
     * The colour painted before the app's own CSS applies, so a cold launch does
     * not flash white. This cannot be media-matched — the manifest holds one value
     * — so it is the light canvas, matching the light-mode default. Dark-mode users
     * see it only for the first frame; the pre-paint script in `layout.tsx` corrects
     * the page itself before anything is drawn.
     */
    background_color: "#ffffff",
    /** The system UI tint around the app window. Matches `--canvas` in light mode. */
    theme_color: "#ffffff",
    /** A phone app that only works in portrait is worse than one that rotates. */
    orientation: "any",
    /** Nothing here is a document to be indexed or shared. */
    categories: ["developer", "productivity"],
    icons: [
      /**
       * `any` and `maskable` are separate entries of the same size on purpose. A
       * maskable icon is cropped to whatever shape the launcher uses — a circle on
       * Android, a squircle on iOS — so its artwork must sit inside the safe zone
       * with bleed around it. Declaring one file as both means either the browser
       * crops a full-bleed icon and cuts the mark, or it shows a padded icon
       * unpadded and the mark floats small in a large box.
       */
      { src: "/icons/icon-192.png", sizes: "192x192", type: "image/png", purpose: "any" },
      { src: "/icons/icon-512.png", sizes: "512x512", type: "image/png", purpose: "any" },
      {
        src: "/icons/icon-maskable-512.png",
        sizes: "512x512",
        type: "image/png",
        purpose: "maskable",
      },
      /** Scales to any size; used where a browser prefers vector. */
      { src: "/icons/icon.svg", sizes: "any", type: "image/svg+xml", purpose: "any" },
    ],
  };
}
