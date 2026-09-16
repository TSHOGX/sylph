"use client";
import { useParams, useSearchParams } from "next/navigation";
import { AgentReader } from "@/components/agent-reader";
export default function AgentPage() {
  const params = useParams<{ ref: string }>();
  const query = useSearchParams();
  const agent = decodeURIComponent(params.ref);
  const history = query.get("history");
  const anchor = query.get("anchor");
  return <AgentReader key={`${agent}:${history ?? ""}:${anchor ?? ""}`} agent={agent} history={history} anchor={anchor} />;
}
