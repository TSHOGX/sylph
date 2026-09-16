import type { Translate } from "./i18n/locale";
import { sameTraceView } from "./trace-cache";
import type { ReadingUnit, TracePage } from "./trace-types";

export interface TraceWindow extends TracePage<ReadingUnit> {
  windows?: TracePage<ReadingUnit>[];
}
/** Keep complete page boundaries so eviction never leaves a cursor beyond discarded rows. */
export function extendTraceWindow(
  current: TraceWindow,
  page: TracePage<ReadingUnit>,
  direction: "before" | "after",
): TraceWindow {
  if (!sameTraceView(current.view, page.view))
    throw new Error("History changed. Refresh around the current item.");
  const previous = current.windows ?? [current];
  const windows =
    direction === "before"
      ? [page, ...previous].slice(0, 2)
      : [...previous, page].slice(-2);
  const items = [
    ...new Map(
      windows
        .flatMap((window) => window.items)
        .map((item) => [item.address, item]),
    ).values(),
  ];
  const first = windows[0],
    last = windows[windows.length - 1];
  return {
    ...page,
    windows,
    items,
    beforeCursor: first.beforeCursor,
    afterCursor: last.afterCursor,
    hasBefore: first.hasBefore,
    hasAfter: last.hasAfter,
    returnedItems: items.length,
    returnedBytes: new TextEncoder().encode(JSON.stringify(items)).length,
  };
}

export function describeCount(
  count: { value: number | null; complete: boolean },
  noun: string,
  t?: Translate,
): string {
  if (t)
    return count.value === null
      ? t("trace.countUnknown", { noun })
      : t(count.complete ? "trace.count" : "trace.countPartial", {
          count: count.value,
          noun,
        });
  return count.value === null
    ? `${noun} count unknown`
    : `${count.value} ${noun}${count.complete ? "" : " · coverage incomplete"}`;
}

/** A failed boundary remains retryable even though it has no successful page metadata. */
export function tracePageButtonDisabled(
  pending: boolean,
  failed: boolean,
  hasNextPage: boolean | undefined,
  hasRequestedPage: boolean,
): boolean {
  return pending || (hasRequestedPage && !failed && !hasNextPage);
}
