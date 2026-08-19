"use client";

import {
  QueryClient,
  QueryClientProvider,
  useQuery,
} from "@tanstack/react-query";
import { ReactNode, useState } from "react";
import { ApiError } from "@/lib/api";
import { ErrorBoundary } from "@/lib/error-boundary";

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
        className="flex min-h-screen items-center justify-center text-sm text-gray-500"
      >
        Connecting to backend...
      </output>
    );
  }

  if (error) {
    return (
      <div
        role="alert"
        className="flex min-h-screen items-center justify-center px-4"
      >
        <div className="max-w-md text-center">
          <h1 className="text-lg font-semibold text-red-800">
            Cannot reach the backend
          </h1>
          <p className="mt-2 text-sm text-gray-600">
            Sylph Web needs the Rust backend running on{" "}
            <code className="rounded bg-gray-100 px-1">localhost:4317</code>.
          </p>
          <pre className="mt-3 overflow-x-auto rounded bg-gray-900 p-3 text-left text-xs text-gray-100">
            cd backend &amp;&amp; cargo run
          </pre>
          <button
            type="button"
            onClick={() => window.location.reload()}
            className="mt-4 rounded-lg bg-gray-900 px-4 py-2 text-sm font-medium text-white hover:bg-gray-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
          >
            Retry
          </button>
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
      <ErrorBoundary label="Sylph Web">
        <AuthGate>{children}</AuthGate>
      </ErrorBoundary>
    </QueryClientProvider>
  );
}
