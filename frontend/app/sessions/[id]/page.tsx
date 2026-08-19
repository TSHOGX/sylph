"use client";

import { useQuery } from "@tantml:tanstack/react-query";
import { useParams } from "next/navigation";
import Link from "next/link";
import { api } from "@/lib/api";

export default function SessionPage() {
  const params = useParams();
  const sessionId = params.id as string;

  const { data, isLoading, error } = useQuery({
    queryKey: ["session", sessionId],
    queryFn: () => api.sessions.get(sessionId),
    refetchInterval: 5000, // Poll every 5s for status updates
  });

  if (isLoading) {
    return (
      <div className="min-h-screen flex items-center justify-center">
        <p className="text-muted-foreground">Loading session...</p>
      </div>
    );
  }

  if (error) {
    return (
      <div className="min-h-screen flex items-center justify-center">
        <div className="text-center">
          <p className="text-destructive font-medium">Failed to load session</p>
          <p className="text-sm text-muted-foreground mt-2">
            {error instanceof Error ? error.message : "Unknown error"}
          </p>
          <Link href="/" className="inline-block mt-4 text-primary hover:underline">
            Back to sessions
          </Link>
        </div>
      </div>
    );
  }

  const session = data?.session;

  return (
    <div className="min-h-screen bg-background">
      <header className="border-b">
        <div className="container mx-auto px-4 py-4">
          <Link href="/" className="text-primary hover:underline text-sm">
            ← Back to sessions
          </Link>
          <h1 className="text-2xl font-bold mt-2">{session?.title}</h1>
          <p className="text-sm text-muted-foreground mt-1">
            {session?.agent} • {session?.origin} •{" "}
            <span
              className={
                session?.status === "running"
                  ? "text-blue-600 font-medium"
                  : "text-gray-600"
              }
            >
              {session?.status}
            </span>
          </p>
        </div>
      </header>

      <main className="container mx-auto px-4 py-8">
        <div className="max-w-4xl mx-auto">
          <div className="text-center py-12 text-muted-foreground">
            <p>Conversation view coming soon</p>
            <p className="text-sm mt-2">
              SSE integration and turn rendering will be added in the next iteration
            </p>
          </div>
        </div>
      </main>
    </div>
  );
}
