/**
 * `omar gateway` in a browser: the operator signs in with the password set by
 * `omar passwd`, picks a session and gets its Mission Control, as on the
 * machine itself, then signs out. Real `omar up`, real gateway.
 *
 * Needs a runtime built with the bundle in it:
 *   (cd web && npm run build:spa)
 *   cargo build --bin omar --features ui
 *   node --test tests/gateway-ui.test.mjs
 * Skips itself when that binary is absent or was built without the feature.
 * GATEWAY_UI_SCREENSHOTS=dir keeps a screenshot of each step;
 * GATEWAY_UI_VIDEO=dir records the browser, slowed to watch.
 */

import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test, { after, before, describe } from "node:test";

import { chromium } from "@playwright/test";

const OMAR_BIN = process.env.OMAR_BIN ?? resolve("../target/debug/omar");
const PORT = Number(process.env.GATEWAY_UI_PORT ?? 7358);
const ORIGIN = `http://127.0.0.1:${PORT}`;
const SHOTS = process.env.GATEWAY_UI_SCREENSHOTS;
const VIDEO = process.env.GATEWAY_UI_VIDEO;
const PASSWORD = "correct horse battery";
const root = mkdtempSync(resolve(tmpdir(), "omar-gateway-ui-"));
const env = { ...process.env, HOME: root, OMAR_HOME: join(root, "omar"), TMUX_TMPDIR: join(root, "tmux") };
delete env.TMUX;
delete env.TMUX_PANE;
mkdirSync(env.TMUX_TMPDIR);

function omar(args, input) {
  const result = spawnSync(OMAR_BIN, args, { encoding: "utf8", env, input, timeout: 60_000 });
  assert.equal(result.status, 0, `omar ${args.join(" ")}: ${result.stderr}`);
  return result.stdout;
}

function bundled() {
  if (!existsSync(OMAR_BIN)) return false;
  const probe = spawnSync(OMAR_BIN, ["serve", "--ui", "--no-ea", "--port", "0"], { encoding: "utf8", env, timeout: 20_000 });
  return !`${probe.stdout}${probe.stderr}`.includes("no UI in it");
}

const AVAILABLE = bundled();

/** With no backend signed in (as on CI), Mission Control opens the Backends
    page first; it is the same page as on the machine, so close it. */
async function closeBackends(page) {
  const backends = page.getByRole("dialog", { name: "Backends" });
  await backends.waitFor({ timeout: 5_000 }).then(
    () => backends.getByRole("button", { name: "Close" }).click(),
    () => {},
  );
}

async function shot(page, name) {
  if (SHOTS) await page.screenshot({ path: join(SHOTS, `${name}.png`) });
}

describe("omar gateway", { skip: AVAILABLE ? false : "no bundled runtime" }, () => {
  let gateway;
  let browser;

  before(async () => {
    omar(["up", "-s", "demo", "--no-ea"]);
    omar(["passwd"], `${PASSWORD}\n`);
    gateway = spawn(OMAR_BIN, ["gateway", "--address", `127.0.0.1:${PORT}`], { env, stdio: "ignore" });
    for (let attempt = 0; attempt < 100; attempt += 1) {
      const up = await fetch(`${ORIGIN}/auth/me`).then(() => true, () => false);
      if (up) break;
      await new Promise((settle) => setTimeout(settle, 100));
    }
    browser = await chromium.launch(VIDEO ? { slowMo: 250 } : {});
  });

  after(async () => {
    for (const context of browser?.contexts() ?? []) await context.close();
    await browser?.close();
    gateway?.kill();
    spawnSync(OMAR_BIN, ["down", "-s", "demo", "--force", "--timeout", "5"], { env, timeout: 30_000 });
    // Only this test's own tmux servers, by socket.
    for (const dir of readdirSync(env.TMUX_TMPDIR)) {
      for (const socket of readdirSync(join(env.TMUX_TMPDIR, dir))) {
        spawnSync("tmux", ["-S", join(env.TMUX_TMPDIR, dir, socket), "kill-server"], { env });
      }
    }
    rmSync(root, { recursive: true, force: true });
  });

  test("the operator signs in, opens a session's Mission Control, and signs out", async () => {
    // Nothing behind the gateway answers before signing in.
    assert.equal((await fetch(`${ORIGIN}/s/demo/v1/chats`)).status, 401);

    const size = { width: 1280, height: 800 };
    const context = await browser.newContext({ viewport: size, ...(VIDEO ? { recordVideo: { dir: VIDEO, size } } : {}) });
    const page = await context.newPage();
    await page.goto(`${ORIGIN}/s/demo/`);
    const password = page.getByLabel("Password");
    await password.fill("not the password");
    await page.getByRole("button", { name: "Sign in" }).click();
    await page.getByRole("alert").filter({ hasText: "wrong password" }).waitFor();
    await shot(page, "1-wrong-password");
    await password.fill(PASSWORD);
    await page.getByRole("button", { name: "Sign in" }).click();

    // Straight into the session the address named, served by its daemon.
    const workflows = page.getByRole("list", { name: "Saved chats" });
    await workflows.getByRole("listitem").first().waitFor({ timeout: 15_000 });
    await closeBackends(page);
    await shot(page, "2-mission-control");

    // The menu leads back to the session list, and out.
    await page.getByRole("button", { name: "Settings" }).click();
    await shot(page, "3-menu");
    await page.getByRole("menuitem", { name: "Switch session" }).click();
    const sessions = page.getByRole("list", { name: "Sessions" });
    await sessions.getByRole("link", { name: /demo\s*Running/ }).waitFor();
    await shot(page, "4-sessions");
    await sessions.getByRole("link", { name: /demo/ }).click();
    await workflows.getByRole("listitem").first().waitFor({ timeout: 15_000 });
    await closeBackends(page);
    await page.getByRole("button", { name: "Settings" }).click();
    await page.getByRole("menuitem", { name: "Sign out" }).click();
    await page.getByRole("button", { name: "Sign in" }).waitFor();
    assert.equal((await page.request.get(`${ORIGIN}/s/demo/v1/chats`)).status(), 401);
    if (VIDEO) await page.waitForTimeout(1500);
    await context.close();
  });
});
