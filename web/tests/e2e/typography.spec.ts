import { expect, test } from "@playwright/test";
import { FAKE_SERVE_PORT } from "../../playwright.config";
import { startFakeServe } from "../fake-serve.mjs";

for (const width of [1440, 390]) {
  test(`readable conversation and controls fit at ${width}px`, async ({ page }) => {
    const fake = await startFakeServe({ port: FAKE_SERVE_PORT });
    try {
      await page.setViewportSize({ width, height: 900 });
      await page.goto("/");
      const composer = page.getByLabel("Describe a workflow");
      await expect(composer).toHaveCSS("font-size", "16px");
      await composer.fill("Review the release plan");
      await page.getByLabel("Draft workflow").click();
      await expect(page.locator(".messages")).toContainText("Which agent should own");
      await expect(page.locator(".message.assistant:not(.progress) .message-body").first()).toHaveCSS("font-size", "16px");
      await expect(page.getByLabel("Draft workflow")).toBeInViewport();
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width);
      const clipped = await page.locator(".conversation-actions button, .prompt-box button").evaluateAll(
        (buttons) => buttons.filter((button) => button.scrollWidth > button.clientWidth + 1 || button.scrollHeight > button.clientHeight + 1).map((button) => button.textContent),
      );
      expect(clipped).toEqual([]);
    } finally {
      await fake.close();
    }
  });
}
