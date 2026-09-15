/** One in-flight read per URL. Each reader owns its cancellation; the transport
 * is cancelled only when its last subscriber leaves. Completed bodies are owned
 * by TanStack Query, not by this request registry. */
const pending = new Map<
  string,
  { controller: AbortController; users: number; promise: Promise<unknown> }
>();
export async function readResource<T>(
  url: string,
  signal: AbortSignal,
): Promise<T> {
  let entry = pending.get(url);
  if (!entry) {
    const controller = new AbortController();
    entry = {
      controller,
      users: 0,
      promise: fetch(url, {
        signal: controller.signal,
        credentials: "include",
        cache: "no-store",
      }).then(async (response) => {
        if (!response.ok)
          throw new Error(`Could not read content (${response.status})`);
        return response.json();
      }),
    };
    pending.set(url, entry);
    const current = entry;
    void entry.promise
      .finally(() => {
        if (pending.get(url) === current) pending.delete(url);
      })
      .catch(() => {});
  }
  entry.users++;
  const current = entry;
  return new Promise<T>((resolve, reject) => {
    let done = false;
    const release = () => {
      if (done) return false;
      done = true;
      signal.removeEventListener("abort", abort);
      if (--current.users === 0) {
        current.controller.abort();
        if (pending.get(url) === current) pending.delete(url);
      }
      return true;
    };
    const abort = () => {
      if (release()) reject(new DOMException("Aborted", "AbortError"));
    };
    signal.addEventListener("abort", abort, { once: true });
    if (signal.aborted) {
      abort();
      return;
    }
    current.promise.then(
      (value) => {
        if (release()) resolve(value as T);
      },
      (error) => {
        if (release()) reject(error);
      },
    );
  });
}
