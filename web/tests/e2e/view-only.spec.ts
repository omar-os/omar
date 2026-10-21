import { expect, test } from "@playwright/test";
import { FAKE_SERVE_PORT, FAKE_SERVE_URL } from "../../playwright.config";
import { startFakeServe } from "../fake-serve.mjs";

type FakeServe = { url: string; viewUrl: string; viewTerminalFrames: number; close(): Promise<void> };

let fake: FakeServe;

// The app is launched against the fixed port, so that is the view port here;
// the full port is where the test, standing in for the operator, sets things up.
test.beforeEach(async () => {
  fake = (await startFakeServe({ stepMs: 1500, port: 0, viewPort: FAKE_SERVE_PORT })) as FakeServe;
});

test.afterEach(async () => {
  await fake.close();
});

async function post(path: string, body: object) {
  const response = await fetch(`${fake.url}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  expect(response.ok).toBe(true);
  return response.json();
}

test("a view port watches conversations and runs without offering a write", async ({ page }) => {
  // One chat with a live run, then a second that becomes the daemon's active one.
  const { active_id: first } = await (await fetch(`${fake.url}/v1/chats`)).json();
  await post(`/chats/${first}/v1/chat`, { text: "Review the release plan" });
  await post(`/chats/${first}/v1/runs`, { program: "main ReviewFlow {}", inputs: {} });
  await post("/v1/chats", {});
  await post("/v1/chat", { text: "Prepare a product launch" });

  const requests: { method: string; url: string }[] = [];
  page.on("request", (request) => requests.push({ method: request.method(), url: request.url() }));
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));

  await page.goto("/");
  await expect(page.locator(".daemon")).toHaveClass(/live/);
  await expect(page.getByRole("status").filter({ hasText: "View only" })).toBeVisible();
  await expect(page.locator(".messages")).toContainText("Prepare a product launch");
  // Nothing that writes is offered.
  await expect(page.getByLabel("Describe a workflow")).toHaveCount(0);
  await expect(page.getByLabel("Draft workflow")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "New workflow" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Files & versions" })).toHaveCount(0);
  await expect(page.locator(".backend-menu")).toHaveCount(0);

  // Selecting a conversation stays in this tab: no activate.
  const history = page.getByRole("complementary", { name: "Chat history" });
  await history.getByRole("button", { name: /Review the release plan/ }).click();
  await expect(page.locator(".messages")).toContainText("Review the release plan");
  await expect(history.locator('[aria-current="true"]')).toContainText("Review the release plan");

  // The run is drawn and followed through the relay.
  await expect(page.locator(".omar-reaction")).toHaveCount(3);
  await expect(page.locator(".connection")).toHaveText("observing");
  await page.getByRole("button", { name: "Show the source pane" }).click();
  await expect(page.locator(".event-strip")).toContainText("reaction started", { timeout: 15_000 });
  await expect(page.getByRole("group", { name: "Run controls" })).toHaveCount(0);
  await expect(page.getByRole("group", { name: "Deploy design" })).toHaveCount(0);
  await page.getByRole("tab", { name: "Source" }).click();
  await expect(page.getByLabel("OMAR program")).toHaveAttribute("readonly", "");

  // A terminal can be watched, not typed into or resized.
  await page.getByRole("button", { name: "Inspect terminal" }).click();
  const terminal = page.getByRole("dialog", { name: "Terminal for the assistant" });
  await expect(terminal).toContainText("the assistant $");
  await expect(terminal).toContainText("View only");
  await terminal.locator(".terminal-screen").click();
  await page.keyboard.type("rm -rf /");
  await page.setViewportSize({ width: 900, height: 600 });
  await page.waitForTimeout(500);
  expect(fake.viewTerminalFrames).toBe(0);
  await page.getByRole("button", { name: "Close terminal" }).click();

  // Every request went to the view port, read only, and the run's loopback
  // diagram server was never asked directly.
  const daemon = requests.filter((request) => request.url.startsWith("http") && !request.url.startsWith("http://127.0.0.1:3100"));
  expect(daemon.length).toBeGreaterThan(0);
  for (const request of daemon) {
    expect(request.url.startsWith(FAKE_SERVE_URL), request.url).toBe(true);
    expect(request.method, request.url).toBe("GET");
  }
  expect(daemon.some((request) => /\/v1\/runs\/[^/]+\/diagram\/v1\/diagram$/.test(request.url))).toBe(true);
  expect(errors).toEqual([]);
});
