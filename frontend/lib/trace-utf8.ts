/** Decode complete UTF-8 scalars from an explicitly ranged retained body. */
export function utf8Window(
  bytes: Uint8Array,
  sourceStart: number,
  requestedStart: number,
  finalRange: boolean,
): { text: string; start: number; next: number } {
  let first = Math.min(bytes.length, Math.max(0, requestedStart - sourceStart));
  while (first > 0 && first < bytes.length && (bytes[first] & 0xc0) === 0x80)
    first--;
  let end = bytes.length;
  if (!finalRange && end > first) {
    let lead = end - 1;
    while (lead > first && (bytes[lead] & 0xc0) === 0x80) lead--;
    const value = bytes[lead];
    const width =
      value < 0x80
        ? 1
        : value >= 0xc2 && value <= 0xdf
          ? 2
          : value >= 0xe0 && value <= 0xef
            ? 3
            : value >= 0xf0 && value <= 0xf4
              ? 4
              : 0;
    if (width && lead + width > end) end = lead;
  }
  return {
    text: new TextDecoder("utf-8", { fatal: true }).decode(
      bytes.subarray(first, end),
    ),
    start: sourceStart + first,
    next: sourceStart + end,
  };
}
