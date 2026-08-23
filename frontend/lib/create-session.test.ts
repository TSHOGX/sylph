import { describe, expect, it } from "vitest";
import { resolveSessionDefaults } from "./create-session";
import { DEFAULT_SETTINGS } from "./settings";
import type { Agent } from "./api";

/**
 * Dialog-free creation depends entirely on these defaults being resolvable, so
 * the failure modes worth pinning are the stale-preference ones: a stored agent
 * or model that names something the installed runners no longer offer.
 */

const AGENTS: Agent[] = [
  { name: "claude", flags: { model: { choices: ["opus", "sonnet"] } } },
  { name: "codex", flags: { model: { choices: ["gpt-5"] } } },
  { name: "plain" },
];

function settings(patch: Partial<typeof DEFAULT_SETTINGS> = {}) {
  return { ...DEFAULT_SETTINGS, ...patch };
}

describe("resolveSessionDefaults", () => {
  it("uses the configured default agent and its configured model", () => {
    const resolved = resolveSessionDefaults(
      AGENTS,
      settings({ defaultAgent: "codex", defaultModels: { codex: "gpt-5" } }),
    );
    expect(resolved).toEqual({ agent: "codex", flags: { model: "gpt-5" }, identityMode: "sylph" });
  });

  it("falls back to the first discovered runner when no default is configured", () => {
    expect(resolveSessionDefaults(AGENTS, settings())?.agent).toBe("claude");
  });

  it("falls back to a discovered runner when the configured default is uninstalled", () => {
    // A stored preference outlives the runner it names. Passing it through would
    // be rejected by the backend as an unknown agent, so creation must not fail
    // on a setting the user cannot see from the sidebar they clicked.
    const resolved = resolveSessionDefaults(AGENTS, settings({ defaultAgent: "removed" }));
    expect(resolved?.agent).toBe("claude");
  });

  it("drops a model the chosen runner does not declare", () => {
    // `--flag model=opus` against a runner without that choice fails at turn
    // time, far from the setting that caused it.
    const resolved = resolveSessionDefaults(
      AGENTS,
      settings({ defaultAgent: "codex", defaultModels: { codex: "opus" } }),
    );
    expect(resolved).toEqual({ agent: "codex", flags: undefined, identityMode: "sylph" });
  });

  it("omits flags entirely for a runner that declares no model flag", () => {
    const resolved = resolveSessionDefaults(
      AGENTS,
      settings({ defaultAgent: "plain", defaultModels: { plain: "opus" } }),
    );
    expect(resolved).toEqual({ agent: "plain", flags: undefined, identityMode: "sylph" });
  });

  it("resolves the model per agent, not globally", () => {
    // `defaultModels` is keyed by agent, so switching the default agent must pick
    // up that agent's entry rather than carrying the previous one.
    const both = settings({ defaultModels: { claude: "sonnet", codex: "gpt-5" } });
    expect(resolveSessionDefaults(AGENTS, { ...both, defaultAgent: "claude" })?.flags).toEqual({
      model: "sonnet",
    });
    expect(resolveSessionDefaults(AGENTS, { ...both, defaultAgent: "codex" })?.flags).toEqual({
      model: "gpt-5",
    });
  });

  it("returns null when no runners were discovered", () => {
    // The one case with no sensible default: the caller must report it rather
    // than sending an empty agent name.
    expect(resolveSessionDefaults([], settings({ defaultAgent: "claude" }))).toBeNull();
  });
});

/**
 * Identity is the one resolved field that becomes durable session data rather
 * than a display preference, and it is fixed at creation — so if it were dropped
 * anywhere on this path, a user who turned SOUL.md off would go on getting it with
 * nothing on screen saying so.
 */
describe("identity in resolved defaults", () => {
  it("asks for Sylph identity when the preference is on", () => {
    expect(resolveSessionDefaults(AGENTS, settings({ injectSoul: true }))?.identityMode).toBe(
      "sylph",
    );
  });

  it("asks for the agent's own identity when the preference is off", () => {
    expect(resolveSessionDefaults(AGENTS, settings({ injectSoul: false }))?.identityMode).toBe(
      "preserve",
    );
  });

  it("defaults to Sylph identity, matching the backend's own default", () => {
    expect(resolveSessionDefaults(AGENTS, settings())?.identityMode).toBe("sylph");
  });

  /**
   * Unlike agent and model, identity is never checked against a descriptor: it is
   * a policy the backend accepts from any client, not a value a runner declares.
   * A runner offering no model must still be able to run without Sylph identity.
   */
  it("is resolved for a runner that declares no flags at all", () => {
    expect(
      resolveSessionDefaults(AGENTS, settings({ defaultAgent: "plain", injectSoul: false }))
        ?.identityMode,
    ).toBe("preserve");
  });
});
