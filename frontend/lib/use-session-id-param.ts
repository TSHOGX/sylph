"use client";

/** Decode opaque prepared-session handles from App Router path parameters. */
import { useParams } from "next/navigation";

export function decodeSessionId(raw: string): string {
  try {
    return decodeURIComponent(raw);
  } catch {
    return raw;
  }
}

export function useSessionIdParam(): string | undefined {
  const params = useParams();
  return typeof params?.id === "string" ? decodeSessionId(params.id) : undefined;
}
