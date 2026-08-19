"use client";

import { useQuery } from "@tanstack/react-query";
import Link from "next/link";
import { api } from "@/lib/api";

export default function HomePage() {
  const { data, isLoading, error } = useQuery({
    queryKey: ["sessions"],
    queryFn: () => api.sessions.list(),
  });

  if (isLoading) {
    return (
      <div className="min-h-screen flex items-center justify-center">
        <p className="text-muted-foreground">Loading sessions...</p>
      </div>
    );
  }

  if (error) {
    return (
      <div className="min-h-screen flex items-center justify-center">
        <div className="text-center">
          <p className="text-destructive font-medium">Failed to load sessions</p>
          <p className="text-sm text-muted-foreground mt-2">
            {error instanceof Error ? error.message : "Unknown error"}
          </p>
        </div>
      </div>
    );
  }

  const sessions = data?.sessions || [];

  return (
    <div className="min-h-screen bg-background">
      <header className="border-b">
        <div className="container mx-auto px-4 py-4 flex items-center justify-between">
          <h1 className="text-2xl font-bold">Sylph Web</h1>
          <Link
            href="/sessions/new"
            className="px-4 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition"
          >
            New Session
          </Link>
        </div>
      </header>

      <main className="container mx-auto px-4 py-8">
        {sessions.length === 0 ? (
          <div className="text-center py-12">
            <p className="text-muted-foreground">No sessions yet</p>
            <Link
              href="/sessions/new"
              className="inline-block mt-4 text-primary hover:underline"
            >
              Create your first session
            </Link>
          </div>
        ) : (
          <div className="grid gap-4">
            {sessions.map((session) => (
              <Link
                key={session.id}
                href={`/sessions/${session.id}`}
                className="block p-4 border rounded-lg hover:bg-accent transition"
              >
                <div className="flex items-start justify-between">
                  <div>
                    <h2 className="font-medium">{session.title}</h2>
                    <p className="text-sm text-muted-foreground mt-1">
                      {session.agent} • {session.origin}
                    </p>
                  </div>
                  <span
                    className={`text-xs px-2 py-1 rounded ${
                      session.status === "running"
                        ? "bg-blue-100 text-blue-800"
                        : "bg-gray-100 text-gray-800"
                    }`}
                  >
                    {session.status}
                  </span>
                </div>
                <p className="text-xs text-muted-foreground mt-2">
                  {new Date(session.created_at).toLocaleString()}
                </p>
              </Link>
            ))}
          </div>
        )}
      </main>
    </div>
  );
}
