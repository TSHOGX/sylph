"use client";

/**
 * The configuration section of settings: what resolved, and who said so.
 *
 * Its own file rather than another function in `settings-panel.tsx` because it is
 * a table with its own fetch, and because what it is *for* is different from
 * everything around it. Every other section changes a preference; this one exists
 * so a misconfiguration is visible.
 *
 * # Why the app shows this at all
 *
 * Every failure in this area has one shape: a path resolves somewhere nothing
 * lives, the server starts, health reports fine, and the symptom appears layers
 * away — a dead slash key, a Quick Start directory nothing can be created in.
 * The fix is trivial once you can see which path was used and who supplied it, and
 * the entire cost is in not being able to see it. `sylph-web-backend doctor` prints
 * this for a terminal; this renders the same report for someone who never opens
 * one.
 *
 * # Why it is read-only
 *
 * There is deliberately no edit control. `browse_roots` is a security decision — it
 * bounds not merely what a client may *enumerate* but what it may name as a session's
 * workdir, which is the `current_dir` of a spawned agent, so it decides where arbitrary
 * code may run — and the config file is the durable, reviewable record of it. So this
 * shows the values and the path of the file to open, which is also what keeps the file
 * the single source rather than one of two.
 */

import { useQuery } from "@tanstack/react-query";
import { api, type ConfigEntry } from "@/lib/api";
import { useT } from "@/lib/i18n";
import { Field } from "@/components/settings-panel";

export function ConfigSection() {
  const t = useT();

  // Not cached hard: the `exists` flags are filesystem facts, and the reason a
  // user opens this panel is usually that they have just changed something. A
  // stale copy telling them a directory they created is still missing is worse
  // than a request.
  const config = useQuery({
    queryKey: ["config"],
    queryFn: () => api.config.get(),
    staleTime: 0,
  });

  if (config.isPending) {
    return <p className="text-xs text-ink-muted">{t("action.loading")}</p>;
  }

  if (config.isError || !config.data) {
    return (
      <div className="space-y-2">
        <p className="text-xs text-danger">{t("settings.config.failed")}</p>
        <button
          type="button"
          onClick={() => void config.refetch()}
          className="rounded border border-line-strong px-2 py-1 text-xs font-medium hover:bg-sunken focus-ring"
        >
          {t("app.retry")}
        </button>
      </div>
    );
  }

  const report = config.data;

  return (
    <div className="space-y-4">
      <Field label={t("settings.config.file.label")} hint={t("settings.config.file.hint")}>
        <p className="rounded border border-line bg-sunken px-2 py-1 font-mono text-2xs text-ink-muted">
          {report.configPath}
        </p>
        {/*
          Stated rather than implied. An absent file is the ordinary state of a
          fresh install and not an error, but a user reading a table of values
          needs to know none of them came from a file they could edit.
        */}
        {!report.configPresent && (
          <p className="text-2xs text-ink-muted">{t("settings.config.file.absent")}</p>
        )}
      </Field>

      <Field label={t("settings.config.resolved.label")} hint={t("settings.config.resolved.hint")}>
        {/*
          A table, not a definition list: the source column is the reason this
          screen exists and it has to be readable *down* the page. Scrolls in its
          own container so a long path cannot push the panel sideways.
        */}
        <div className="overflow-x-auto">
          <table className="w-full border-collapse text-2xs">
            <thead>
              <tr className="text-left text-ink-muted">
                <th scope="col" className="pb-1 pr-3 font-medium">
                  {t("settings.config.column.setting")}
                </th>
                <th scope="col" className="pb-1 pr-3 font-medium">
                  {t("settings.config.column.value")}
                </th>
                <th scope="col" className="pb-1 font-medium">
                  {t("settings.config.column.source")}
                </th>
              </tr>
            </thead>
            <tbody>
              {report.entries.map((entry) => (
                <Row key={entry.key} entry={entry} />
              ))}
            </tbody>
          </table>
        </div>
      </Field>

      {/* The "Bundled adapters" table is deleted rather than moved. It listed the
          directories adapter discovery searched and marked which one was in use, and
          that question no longer exists: adapters are linked into the envagent, so
          there is no directory to search and no candidate to choose between. The
          escape hatch for "a vendor changed faster than the shipped adapter" is
          upgrading the envagent on that one machine, which touches no server, no
          schema, and no other environment.

          What answers the question the table existed for — "why can I not use this
          agent" — is Settings → Machines & agents, which names the CLI that was not
          found and the machine it was not found on. */}
    </div>
  );
}

function Row({ entry }: { entry: ConfigEntry }) {
  const t = useT();

  // `undefined` and `false` must not render the same way: a log level is not a
  // path, and marking it missing would report a working setting as broken.
  const missing = entry.exists === false;

  return (
    <tr className="align-baseline">
      <th scope="row" className="py-0.5 pr-3 text-left font-mono font-normal text-ink-muted">
        {entry.key}
      </th>
      <td className="py-0.5 pr-3 font-mono break-all">
        <span className={missing ? "text-danger" : "text-ink"}>{entry.value || "—"}</span>
        {missing && (
          <span className="ml-1.5 font-sans text-danger">{t("settings.config.missing")}</span>
        )}
      </td>
      <td className="py-0.5 whitespace-nowrap text-ink-muted">
        {entry.source === "env" ? (
          // The variable, not the word "environment": a reader acting on this row
          // needs the name, and having it here means not consulting a table
          // somewhere else.
          <span className="font-mono">{entry.envVar}</span>
        ) : (
          t(entry.source === "file" ? "settings.config.source.file" : "settings.config.source.default")
        )}
      </td>
    </tr>
  );
}
