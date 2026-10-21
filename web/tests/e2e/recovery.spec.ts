import { expect, test } from "@playwright/test";
import { startFakeServe } from "../fake-serve.mjs";
import { FAKE_SERVE_PORT } from "../../playwright.config";

test("automatic recovery reports progress without runtime notices ending the turn", async ({ page }) => {
  const fake = await startFakeServe({ port: FAKE_SERVE_PORT });
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    await page.goto("/");
    await expect(page.getByLabel("Describe a workflow")).toBeVisible();
    await expect(page.locator(".daemon")).toHaveClass(/live/);
    fake.publishRuntime("The workflow stopped because Cargo is unavailable. OMAR will investigate automatically.");
    const reply = (text: string, progress: boolean) => page.request.post(`${fake.url}/v1/agent/reply`, {
      data: { token: fake.agentToken, text, progress },
    });
    await reply("I'm trying to solve the workflow problem. Checking the Rust toolchain now.", true);
    await expect(page.locator(".messages")).toContainText("trying to solve");
    await expect(page.getByLabel("Draft workflow")).toBeDisabled();
    fake.publishRuntime("Your response is needed. Open the response panel, complete the task in ChatGPT, then submit the result here.");
    await expect(page.locator(".messages")).toContainText("Your response is needed");
    await expect(page.getByLabel("Draft workflow")).toBeDisabled();
    const runtimeNotice = page.locator(".message").filter({ hasText: "Your response is needed" });
    await expect(runtimeNotice.locator(".message-author")).toHaveText("OMAR");
    await page.getByRole("button", { name: "Inspect terminal" }).click();
    await expect(page.getByRole("dialog", { name: /Terminal/i })).toBeVisible();
    await page.getByRole("dialog", { name: /Terminal/i }).getByRole("button", { name: "Close terminal", exact: true }).click();
    await reply("I found Cargo and verified the build. Next: choose Run again to retry the workflow.", false);
    await page.getByLabel("Describe a workflow").fill("What should I do next?");
    await expect(page.getByLabel("Draft workflow")).toBeEnabled();
    expect(errors).toEqual([]);
  } finally {
    await fake.close();
  }
});
