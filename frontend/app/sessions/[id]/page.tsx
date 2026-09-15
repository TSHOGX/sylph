"use client";
import { SessionView } from "@/components/session-view";
import { useSessionIdParam } from "@/lib/use-session-id-param";

export default function SessionPage() {
  const sessionId = useSessionIdParam() ?? "";
  return <SessionView key={sessionId} sessionId={sessionId} />;
}
