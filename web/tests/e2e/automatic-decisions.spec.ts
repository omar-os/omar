import { expect, test } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { FAKE_SERVE_PORT } from "../../playwright.config";
import { startFakeServe } from "../fake-serve.mjs";

test("automatic gate explains escalation and preserves the final revision route", async ({ page, request }) => {
  const fake = await startFakeServe({ port: FAKE_SERVE_PORT, snapshot: "automatic-decisions.v1.json", stepMs: 1000 });
  try {
    const program = await readFile(new URL("../../../examples/assistance/automatic_requirement.omar", import.meta.url), "utf8");
    await page.goto("/");
    await expect(page.getByLabel("Describe a workflow")).toBeEnabled();
    await request.post(`${fake.url}/v1/agent/proposals`, { data: { token: fake.agentToken, summary: "Check the opening before the next step.", program, inputs: { "check.draft": "Example announced its launch." } } });
    const decisions = page.getByLabel("Automatic workflow decisions");
    await expect(decisions).toContainText("Jev check · reasoning if needed");
    await decisions.locator("summary").click();
    await expect(decisions).toContainText("Requirement not met → check.revise");
    await page.getByRole("button", { name: "Deploy", exact: true }).click();
    await page.getByRole("button", { name: "Confirm deploy", exact: true }).click();
    await expect(decisions).toContainText("Reasoning review");
    await expect(decisions).toContainText("Jev confidence 0.810 is below 0.95");
    await expect(decisions).toContainText("Reasoning decided");
    await expect(decisions).toContainText("The opening omits the founder.");
    await expect(decisions).toContainText("→ check.revise");
    await expect(decisions).toContainText("Evidence sufficiency 0.990");
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(decisions).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  } finally { await fake.close(); }
});
