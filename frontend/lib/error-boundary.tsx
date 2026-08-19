"use client";

import { Component, ReactNode } from "react";

interface Props {
  children: ReactNode;
  /** Shown instead of the default panel, for scoping a boundary to one region. */
  fallback?: (error: Error, reset: () => void) => ReactNode;
  /** Human-readable name of the region, used in the default message. */
  label?: string;
}

interface State {
  error: Error | null;
}

/**
 * Catches render-time exceptions so one broken region cannot blank the page.
 *
 * React only routes errors thrown during render, lifecycle, or constructors
 * here. Rejected promises from TanStack Query surface as `error` on the query
 * result instead, so query failures are handled at the call site; this boundary
 * is the net for genuine render bugs, such as an event payload whose shape the
 * renderer did not expect.
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: { componentStack?: string | null }) {
    // Local-first app: the console is the operator's log.
    console.error("Render error caught by boundary:", error, info.componentStack);
  }

  private reset = () => this.setState({ error: null });

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;

    if (this.props.fallback) return this.props.fallback(error, this.reset);

    const region = this.props.label ?? "this view";
    return (
      <div
        role="alert"
        className="m-4 rounded-lg border border-red-200 bg-red-50 p-4 text-red-900"
      >
        <p className="font-medium">Something went wrong in {region}.</p>
        <p className="mt-1 text-sm break-words">{error.message}</p>
        <div className="mt-3 flex gap-2">
          <button
            type="button"
            onClick={this.reset}
            className="rounded border border-red-300 bg-white px-3 py-1.5 text-sm font-medium hover:bg-red-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-500"
          >
            Try again
          </button>
          <button
            type="button"
            onClick={() => window.location.reload()}
            className="rounded border border-red-300 px-3 py-1.5 text-sm font-medium hover:bg-red-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-500"
          >
            Reload page
          </button>
        </div>
      </div>
    );
  }
}
