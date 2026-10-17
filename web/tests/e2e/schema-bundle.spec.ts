import { expect, test } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { FAKE_SERVE_PORT } from "../../playwright.config";
import { startFakeServe } from "../fake-serve.mjs";

test("builder carries the reviewed schema bundle into validation and deployment", async ({ page, request }, testInfo) => {
  const fake = await startFakeServe({ port: FAKE_SERVE_PORT });
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => { if (message.type() === "error") errors.push(message.text()); });
  try {
    const program = await readFile(new URL("../../../tests/topology/src/JevReview.omar", import.meta.url), "utf8");
    const schema = await readFile(new URL("../../../tests/topology/src/schemas/jev-decision.json", import.meta.url), "utf8");
    const files = { "schemas/jev-decision.json": schema };
    await page.goto("/");
    await expect(page).toHaveTitle(/OMAR/);
    await expect(page.getByLabel("Describe a workflow")).toBeEnabled();
    const checked = page.waitForRequest((event) => event.url().endsWith("/v1/programs/check") && event.postDataJSON().program === program);
    const proposed = await request.post(`${fake.url}/v1/agent/proposals`, { data: {
      token: fake.agentToken, summary: "Choose with Jev; reason below 95%.", program, files, inputs: { "checks.request": "Original request ✅" },
    } });
    expect(proposed.ok()).toBe(true);
    expect((await checked).postDataJSON().files).toEqual(files);
    await page.getByRole("button", { name: "Show the source pane" }).click();
    await page.getByRole("button", { name: "View companion files (1)" }).click();
    const imported = page.getByRole("dialog", { name: "Files", exact: true });
    await imported.getByRole("button", { name: "schemas/jev-decision.json", exact: true }).click();
    await expect(imported.getByLabel("File preview")).toContainText('"defer"');
    await page.getByRole("button", { name: "Close files", exact: true }).click();
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await expect(page.locator("vite-error-overlay")).toHaveCount(0);
    await page.screenshot({ path: testInfo.outputPath("schema-review.png"), fullPage: true });
    const deployed = page.waitForRequest((event) => event.url().endsWith("/v1/runs") && event.method() === "POST");
    await page.getByRole("button", { name: "Deploy", exact: true }).click();
    await page.getByRole("button", { name: "Confirm deploy", exact: true }).click();
    expect((await deployed).postDataJSON()).toMatchObject({ program, files, inputs: { "checks.request": "Original request ✅" } });
    await expect(page.getByRole("button", { name: "Confirm deploy", exact: true })).toHaveCount(0);
    expect(errors).toEqual([]);
  } finally { await fake.close(); }
});
