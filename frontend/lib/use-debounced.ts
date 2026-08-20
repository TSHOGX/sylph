import { useEffect, useState } from "react";

/**
 * Trail a rapidly changing value, so typing does not fire a request per keystroke.
 *
 * Used for free-text filters, which now run server-side: without this, every
 * character in the workspace box would be its own query. Select and date inputs
 * change at most once per interaction and are passed through undebounced.
 */
export function useDebounced<T>(value: T, delayMs = 250): T {
  const [settled, setSettled] = useState(value);

  useEffect(() => {
    const timer = setTimeout(() => setSettled(value), delayMs);
    return () => clearTimeout(timer);
  }, [value, delayMs]);

  return settled;
}
