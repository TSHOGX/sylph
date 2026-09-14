/** A failed logical submission keeps its identity; acceptance ends that identity. */
export function createSubmissionIdentity(makeKey: () => string = () => crypto.randomUUID()) {
  let pending: { fingerprint: string; key: string } | undefined;
  return {
    key(payload: unknown, initialKey?: string) {
      const fingerprint = JSON.stringify(payload);
      if (pending?.fingerprint !== fingerprint) pending = { fingerprint, key: initialKey ?? makeKey() };
      return pending.key;
    },
    accepted(key: string) {
      if (pending?.key === key) pending = undefined;
    },
  };
}
