import type { PayloadAccess } from "./trace-types";

export interface BodyWindow {
  start: number;
  sourceStart: number;
  end: number;
  rangeEnd: number;
  nextRange: number | null;
  previous: number | null;
}

/** A body window never asks the server to fill an unretained interval. */
export function bodyWindow(
  access: Pick<PayloadAccess, "availableRanges" | "totalBytes" | "availability">,
  requestedStart: number,
  maximum: number,
  overlap: number,
): BodyWindow | null {
  const declared = access.availableRanges.length
    ? access.availableRanges
    : access.availability.kind === "available"
      ? [{ start: 0, endExclusive: access.totalBytes ?? Infinity }]
      : [];
  // Adjacent spans contain no missing byte. Treating their shared boundary as
  // an end could split a UTF-8 scalar even though all of its bytes are retained.
  const ranges: { start: number; endExclusive: number }[] = [];
  for (const range of declared) {
    const previous = ranges.at(-1);
    if (previous?.endExclusive === range.start) previous.endExclusive = range.endExclusive;
    else ranges.push({ ...range });
  }
  if (!ranges.length || maximum <= overlap || requestedStart < 0) return null;
  const found = ranges.findIndex((range) => requestedStart < range.endExclusive);
  const index = found < 0 ? 0 : found;
  const range = ranges[index];
  const start = found < 0 ? range.start : Math.max(requestedStart, range.start);
  const sourceStart = Math.max(range.start, start - overlap);
  const previousRange = ranges[index - 1];
  return {
    start,
    sourceStart,
    end: Math.min(sourceStart + maximum, range.endExclusive),
    rangeEnd: range.endExclusive,
    nextRange: ranges[index + 1]?.start ?? null,
    previous: start > range.start
      ? Math.max(range.start, start - maximum + overlap)
      : previousRange ? Math.max(previousRange.start, previousRange.endExclusive - maximum) : null,
  };
}

export function nextBodyStart(window: BodyWindow, decodedEnd: number, total: number | null): number | null {
  if (decodedEnd >= window.rangeEnd) return window.nextRange;
  if (total !== null && decodedEnd >= total) return null;
  return decodedEnd > window.start ? decodedEnd : null;
}
