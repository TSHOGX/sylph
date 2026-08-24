import type { Metadata, Viewport } from "next";
import "./globals.css";
import { Providers } from "@/lib/providers";
import { AppShell } from "@/components/app-shell";
import { SkipLink } from "@/components/skip-link";
import { RegisterServiceWorker } from "@/lib/register-sw";
import { THEME_BOOTSTRAP } from "@/lib/theme";

export const metadata: Metadata = {
  title: "Sylph Web",
  description: "Local-first UI for multi-agent coding sessions",
  applicationName: "Sylph",
  /**
   * iOS reads these rather than the manifest to decide how an installed app
   * behaves. Without `capable`, "Add to Home Screen" produces a bookmark that
   * opens in Safari with its chrome — which looks like the install silently did
   * nothing. `statusBarStyle: "default"` keeps the status bar legible in both
   * themes, where a fixed `black` would be invisible against the dark canvas.
   */
  appleWebApp: {
    capable: true,
    title: "Sylph",
    statusBarStyle: "default",
  },
  /**
   * `apple-touch-icon` is declared here rather than left to convention: iOS does
   * not read the manifest for it, and without an explicit link it screenshots the
   * page and uses that as the home-screen icon.
   */
  icons: {
    icon: [
      { url: "/icons/icon.svg", type: "image/svg+xml" },
      { url: "/favicon.ico", sizes: "any" },
    ],
    apple: { url: "/icons/apple-touch-icon.png", sizes: "180x180" },
  },
  /** No indexing: this binds to loopback and serves one user's private work. */
  robots: { index: false, follow: false },
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  /**
   * Zoom stays enabled. Pinch-zoom is an accessibility affordance (WCAG 1.4.4),
   * and the usual reason to disable it — stopping iOS from zooming when a small
   * input takes focus — is handled properly instead, by floored input text size in
   * `globals.css` and `text-base` on the composer's textarea.
   */
  maximumScale: 5,
  /**
   * Draw into the notch and home-indicator areas; the `*-safe` utilities pad the
   * chrome back out. Without this the page is letterboxed on a notched phone and
   * the app has grey bars where a native app has content.
   */
  viewportFit: "cover",
  /**
   * The on-screen keyboard shrinks the layout viewport rather than sliding the page
   * up. That is what keeps `100dvh` honest while the keyboard is open: the composer
   * stays pinned above it, and the transcript above that, instead of the whole shell
   * scrolling out of view — the same class of bug as the `100vh` mismatch.
   */
  interactiveWidget: "resizes-content",
  /**
   * Paints the browser's own chrome to match the app's canvas. Media-matched rather
   * than a single colour: a light bar above a dark app is the seam that makes an
   * installed PWA read as a web page. Values are the resolved `--canvas` tokens.
   */
  themeColor: [
    { media: "(prefers-color-scheme: light)", color: "#ffffff" },
    { media: "(prefers-color-scheme: dark)", color: "#14181f" },
  ],
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        {/*
          Resolves the theme onto `<html>` before the first paint.

          It has to be inline and blocking. A React effect runs after paint by
          definition, so a dark-mode user would see a white page for the length of
          hydration — long enough on a cold load to read as a flash of the wrong
          app. `suppressHydrationWarning` on `<html>` above is required because this
          script mutates the very attribute React is about to reconcile.
        */}
        <script dangerouslySetInnerHTML={{ __html: THEME_BOOTSTRAP }} />
      </head>
      <body className="antialiased">
        {/*
          Visible only while focused, so a keyboard user can jump past the header on
          every page instead of tabbing through it each time. Each page gives its
          <main> the matching id.

          Its own client component because the locale lives in `localStorage` and this
          layout is a server component — and a skip link is the one control whose
          entire purpose is to be read aloud, so an untranslatable one is the worst
          place to leave a hardcoded string.
        */}
        <SkipLink />
        {/*
          The shell lives in the layout, not in each page: sidebars must keep
          their state and scroll position across navigation, which a shell
          mounted inside a page cannot do because it remounts on every route
          change. `<main id="main">` is provided by the shell, so pages no
          longer declare their own.
        */}
        <RegisterServiceWorker />
        <Providers>
          <AppShell>{children}</AppShell>
        </Providers>
      </body>
    </html>
  );
}
