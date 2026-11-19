import { expect, test } from "@playwright/test";
import { FAKE_SERVE_PORT } from "../../playwright.config";
import { startFakeServe } from "../fake-serve.mjs";

test("saved answers require revision; reviewed model choices persist and gate deployment", async ({ page }) => {
  const fake = await startFakeServe({ port: FAKE_SERVE_PORT, workflowPlanning: true });
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  try {
    await page.goto("/");
    await expect(page.locator(".daemon")).toHaveClass(/live/);
    await page.getByLabel("Describe a workflow").fill("Review the release plan");
    await page.getByLabel("Draft workflow").click();
    await expect(page.locator(".messages")).toContainText("Which agent should own");
    await page.getByLabel("Describe a workflow").fill("The planner");
    await page.getByLabel("Draft workflow").click();
    const review = page.getByRole("region", { name: "Workflow review" });
    const deploy = page.getByRole("button", { name: "Deploy", exact: true });
    await expect(review).toContainText("A grounded release recommendation");
    await expect(deploy).toBeDisabled();
    await review.getByRole("button", { name: "View affected step" }).click();
    await expect(page.getByRole("complementary", { name: "Step details" })).toContainText("TASK");
    await page.getByRole("button", { name: "Close step details" }).click();
    await review.getByLabel("Who will use this brief?").fill("Product team");
    await review.getByRole("button", { name: "Save review" }).click();
    await expect(review).toContainText("Answers saved");
    await page.reload();
    await expect(review.getByLabel("Who will use this brief?")).toHaveValue("Product team");
    await expect(deploy).toBeDisabled();
    await review.getByRole("button", { name: "Update workflow from answers" }).click();
    await expect(review.getByLabel("Who will use this brief?")).toHaveCount(0);
    await review.getByText("Agents and model choices", { exact: true }).click();
    await review.getByLabel("Model for flow.planner", { exact: true }).selectOption("codex-economy");
    await review.getByRole("checkbox", { name: /This brief is for an internal audience/ }).check();
    await review.getByRole("checkbox", { name: "I reviewed the workflow and model choices" }).check();
    await review.getByRole("button", { name: "Save review" }).click();
    await expect(review).toContainText("Ready for confirmation");
    await page.reload();
    await review.getByText("Agents and model choices", { exact: true }).click();
    await expect(review.getByLabel("Model for flow.planner", { exact: true })).toHaveValue("codex-economy");
    await expect(deploy).toBeEnabled();
    await review.getByLabel("Model for flow.planner", { exact: true }).selectOption("codex-quality");
    await expect(deploy).toBeDisabled();
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    expect(errors).toEqual([]);
  } finally { await page.close(); await fake.close(); }
});
