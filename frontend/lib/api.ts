/**
 * API client for Sylph Web backend.
 *
 * All requests go through the Next.js rewrite (`/api/v1/*` → backend on 4317).
 * Auth is cookie-based: the bootstrap token endpoint sets `sylph_session`, which
 * the browser sends on every subsequent request. No explicit Authorization header.
 */

const API_BASE = "/api/v1";

export interface Session {
  id: string;
  agent: string;
  workdir: string;
  title: string;
  origin: "sylph" | "native";
  created_at: string;
  updated_at: string;
  status: "idle" | "running";
}

export interface Turn {
  id: string;
  session_id: string;
  status: "pending" | "running" | "completed" | "failed" | "cancelled" | "interrupted";
  created_at: string;
  started_at?: string;
  completed_at?: string;
}

export interface CreateSessionRequest {
  agent: string;
  workdir: string;
  title: string;
  origin?: "sylph" | "native";
}

export interface CreateTurnRequest {
  model: Record<string, unknown>;
  message: {
    role: "user";
    content: Array<{ type: "text"; text: string }>;
  };
  attachments: Array<unknown>;
}

async function fetchAPI<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${API_BASE}${path}`, {
    ...init,
    credentials: "include", // Send cookies
    headers: {
      "Content-Type": "application/json",
      ...init?.headers,
    },
  });

  if (!response.ok) {
    const error = await response.json().catch(() => ({ message: response.statusText }));
    throw new Error(error.message || `API error: ${response.status}`);
  }

  return response.json();
}

export const api = {
  sessions: {
    list: () => fetchAPI<{ sessions: Session[] }>("/sessions"),
    get: (id: string) => fetchAPI<{ session: Session }>(`/sessions/${id}`),
    create: (req: CreateSessionRequest) =>
      fetchAPI<{ session: Session }>("/sessions", {
        method: "POST",
        body: JSON.stringify(req),
      }),
  },
  turns: {
    create: (sessionId: string, req: CreateTurnRequest) =>
      fetchAPI<{ turn: Turn }>(`/sessions/${sessionId}/turns`, {
        method: "POST",
        body: JSON.stringify(req),
      }),
    get: (turnId: string) => fetchAPI<{ turn: Turn }>(`/turns/${turnId}`),
  },
  health: () => fetchAPI<{ ok: boolean; apiVersion: string }>("/health"),
};
