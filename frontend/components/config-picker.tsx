"use client";

/**
 * Session config: the picker a control command opens.
 *
 * `/model` in a terminal opens a list. Through a headless protocol the same request
 * sent as prompt text can only come back as a paragraph, because the CLI has no
 * picker to show — so this is where the picker lives instead, driven by typed state
 * the agent reported rather than by anything hardcoded here.
 *
 * Nothing in this file names a model, an effort level, or a permission mode. It
 * renders whatever `ConfigOption`s the session declared, grouped by their own
 * `category`, which is what makes it correct for an agent this UI has never seen.
 * The two installed agents already disagree about the contents: `claude` offers a
 * model catalogue plus effort, permissions and fast mode; `pi` offers a model list
 * of 21 across providers plus a two-value thinking level.
 */

import { useMemo } from "react";
import type { ConfigOption } from "@/lib/api";
import { useT, type Translate } from "@/lib/i18n";
import { MenuButton, type MenuItem } from "@/components/ui/menu";
import { ChevronDownIcon } from "@/components/icons";

/** The label for an option, falling back to its id rather than inventing one. */
function optionLabel(option: ConfigOption): string {
  return option.name?.trim() || option.id;
}

/**
 * What the control currently reads as.
 *
 * A `select` shows the *name* of its current value, not the raw value: an agent's
 * model ids are things like `claude-opus-5[1m]`, and a control row is not where a
 * user should have to decode one.
 */
function currentLabel(option: ConfigOption, t: Translate): string {
  if (option.type === "boolean") {
    return option.currentValue ? t("action.on") : t("action.off");
  }
  const current = option.options.find((choice) => choice.value === option.currentValue);
  // An unmatched current value is shown verbatim rather than as "default": the agent
  // said it is set to something, and silently relabelling that hides a real state.
  return current?.name?.trim() || option.currentValue || t("composer.agentDefault");
}

/** Menu rows for one option. */
export function configItems(
  option: ConfigOption,
  onSet: (value: string | boolean) => void,
  t: Translate,
): MenuItem[] {
  if (option.type === "boolean") {
    return [
      {
        label: t("action.on"),
        checked: option.currentValue === true,
        onSelect: () => onSet(true),
      },
      {
        label: t("action.off"),
        checked: option.currentValue !== true,
        onSelect: () => onSet(false),
      },
    ];
  }

  return option.options.map((choice) => ({
    label: choice.name?.trim() || choice.value,
    // The agent's own description, which for a model list is what distinguishes
    // otherwise similar entries — pricing for `claude`, the provider for `pi`.
    hint: choice.description,
    checked: choice.value === option.currentValue,
    onSelect: () => onSet(choice.value),
  }));
}

/**
 * One config option as a labelled button that opens its choices.
 *
 * A button rather than a `<select>`, for the reason recorded in `ui/menu.tsx`: a
 * `<select>` is sized by its longest option, so one long model name widens the
 * control permanently and cannot be recovered with `truncate`.
 */
export function ConfigControl({
  option,
  onSet,
  disabled,
  icon,
}: {
  option: ConfigOption;
  onSet: (value: string | boolean) => void;
  disabled?: boolean;
  icon?: React.ReactNode;
}) {
  const t = useT();
  const items = useMemo(() => configItems(option, onSet, t), [option, onSet, t]);
  const name = optionLabel(option);

  return (
    <MenuButton label={name} items={items} align="start">
      {(trigger) => (
        <button
          {...trigger}
          type="button"
          disabled={disabled}
          // The option's own name is the accessible name, and the current value is
          // read after it, so a screen reader announces "Model, Sonnet 5" rather
          // than a bare value with no idea what it configures.
          aria-label={`${name}: ${currentLabel(option, t)}`}
          className="flex min-w-0 items-center gap-1 rounded-md border border-line px-2 py-1 text-2xs text-ink-muted hover:bg-active disabled:opacity-50"
        >
          {icon}
          <span className="truncate">{currentLabel(option, t)}</span>
          <ChevronDownIcon size={12} />
        </button>
      )}
    </MenuButton>
  );
}

/**
 * Every config option, as one flat menu grouped by category.
 *
 * Used when a control command names no specific option — `/config` on an agent that
 * reports several — and by the composer's overflow menu. Flat rather than nested
 * because a submenu on a touch screen is a reliable miss-tap generator: the parent
 * row is still a target while the child list opens.
 */
export function configMenuItems(
  config: ConfigOption[],
  onSet: (configId: string, value: string | boolean) => void,
  t: Translate,
): MenuItem[] {
  const groups = new Map<string, ConfigOption[]>();
  for (const option of config) {
    const key = option.category ?? option.id;
    const existing = groups.get(key);
    if (existing) existing.push(option);
    else groups.set(key, [option]);
  }

  const items: MenuItem[] = [];
  for (const options of groups.values()) {
    for (const option of options) {
      items.push({
        label: optionLabel(option),
        // A disabled row as a heading: it introduces the choices beneath it without
        // being a target itself.
        separated: true,
        disabled: true,
        onSelect: () => {},
      });
      items.push(
        ...configItems(option, (value) => onSet(option.id, value), t),
      );
    }
  }
  return items;
}
