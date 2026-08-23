"use client";

import {
  QueryClient,
  QueryClientProvider,
  useQuery,
} from "@tanstack/react-query";
import { ReactNode, useEffect, useState } from "react";
import { ApiError } from "@/lib/api";
import { ErrorBoundary } from "@/lib/error-boundary";
import { useSettings } from "@/lib/settings";
import { applyTheme, watchSystemTheme } from "@/lib/theme";
import { applyLocale } from "@/lib/i18n";
import { Button } from "@/components/ui/button";

/**
 * Keeps `<html data-theme>` in step with the preference.
 *
 * The bootstrap script in `layout.tsx` has already set the attribute before this
 * mounts, so on a first load this re-applies the value that is already there — a
 * deliberate no-op. What it exists for is the two cases the script cannot cover: a
 * change made in the settings panel, and the OS flipping while a `system`
 * preference is in effect.
 *
 * Renders nothing. It is a component rather than a hook inside `Providers` so that
 * a settings change re-renders only this, not the whole application tree.
 */
function ThemeSync() {
  const { settings } = useSettings();
  const theme = settings.theme;

  useEffect(() => {
    applyTheme(theme);
    // Re-resolves on an OS change, and only while the preference is `system`.
    return watchSystemTheme(theme, () => applyTheme(theme));
  }, [theme]);

  return null;
}

/**
 * Keeps `<html lang>` in step with the language preference.
 *
 * Unlike the theme there is no pre-paint script for this, and none is needed: the
 * attribute is not visual, so setting it after hydration costs nothing a user can
 * see. What it does affect is assistive technology — a screen reader picks its
 * voice and pronunciation from `lang`, so a Chinese UI still labelled `en` is read
 * as English attempting Chinese. The document ships `lang="en"`, which is correct
 * until this runs.
 *
 * Separate from `ThemeSync` rather than merged into one `DocumentSync`: they are
 * driven by different preferences, and keeping them apart means a language change
 * does not re-run theme resolution or vice versa.
 */
function LocaleSync() {
  const locale = useSettings().settings.locale;

  useEffect(() => {
    applyLocale(locale);
  }, [locale]);

  return null;
}

/**
 * Fetch the session cookie before anything else runs.
 *
 * The backend generates a fresh token at startup and only sets it from
 * `GET /api/v1/auth/token`; every other route 401s without it. Nothing else
 * calls that endpoint, so without this gate a freshly loaded page fails every
 * request. Because the token rotates on each backend restart, this must run on
 * every page load, not once per browser.
 */
function AuthGate({ children }: { children: ReactNode }) {
  const { isPending, error } = useQuery({
    queryKey: ["auth", "token"],
    queryFn: async () => {
      const response = await fetch("/api/v1/auth/token", {
        credentials: "include",
      });
      if (!response.ok) {
        throw new ApiError(
          response.status,
          "AUTH_BOOTSTRAP_FAILED",
          "Could not obtain a session cookie from the backend.",
        );
      }
      return response.json();
    },
    // The cookie lasts as long as the backend process; no need to refetch.
    staleTime: Infinity,
    retry: 1,
  });

  if (isPending) {
    return (
      <output
        aria-live="polite"
        className="flex h-dvh items-center justify-center text-sm text-ink-muted"
      >
        Connecting to backend...
      </output>
    );
  }

  if (error) {
    return (
      <div role="alert" className="flex h-dvh items-center justify-center px-4">
        <div className="max-w-md text-center">
          <h1 className="text-lg font-semibold text-danger">Cannot reach the backend</h1>
          <p className="mt-2 text-sm text-ink-muted">
            Sylph Web needs the Rust backend running on{" "}
            <code className="rounded bg-hover px-1">localhost:4317</code>.
          </p>
          <pre className="mt-3 overflow-x-auto rounded-lg bg-sunken p-3 text-left text-xs text-ink-soft">
            cd backend &amp;&amp; cargo run
          </pre>
          <Button
            variant="primary"
            size="md"
            onClick={() => window.location.reload()}
            className="mt-4"
          >
            Retry
          </Button>
        </div>
      </div>
    );
  }

  return <>{children}</>;
}

export function Providers({ children }: { children: ReactNode }) {
  const [queryClient] = useState(
    () =>
      new QueryClient({
        defaultOptions: {
          queries: {
            staleTime: 5000,
            refetchOnWindowFocus: false,
            // One retry absorbs a transient blip; more would delay the error
            // state a local user is watching for.
            retry: (failureCount, error) => {
              // A 4xx will not become a 2xx on retry.
              if (error instanceof ApiError && error.status >= 400 && error.status < 500) {
                return false;
              }
              return failureCount < 1;
            },
          },
        },
      }),
  );

  return (
    <QueryClientProvider client={queryClient}>
      {/* Outside the auth gate: the theme has to be right on the "Connecting to
          backend..." screen and on the failure screen too, both of which render
          before the gate resolves. */}
      <ThemeSync />
      <LocaleSync />
      <ErrorBoundary label="Sylph Web">
        <AuthGate>{children}</AuthGate>
      </ErrorBoundary>
    </QueryClientProvider>
  );
}
