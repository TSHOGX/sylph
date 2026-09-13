"use client";

import {
  QueryClient,
  QueryClientProvider,
  useQuery,
} from "@tanstack/react-query";
import { ReactNode, useEffect, useState, useSyncExternalStore } from "react";
import { ApiError, api } from "@/lib/api";
import { ErrorBoundary } from "@/lib/error-boundary";
import { useSettings } from "@/lib/settings";
import { applyTheme, watchSystemTheme } from "@/lib/theme";
import { applyLocale, useT } from "@/lib/i18n";
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
  // `useT` reads the locale from the settings store rather than from a provider, so
  // it works here — outside the auth gate and before any session exists. The two
  // screens below are the first thing a user ever sees, and they were the only ones
  // still hardcoded to English.
  const t = useT();
  // Logout is a deliberate local state.  The bootstrap endpoint is intentionally
  // unauthenticated for first-run development, so calling it after logout would
  // immediately create a new Bootstrap identity and make "Sign out" meaningless.
  // sessionStorage keeps this choice scoped to this browser tab and disappears
  // when the tab is closed, which is the least surprising local-first behaviour.
  const signedOut = useSyncExternalStore(
    () => () => undefined,
    () => window.sessionStorage.getItem("sylph:signed-out") === "1",
    () => false,
  );
  const [signedOutOverride, setSignedOutOverride] = useState<boolean | null>(null);
  const effectiveSignedOut = signedOutOverride ?? signedOut;
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
    enabled: !effectiveSignedOut,
    // The cookie lasts as long as the backend process; no need to refetch.
    staleTime: Infinity,
    retry: 1,
  });

  if (effectiveSignedOut) {
    return <SignedOutScreen onAuthenticated={() => {
      window.sessionStorage.removeItem("sylph:signed-out");
      setSignedOutOverride(false);
    }} />;
  }

  // A backend restart rotates the process cookie. Once a registered Subject
  // exists the bootstrap endpoint correctly answers 401, so show the same
  // explicit sign-in surface instead of mislabelling it as an outage.
  if (error instanceof ApiError && error.status === 401) {
    return <SignedOutScreen onAuthenticated={() => {
      window.sessionStorage.removeItem("sylph:signed-out");
      setSignedOutOverride(false);
    }} />;
  }

  if (isPending) {
    return (
      <output
        aria-live="polite"
        className="flex h-dvh items-center justify-center text-sm text-ink-muted"
      >
        {t("app.connecting")}
      </output>
    );
  }

  if (error) {
    return (
      <div role="alert" className="flex h-dvh items-center justify-center px-4">
        <div className="max-w-md text-center">
          <h1 className="text-lg font-semibold text-danger">
            {t("app.backendUnreachableTitle")}
          </h1>
          <p className="mt-2 text-sm text-ink-muted">
            {t("app.backendUnreachableBody")}{" "}
            <code className="rounded bg-hover px-1">localhost:4317</code>.
          </p>
          <pre className="mt-3 overflow-x-auto rounded-lg bg-sunken p-3 text-left text-xs text-ink-soft">
            cd crates/server &amp;&amp; cargo run
          </pre>
          <Button
            variant="primary"
            size="md"
            onClick={() => window.location.reload()}
            className="mt-4"
          >
            {t("app.retry")}
          </Button>
        </div>
      </div>
    );
  }

  return <>{children}</>;
}

/** The only screen reachable after an explicit logout. */
function SignedOutScreen({ onAuthenticated }: { onAuthenticated: () => void }) {
  const t = useT();
  const [name, setName] = useState("");
  const [token, setToken] = useState("");
  const [issuedToken, setIssuedToken] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function authenticate(action: "register" | "login") {
    setBusy(true);
    setError(null);
    try {
      if (action === "register") {
        const subject = await api.auth.register(name.trim());
        setIssuedToken(subject.token);
        setName("");
      } else {
        await api.auth.login(token.trim());
        setToken("");
        onAuthenticated();
      }
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : t("settings.account.authFailed"));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex h-dvh items-center justify-center px-4">
      <section className="w-full max-w-sm space-y-4 rounded-lg border border-line bg-raised p-5 shadow-sm" aria-labelledby="signed-out-title">
        <div>
          <h1 id="signed-out-title" className="text-lg font-semibold">{t("settings.account.signedOutTitle")}</h1>
          <p className="mt-1 text-sm text-ink-muted">{t("settings.account.signedOutDescription")}</p>
        </div>
        <div className="space-y-2">
          <label className="block text-xs font-medium" htmlFor="signed-out-name">{t("settings.account.displayName")}</label>
          <div className="flex gap-2">
            <input id="signed-out-name" value={name} onChange={(event) => setName(event.target.value)} className="min-w-0 flex-1 rounded border border-line bg-raised px-2 py-1 text-sm focus-ring" />
            <Button variant="primary" size="md" disabled={!name.trim() || busy} onClick={() => void authenticate("register")}>{t("settings.account.register")}</Button>
          </div>
        </div>
        <div className="space-y-2">
          <label className="block text-xs font-medium" htmlFor="signed-out-token">{t("settings.account.token")}</label>
          <div className="flex gap-2">
            <input id="signed-out-token" type="password" value={token} onChange={(event) => setToken(event.target.value)} className="min-w-0 flex-1 rounded border border-line bg-raised px-2 py-1 text-sm focus-ring" />
            <Button variant="secondary" size="md" disabled={!token.trim() || busy} onClick={() => void authenticate("login")}>{t("settings.account.signIn")}</Button>
          </div>
        </div>
        {issuedToken && (
          <div className="space-y-2">
            <p role="status" className="text-xs text-ink-muted">{t("settings.account.tokenSaved")} <code className="select-all break-all">{issuedToken}</code></p>
            <Button variant="primary" size="md" onClick={onAuthenticated}>{t("settings.account.continue")}</Button>
          </div>
        )}
        {error && <p role="alert" className="text-xs text-danger">{error}</p>}
      </section>
    </div>
  );
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
