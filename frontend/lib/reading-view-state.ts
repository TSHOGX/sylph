import { ExecutionStore } from "./execution-store";

/** Keep lightweight view state for recent sessions. Closing a reader stops all
 * transports; reopening resumes its bounded windows rather than losing position. */
const sessions = new Map<string, Map<string, ExecutionStore>>();
export function readingView(
  sessionId: string,
  root: string,
  view: string,
): ExecutionStore {
  const views = sessions.get(sessionId) ?? new Map<string, ExecutionStore>();
  sessions.delete(sessionId);
  sessions.set(sessionId, views);
  while (sessions.size > 10) {
    const oldest = [...sessions.keys()].find((id) =>
      [...sessions.get(id)!.values()].every((store) => !store.visibleReader),
    );
    if (oldest === undefined) break;
    for (const store of sessions.get(oldest)!.values()) store.dispose();
    sessions.delete(oldest);
  }
  const key = `${view}:${root}`;
  let store = views.get(key);
  if (!store) {
    store = new ExecutionStore(sessionId, true, root);
    views.set(key, store);
    // Histories beyond this bound retain their server addresses and remain
    // reachable through the directory; no hidden subtree grows without limit.
    while (views.size > 10) {
      const oldest = [...views.keys()].find(
        (id) => !views.get(id)!.visibleReader,
      );
      if (oldest === undefined) break;
      views.get(oldest)!.dispose();
      views.delete(oldest);
    }
  }
  return store;
}
