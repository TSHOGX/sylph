"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useParams } from "next/navigation";
import Link from "next/link";
import { useState } from "react";
import { api } from "@/lib/api";
import { useSSE, StreamEvent } from "@/lib/use-sse";

export default function SessionPage() {
  const params = useParams();
  const sessionId = params.id as string;
  const queryClient = useQueryClient();

  const [message, setMessage] = useState("");
  const [activeTurnId, setActiveTurnId] = useState<string | null>(null);

  const { data, isLoading, error } = useQuery({
    queryKey: ["session", sessionId],
    queryFn: () => api.sessions.get(sessionId),
  });

  const createTurnMutation = useMutation({
    mutationFn: (text: string) =>
      api.turns.create(sessionId, {
        model: {},
        message: {
          role: "user",
          content: [{ type: "text", text }],
        },
        attachments: [],
      }),
    onSuccess: (data) => {
      setActiveTurnId(data.turn.id);
      setMessage("");
      queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
    },
  });

  const handleSend = (e: React.FormEvent) => {
    e.preventDefault();
    if (!message.trim()) return;
    createTurnMutation.mutate(message);
  };

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
    <div className="min-h-screen bg-background flex flex-col">
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

      <main className="flex-1 container mx-auto px-4 py-8">
        <div className="max-w-4xl mx-auto flex flex-col h-full">
          <div className="flex-1 overflow-y-auto mb-4 space-y-4">
            {activeTurnId ? (
              <TurnView turnId={activeTurnId} />
            ) : (
              <div className="text-center py-12 text-muted-foreground">
                <p>No turns yet. Send a message to start.</p>
              </div>
            )}
          </div>

          <form onSubmit={handleSend} className="border-t pt-4">
            <div className="flex gap-2">
              <input
                type="text"
                value={message}
                onChange={(e) => setMessage(e.target.value)}
                placeholder="Type your message..."
                className="flex-1 px-4 py-2 border rounded-lg focus:ring-2 focus:ring-primary focus:outline-none"
                disabled={createTurnMutation.isPending}
              />
              <button
                type="submit"
                disabled={createTurnMutation.isPending || !message.trim()}
                className="px-6 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition disabled:opacity-50"
              >
                {createTurnMutation.isPending ? "Sending..." : "Send"}
              </button>
            </div>
            {createTurnMutation.error && (
              <p className="text-sm text-destructive mt-2">
                {createTurnMutation.error instanceof Error
                  ? createTurnMutation.error.message
                  : "Failed to create turn"}
              </p>
            )}
          </form>
        </div>
      </main>
    </div>
  );
}

function TurnView({ turnId }: { turnId: string }) {
  const { events, isConnected } = useSSE({ turnId });

  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <div className={`w-2 h-2 rounded-full ${isConnected ? "bg-green-500" : "bg-gray-400"}`} />
        {isConnected ? "Connected" : "Reconnecting..."}
      </div>

      {events.map((event, index) => (
        <EventCard key={`${event.seq}-${index}`} event={event} />
      ))}
    </div>
  );
}

function EventCard({ event }: { event: StreamEvent }) {
  const getEventStyle = () => {
    switch (event.kind) {
      case "thinking":
        return "bg-yellow-50 border-yellow-200 text-yellow-900";
      case "tool_call":
        return "bg-blue-50 border-blue-200 text-blue-900";
      case "assistant":
        return "bg-gray-50 border-gray-200";
      case "status":
        return "bg-green-50 border-green-200 text-green-900";
      case "error":
        return "bg-red-50 border-red-200 text-red-900";
      default:
        return "bg-gray-50 border-gray-200";
    }
  };

  const hasData = event.data !== undefined && event.data !== null;

  return (
    <div className={`p-4 border rounded-lg ${getEventStyle()}`}>
      <div className="flex items-start justify-between mb-2">
        <span className="text-sm font-medium">{event.kind}</span>
        <span className="text-xs text-muted-foreground">
          {new Date(event.created_at_ms).toLocaleTimeString()}
        </span>
      </div>

      {event.text && (
        <p className="whitespace-pre-wrap text-sm">{event.text}</p>
      )}

      {hasData && (
        <details className="mt-2">
          <summary className="text-xs cursor-pointer text-muted-foreground hover:text-foreground">
            View data
          </summary>
          <pre className="text-xs mt-2 p-2 bg-white/50 rounded overflow-x-auto">
            {JSON.stringify(event.data, null, 2)}
          </pre>
        </details>
      )}
    </div>
  );
}
