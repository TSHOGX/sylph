interface Entry {
  bytes: Uint8Array | null;
  cost: number;
  readers: number;
}

/** Open body windows hold reservations; eviction can only release closed windows. */
export class PayloadCache {
  private entries = new Map<string, Entry>();
  private used = 0;
  private limit = 0;
  configure(limit: number) {
    this.limit = Math.max(0, limit);
    this.trim(0);
  }
  get bytes() {
    return this.used;
  }
  acquire(key: string, maximumBytes: number): boolean {
    const existing = this.entries.get(key);
    if (existing) {
      existing.readers++;
      return true;
    }
    const cost = maximumBytes + new TextEncoder().encode(key).length;
    if (!Number.isSafeInteger(cost) || cost <= 0 || !this.trim(cost))
      return false;
    this.entries.set(key, { bytes: null, cost, readers: 1 });
    this.used += cost;
    return true;
  }
  set(key: string, bytes: Uint8Array) {
    const entry = this.entries.get(key);
    if (!entry) return;
    const cost = bytes.byteLength + new TextEncoder().encode(key).length;
    if (cost > entry.cost)
      throw new Error("Retained body exceeded its reserved byte range");
    if (entry.bytes) {
      if (
        entry.bytes.length !== bytes.length ||
        bytes.some((value, index) => value !== entry.bytes![index])
      )
        throw new Error(
          "Retained content changed without an immutable revision",
        );
      return;
    }
    this.used += cost - entry.cost;
    entry.cost = cost;
    entry.bytes = bytes;
  }
  get(key: string): Uint8Array | undefined {
    const entry = this.entries.get(key);
    if (!entry?.bytes) return;
    this.entries.delete(key);
    this.entries.set(key, entry);
    return entry.bytes;
  }
  release(key: string) {
    const entry = this.entries.get(key);
    if (!entry) return;
    entry.readers = Math.max(0, entry.readers - 1);
    if (!entry.readers && !entry.bytes) {
      this.entries.delete(key);
      this.used -= entry.cost;
    }
    this.trim(0);
  }
  clear() {
    this.entries.clear();
    this.used = 0;
  }
  private trim(incoming: number): boolean {
    if (incoming > this.limit) return false;
    for (const [key, entry] of this.entries) {
      if (this.used + incoming <= this.limit) break;
      if (entry.readers) continue;
      this.entries.delete(key);
      this.used -= entry.cost;
    }
    return this.used + incoming <= this.limit;
  }
}
