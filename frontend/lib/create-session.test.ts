import { describe, expect, it } from "vitest";
import { isDefaultsProblem, resolveSessionDefaults } from "./create-session";
import { DEFAULT_SETTINGS } from "./settings";
import type { Environment } from "./api";

/**
 * Dialog-free creation depends entirely on these defaults being resolvable, so the
 * failure modes worth pinning are the stale-preference ones: a stored agent or model
 * naming something that no longer exists.
 *
 * **What changed with environments, and why it is more than a signature.** Agent
 * availability is per machine, so `defaultAgent` is a preference expressed with no
 * environment in view — it can name an agent that exists on the laptop and not on the
 * build box. Resolution therefore happens *within* a chosen environment, and the
 * interesting new case is the one at the bottom: the same settings resolving to
 * different agents on different machines. Nothing about the old signature could
 * express that, which is why it took an array and could only ever be right by
 * coincidence on a multi-machine install.
 */

function environment(patch: Partial<Environment> = {}): Environment {
  return {
    id: "env7fk2",
    name: "laptop",
    connected: true,
    origin: "self-installed",
    reachableVia: null,
    agents: [
      { name: "claude", version: "2.1.247", supports: [], flags: { model: { choices: ["opus", "sonnet"] } } },
      { name: "codex", version: "0.150.1", supports: [], flags: { model: { choices: ["gpt-5"] } } },
      { name: "plain", version: "1.0.0", supports: [] },
    ],
    ...patch,
  };
}

function settings(patch: Partial<typeof DEFAULT_SETTINGS> = {}) {
  return { ...DEFAULT_SETTINGS, ...patch };
}

/** Narrowed, so a test asserting on fields fails loudly rather than on `undefined`. */
function resolved(env: Environment | null, config = settings()) {
  const result = resolveSessionDefaults(env, config);
  if (isDefaultsProblem(result)) {
    throw new Error(`expected defaults, got the problem "${result}"`);
  }
  return result;
}

describe("resolveSessionDefaults", () => {
  it("uses the configured default agent and its configured model", () => {
    expect(
      resolved(
        environment(),
        settings({ defaultAgent: "codex", defaultModels: { codex: "gpt-5" } }),
      ),
    ).toEqual({
      environment: "env7fk2",
      agent: "codex",
      flags: { model: "gpt-5" },
      injectIdentity: true,
    });
  });

  it("names the environment it resolved against, because there is no default one", () => {
    // `POST /sessions` has no default environment — not even the machine the server
    // runs on is privileged — so an omitted field is not "the obvious one", it is a
    // request that cannot be served.
    expect(resolved(environment({ id: "env9xk4" })).environment).toBe("env9xk4");
  });

  it("falls back to the first agent on the machine when no default is configured", () => {
    expect(resolved(environment()).agent).toBe("claude");
  });

  it("falls back when the configured default is not installed on this machine", () => {
    // A stored preference outlives the CLI it names. Passing it through would be
    // rejected as an unknown agent, so creation must not fail on a setting the user
    // cannot see from the directory they clicked.
    expect(resolved(environment(), settings({ defaultAgent: "removed" })).agent).toBe(
      "claude",
    );
  });

  it("resolves the same preference to different agents on different machines", () => {
    // The case the old agent-array signature could not express, and the reason this
    // function takes an environment. `codex` is the stored preference; the build box
    // does not have it, so the same settings must resolve to what that machine has
    // rather than sending an agent it cannot run.
    const preference = settings({ defaultAgent: "codex" });
    const buildBox = environment({
      id: "env2m5q",
      name: "build-box",
      agents: [{ name: "claude", version: "2.1.247", supports: [] }],
    });

    expect(resolved(environment(), preference).agent).toBe("codex");
    expect(resolved(buildBox, preference).agent).toBe("claude");
  });

  it("drops a model the chosen agent does not declare", () => {
    // A flag the agent never offered fails at turn time, far from the setting that
    // caused it.
    expect(
      resolved(
        environment(),
        settings({ defaultAgent: "codex", defaultModels: { codex: "opus" } }),
      ),
    ).toEqual({
      environment: "env7fk2",
      agent: "codex",
      flags: undefined,
      injectIdentity: true,
    });
  });

  it("omits flags entirely for an agent that declares no model flag", () => {
    expect(
      resolved(
        environment(),
        settings({ defaultAgent: "plain", defaultModels: { plain: "opus" } }),
      ).flags,
    ).toBeUndefined();
  });

  it("resolves the model per agent, not globally", () => {
    // `defaultModels` is keyed by agent, so switching the default agent must pick up
    // that agent's entry rather than carrying the previous one.
    const both = settings({ defaultModels: { claude: "sonnet", codex: "gpt-5" } });
    expect(resolved(environment(), { ...both, defaultAgent: "claude" }).flags).toEqual({
      model: "sonnet",
    });
    expect(resolved(environment(), { ...both, defaultAgent: "codex" }).flags).toEqual({
      model: "gpt-5",
    });
  });
});

/**
 * The two failures, which are distinct because the fixes are: pair or wake a machine,
 * against install a CLI on the one you have. A caller that cannot tell them apart can
 * only say "something is wrong", which is what returning `null` for both did.
 */
describe("problems are distinguishable", () => {
  it("reports no-environment when nothing was chosen", () => {
    expect(resolveSessionDefaults(null, settings())).toBe("no-environment");
  });

  it("reports no-agent when the machine has no CLI on its PATH", () => {
    // A connected machine with an empty agent list is an ordinary state — an envagent
    // paired before any agent was installed — and it is emphatically not the same
    // problem as having no machine.
    expect(resolveSessionDefaults(environment({ agents: [] }), settings())).toBe("no-agent");
  });

  it("narrows for the caller rather than making it re-check the shape", () => {
    expect(isDefaultsProblem("no-agent")).toBe(true);
    expect(isDefaultsProblem(resolveSessionDefaults(environment(), settings()))).toBe(false);
  });
});

/**
 * Identity is the one resolved field that becomes durable session data rather than a
 * display preference, and it is fixed at creation — so if it were dropped anywhere on
 * this path, a user who turned SOUL.md off would go on getting it with nothing on
 * screen saying so.
 *
 * It is also the field whose spelling changed: `identityMode: "sylph" | "preserve"` is
 * now the boolean `injectIdentity`, because there was never a second identity *policy*
 * to name — only a yes and a no.
 */
describe("identity in resolved defaults", () => {
  it("asks for Sylph identity when the preference is on", () => {
    expect(resolved(environment(), settings({ injectSoul: true })).injectIdentity).toBe(
      true,
    );
  });

  it("asks for the agent's own identity when the preference is off", () => {
    expect(resolved(environment(), settings({ injectSoul: false })).injectIdentity).toBe(
      false,
    );
  });

  it("is always explicit, even when it matches the backend's own default", () => {
    // Never omitted: an absent field would make "the user asked for Sylph's identity"
    // and "a client that does not know about identity" the same request, and the
    // created session's own record is the only place this is readable afterwards.
    expect(resolved(environment(), settings())).toHaveProperty("injectIdentity");
    expect(resolved(environment(), settings()).injectIdentity).toBe(true);
  });

  /**
   * Unlike agent and model, identity is never checked against a descriptor: it is a
   * policy the backend accepts from any client, not a value an agent declares. An agent
   * offering no flags at all must still be able to run without Sylph's identity.
   */
  it("is resolved for an agent that declares no flags at all", () => {
    expect(
      resolved(environment(), settings({ defaultAgent: "plain", injectSoul: false }))
        .injectIdentity,
    ).toBe(false);
  });
});
