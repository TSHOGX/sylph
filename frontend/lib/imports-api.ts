import { fetchAPI } from "./api";

export type ImportMode = "import_once" | "import_and_watch";
export interface ImportJob {
  id: string;
  environment: string;
  originSubject: string;
  workdir: string | null;
  mode: ImportMode;
  status: "queued" | "running" | "completed" | "cancelled" | "failed";
  scanComplete: boolean;
  cancelRequested: boolean;
  total: number;
  imported: number;
  refreshed: number;
  skipped: number;
  failed: number;
  pending: number;
  error: string | null;
}
export interface ImportItem {
  ordinal: number;
  agent: string;
  nativeId: string;
  title: string | null;
  workdir: string | null;
  status: "pending" | "running" | "imported" | "refreshed" | "skipped" | "failed";
  reason: string | null;
}

export const importsApi = {
  create: (environment: string, options: { workdir?: string; agents?: string[]; mode: ImportMode; idempotencyKey: string }) =>
    fetchAPI<{ job: ImportJob }>(`/environments/${encodeURIComponent(environment)}/imports`, { method: "POST", body: JSON.stringify(options) }),
  list: (environment: string, workdir?: string, signal?: AbortSignal) => fetchAPI<{ jobs: ImportJob[] }>(`/environments/${encodeURIComponent(environment)}/imports/list${workdir ? `?workdir=${encodeURIComponent(workdir)}` : ""}`, { signal }),
  get: (id: string, signal?: AbortSignal) => fetchAPI<{ job: ImportJob }>(`/imports/${encodeURIComponent(id)}`, { signal }),
  items: (id: string, after?: number, signal?: AbortSignal) => fetchAPI<{ items: ImportItem[]; nextCursor: number | null }>(`/imports/${encodeURIComponent(id)}/items${after !== undefined ? `?after=${after}` : ""}`, { signal }),
  cancel: (id: string) => fetchAPI<{ job: ImportJob }>(`/imports/${encodeURIComponent(id)}/cancel`, { method: "POST" }),
  retry: (id: string) => fetchAPI<{ job: ImportJob }>(`/imports/${encodeURIComponent(id)}/retry`, { method: "POST" }),
};
