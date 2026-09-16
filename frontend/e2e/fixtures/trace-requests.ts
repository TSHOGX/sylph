import type { PendingRequest } from "../../lib/api-types";
import type { Page } from "@playwright/test";
import { installTraceFixture, MAIN_AGENT, openTrace } from "./trace";

export function questionRequest(): PendingRequest {
  return {
    id: "question-request",
    kind: "choice",
    title: "Two questions",
    detail: {},
    options: [
      { id: "submit", label: "Send answer" },
      { id: "deny", label: "Don't answer" },
    ],
    form: {
      fields: [
        {
          type: "select",
          id: "q:0",
          label: "Indentation?",
          mode: "one",
          choices: [
            { value: "Tabs", description: "Indent with tabs." },
            { value: "Spaces", description: "Indent with spaces." },
          ],
          allowOther: true,
          otherLabel: "Something else",
          notes: { label: "Notes (optional)" },
        },
        {
          type: "select",
          id: "q:1",
          label: "Linters?",
          mode: "many",
          choices: [
            { value: "ESLint" },
            { value: "Prettier" },
            { value: "Clippy" },
          ],
          required: true,
        },
      ],
    },
    allowPersistent: true,
  };
}
export async function requestFixture(page: Page, request = questionRequest()) {
  const state = await installTraceFixture(page);
  state.control = {
    kind: "available",
    target: {
      kind: "session",
      agent: MAIN_AGENT,
      sessionId: "fixture-session",
    },
    actions: ["send", "approval"],
    lease: "control-lease",
    affectedExecution: null,
  };
  state.requests.push(request);
  return state;
}
export async function openRequest(page: Page) {
  await openTrace(page);
  await page.getByRole("button", { name: "Show request", exact: true }).click();
  return page.getByTestId("approval-dialog");
}
