import { expect, test, type Page } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { FAKE_SERVE_PORT } from "../../playwright.config";
import { startFakeServe } from "../fake-serve.mjs";

let fake: Awaited<ReturnType<typeof startFakeServe>>;
test.beforeEach(async () => { fake = await startFakeServe({ port: FAKE_SERVE_PORT }); });
test.afterEach(async () => { await fake.close(); });

/** UI fixture only. Rust tests exercise actual admission, validation and policy. */
async function advisoryFixture(page: Page, unavailable = false) {
  const program = await readFile(new URL("../../../examples/assistance/writing_review.omar", import.meta.url), "utf8");
  const calls = { evaluations: 0, controlWrites: 0 };
  const states = new Map<string, Record<string, any>>(); // eslint-disable-line @typescript-eslint/no-explicit-any
  page.on("request", (request) => {
    if (request.method() === "POST" && /\/v1\/(runs|chat|permissions)(\/|$)/.test(new URL(request.url()).pathname)) calls.controlWrites += 1;
  });
  await page.route("**/v1/assist/**", async (route) => {
    const path = new URL(route.request().url()).pathname.replace(/^\/chats\/[^/]+/, "").replace("/v1/assist/", "");
    const body = route.request().postDataJSON();
    let data: unknown;
    if (path === "capabilities") data = { schema_version: 1, available: true, configured: true, enabled: true, key_present: true, model: "jev-1.13.0", modes: ["off", "shadow", "suggest"], profiles: ["template-fit-v1", "scenario-coverage-v1", "artifact-requirement-v1"], max_requests_per_run: 100, max_source_bytes: 65536 };
    else if (path === "templates") data = { version: "starter-workflows-v1", templates: [{ id: "writing-review", name: "Writing and review", purpose: "Draft, review, revise for a named audience.", eligible: true, backend: "Codex", constraint: "Three finite steps.", version: "starter-workflows-v1", required_inputs: ["flow.brief"], output_type: "text/markdown", program, scenarios: [] }] };
    else if (path === "artifact-snapshots") data = { capability: "saved text", workspaces: [{ workspace_id: "workspace-a", instance: "flow", snapshots: [{ id: "snapshot-1", commit: "11111111", label: "Before revision" }, { id: "snapshot-2", commit: "22222222", label: "After revision" }] }] };
    else if (path === "artifact-preview") data = { revision: body.snapshot_id, text: "A guide for beginners.", characters: 22, complete: true };
    else if (path === "subjects") {
      const old = states.get(body.id);
      const text = body.payload.text ?? body.payload.program ?? "A guide for beginners.";
      const subject = { id: body.id, kind: body.payload.kind, chat_id: "chat-a", workspace_id: "ea:0", revision: `${body.payload.kind}-revision`, sha256: `${body.id}:${JSON.stringify(body.payload)}` };
      data = { subject, mode: "off", records: old?.records.map((r: object) => ({ ...r, freshness: "stale" })) ?? [], input: { subject, profile_id: body.profile_id, criterion: body.criterion ?? { id: "review-actual-output", version: "1", text: "Reviewer assesses the actual draft.", requires_complete: true }, evidence: [{ id: "excerpt-1", text, start: 0, end: [...text].length }], complete: true, deterministic_failures: [], eligible_templates: ["writing-review"], responsibility_map: null } };
      states.set(body.id, data as Record<string, unknown>);
    } else {
      const [, id, action] = path.split("/");
      const state = states.get(id)!;
      if (action === "mode") { state.mode = body.mode; data = state; }
      else if (action === "evaluations") {
        calls.evaluations += 1;
        const profile = state.input.profile_id;
        const outcome = profile === "template-fit-v1" ? "writing-review" : profile === "scenario-coverage-v1" ? "missing_contract" : "not_satisfied";
        const record = { schema_version: 2, decision_id: `decision-${calls.evaluations}`, request_id: body.request_id, fingerprint: "fixture", input: state.input, mode: "suggest", profile_sha256: "fixture", policy_version: "workflow-advice-v1", status: unavailable ? "unavailable" : "suggested", freshness: "current", created_at_ms: Date.now(), latency_ms: 4, error: unavailable ? "Provider unavailable" : null, result: unavailable ? null : { outcome, message: "Inspect the supplied evidence before acting.", confidence: 0.96, selected_probability: 0.97, sufficient_context: 0.95, evidence_id: profile === "artifact-requirement-v1" ? "excerpt-1" : null, model: "jev-1.13.0", input_tokens: 45, probabilities: { [outcome]: 0.97 } } };
        state.records.push(record); data = record;
      } else data = state;
    }
    await route.fulfill({ json: data });
  });
  return calls;
}

async function prepareAndEnable(page: Page) {
  await page.getByLabel("I confirm this evidence and criterion for review.").check();
  await page.getByRole("button", { name: "Prepare review locally" }).click();
  await page.getByRole("button", { name: "Enable suggestions for this revision" }).click();
}

test("workflow assistance recommends, rejects, checks before a run and reviews revision-bound text", async ({ page }) => {
  const calls = await advisoryFixture(page);
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => { if (message.type() === "error") errors.push(message.text()); });
  await page.goto("/");
  await expect(page).toHaveTitle(/OMAR/i);
  expect(new URL(page.url()).pathname).toBe("/");
  await expect(page.locator("vite-error-overlay, nextjs-portal")).toHaveCount(0);
  await page.getByText("Optional workflow assistance", { exact: true }).click();
  await page.getByLabel("Workflow brief").fill("Write an onboarding guide for beginners.");
  await page.getByRole("button", { name: "Browse supported workflows" }).click();
  expect(calls.evaluations).toBe(0);
  await prepareAndEnable(page);
  expect(calls.evaluations).toBe(0);
  await page.getByRole("button", { name: "Suggest a workflow", exact: true }).click();
  await expect(page.getByText("Outcome: writing-review")).toBeVisible();
  await page.getByRole("button", { name: "Reject suggestion" }).click();
  await expect(page.getByText("Outcome: writing-review")).toBeHidden();
  expect(calls.controlWrites).toBe(0);
  await page.getByRole("button", { name: "Use Writing and review" }).click();
  await expect(page.getByRole("group", { name: "Deploy design" })).toBeVisible();
  expect(calls.controlWrites).toBe(0);
  await page.getByLabel("Review stage").selectOption("proposal");
  await prepareAndEnable(page);
  await page.getByRole("button", { name: "Check selected scenario" }).click();
  await expect(page.getByText("Outcome: missing_contract")).toBeVisible();
  expect(calls.controlWrites).toBe(0);
  await page.getByLabel("Review stage").selectOption("artifact");
  await page.getByRole("button", { name: "Load saved revisions" }).click();
  await page.getByRole("combobox", { name: "Workspace", exact: true }).selectOption("workspace-a");
  await page.getByLabel("Saved revision").selectOption("snapshot-1");
  await page.getByLabel("Relative file path").fill("result.md");
  await page.getByRole("button", { name: "Preview saved text" }).click();
  await page.getByLabel("Requirement to confirm").fill("Explain the first step for a beginner.");
  await prepareAndEnable(page);
  await page.getByRole("button", { name: "Review confirmed requirement" }).click();
  await page.getByText("Inspect evidence excerpt-1", { exact: true }).click();
  await expect(page.locator(".advice-card pre")).toHaveText("A guide for beginners.");
  await page.getByLabel("Saved revision").selectOption("snapshot-2");
  await expect(page.getByRole("button", { name: "Review confirmed requirement" })).toBeDisabled();
  await expect(page.getByText("Stale — evidence changed. Prepare a fresh review.", { exact: false })).toBeVisible();
  expect(calls.evaluations).toBe(3);
  expect(calls.controlWrites).toBe(0);
  expect(errors).toEqual([]);
  if (process.env.OMAR_QA_SCREENSHOT) await page.screenshot({ path: process.env.OMAR_QA_SCREENSHOT });
});

test("provider failure leaves normal authoring available and shadow sends no evaluation", async ({ page }) => {
  const calls = await advisoryFixture(page, true);
  await page.goto("/");
  await page.getByText("Optional workflow assistance", { exact: true }).click();
  await page.getByLabel("Workflow brief").fill("Draft a launch brief.");
  await prepareAndEnable(page);
  await page.getByRole("button", { name: "Shadow: keep local" }).click();
  await expect(page.getByRole("button", { name: "Suggest a workflow", exact: true })).toBeDisabled();
  expect(calls.evaluations).toBe(0);
  await page.getByRole("button", { name: "Enable suggestions for this revision" }).click();
  await page.getByRole("button", { name: "Suggest a workflow", exact: true }).click();
  await expect(page.getByText("Provider unavailable", { exact: true })).toBeVisible();
  await expect(page.getByLabel("Describe a workflow")).toBeEnabled();
  await page.getByLabel("Workflow brief").fill("A changed brief");
  await expect(page.getByRole("button", { name: "Suggest a workflow", exact: true })).toBeDisabled();
  expect(calls.evaluations).toBe(1);
  expect(calls.controlWrites).toBe(0);
});

test("mobile workflow assistance remains readable without unsolicited evaluation", async ({ page }) => {
  const calls = await advisoryFixture(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await page.getByText("Optional workflow assistance", { exact: true }).click();
  await page.getByLabel("Workflow brief").fill("Write a guide for beginners.");
  await expect(page.getByRole("button", { name: "Prepare review locally" })).toBeDisabled();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  expect(calls.evaluations).toBe(0);
  if (process.env.OMAR_QA_SCREENSHOT) await page.screenshot({ path: process.env.OMAR_QA_SCREENSHOT.replace(".png", "-mobile.png") });
});
