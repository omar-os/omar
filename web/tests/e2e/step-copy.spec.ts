import { expect, test } from "@playwright/test";
import { FAKE_SERVE_PORT } from "../../playwright.config";
import { startFakeServe } from "../fake-serve.mjs";

test("descriptive titles survive reload and full explanations are readable in step details", async ({ page }) => {
  const fake = await startFakeServe({port: FAKE_SERVE_PORT, taskMetadata: [
    {title: "Draft a release plan", description: "Turn the supplied request into a concrete release plan for review."},
    {title: "Check the plan for risks", description: "Review the proposed release plan for risks and omissions. Return findings for the final revision."},
    {title: "Incorporate the review findings", description: "Use the review findings to revise the plan and produce the final answer."},
  ]});
  try {
    const errors: string[] = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.setViewportSize({width:1512, height:900});
    await page.goto("/");
    await page.getByLabel("Describe a workflow").fill("Review the release plan");
    await page.getByLabel("Draft workflow").click();
    await expect(page.locator(".messages")).toContainText("Which agent should own");
    await page.getByLabel("Describe a workflow").fill("The planner");
    await page.getByLabel("Draft workflow").click();
    const card = page.locator(".omar-reaction").nth(1);
    await expect(card.locator(".omar-reaction-name")).toHaveText("Check the plan for risks");
    await card.press("Enter");
    const inspector = page.getByRole("complementary", {name:"Step details"});
    await expect(inspector.getByRole("heading", {name:"Check the plan for risks"})).toBeVisible();
    await expect(inspector.locator(".inspector-description")).toContainText("Return findings for the final revision.");
    await expect(card.locator("title").first()).toContainText("Return findings for the final revision.");
    await inspector.getByLabel("Close step details").click();
    const bounds = await card.evaluate(node => {
      const text = node.querySelector(".omar-reaction-name") as SVGGraphicsElement;
      const body = node.querySelector(".omar-reaction-body") as SVGGraphicsElement;
      const a = text.getBBox(), b = body.getBBox();
      return {textRight:a.x+a.width, cardRight:b.x+b.width, textBottom:a.y+a.height};
    });
    expect(bounds.textRight).toBeLessThan(bounds.cardRight);
    expect(bounds.textBottom).toBeLessThan(60);
    await page.reload();
    await expect(page.locator(".omar-reaction-name").nth(1)).toHaveText("Check the plan for risks");
    await page.setViewportSize({width:390, height:844});
    await page.locator(".omar-reaction").nth(1).press("Enter");
    await expect(inspector.locator(".inspector-description")).toContainText("Return findings for the final revision.");
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    expect(errors).toEqual([]);
  } finally { await fake.close(); }
});
