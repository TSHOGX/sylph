/**
 * Shortcut hooks: the parts that read the settings store.
 *
 * The combo grammar and the browser's reserved set live in `lib/combo.ts`, which
 * is free of React and of this store. Everything from there is re-exported here,
 * so a component can keep importing `@/lib/shortcuts` and see one API.
 */

import { useCallback, useSyncExternalStore } from "react";
import { useSettings, type ShortcutAction } from "@/lib/settings";
import { formatCombo, isMacPlatform } from "@/lib/combo";

/**
 * Platform detection as an external-store read.
 *
 * The platform is a property of the environment, not React state, and reading it
 * in an effect that then calls `setState` costs a second render on every mount of
 * every component that shows a shortcut hint. `useSyncExternalStore` with a
 * never-firing subscribe expresses "constant, but unavailable on the server":
 * the server snapshot is `false`, so shortcut hints render in their non-Mac form
 * during prerender and settle on hydration without a mismatch.
 */
const noopSubscribe = () => () => {};

export function useIsMac(): boolean {
  return useSyncExternalStore(noopSubscribe, isMacPlatform, () => false);
}

/**
 * Look up the display form of whatever combo is bound to an action.
 *
 * Reverse of the stored mapping, which is keyed by combo because that is what a
 * keydown produces. Surfaces that *teach* a shortcut need the opposite direction
 * — "how do I create a session" — and every one of them was writing the same
 * `Object.entries(...).find(...)` before rendering a `<kbd>`. Hard-coding `⌘N`
 * instead would be wrong for a rebound binding and wrong on Linux, which is the
 * whole reason bindings are stored platform-neutral.
 *
 * Returns `""` when an action has no binding, so a caller can treat it as falsy
 * and render no hint rather than an empty key cap.
 */
export function useShortcutHint(): (action: ShortcutAction) => string {
  const { settings } = useSettings();
  const isMac = useIsMac();

  return useCallback(
    (action: ShortcutAction) => {
      const combo = Object.entries(settings.shortcuts).find(
        ([, bound]) => bound === action,
      )?.[0];
      return combo ? formatCombo(combo, isMac) : "";
    },
    [settings.shortcuts, isMac],
  );
}

export {
  comboFromEvent,
  formatCombo,
  isMacPlatform,
  isTypingTarget,
  makeCombo,
  pruneReserved,
  RESERVED_COMBOS,
  reservedReason,
} from "@/lib/combo";
export type { Combo } from "@/lib/combo";
