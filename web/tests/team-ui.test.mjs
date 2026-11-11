/**
 * `omar gateway` in a browser: two members of one session sign in with the
 * temporary passwords `omar user add` printed, must choose their own, and
 * then see their own workflows, what the other shares, and (for the admin)
 * the team's counts. Real `omar up`, real gateway, real Mission Control.
 *
 * Needs a runtime built with the bundle in it:
 *   (cd web && npm run build:spa)
 *   cargo build --bin omar --features ui
 *   node --test tests/team-ui.test.mjs
 * Skips itself when that binary is absent or was built without the feature.
 * TEAM_UI_SCREENSHOTS=dir keeps a screenshot of each step; TEAM_UI_VIDEO=dir
 * records each member's browser, slowed to watch, with `starts.json` giving
 * when each recording began.
 */

import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test, { after, before, describe } from "node:test";

import { chromium } from "@playwright/test";

const OMAR_BIN = process.env.OMAR_BIN ?? resolve("../target/debug/omar");
const PORT = Number(process.env.TEAM_UI_PORT ?? 7357);
const ORIGIN = `http://127.0.0.1:${PORT}`;
const SHOTS = process.env.TEAM_UI_SCREENSHOTS;
const VIDEO = process.env.TEAM_UI_VIDEO;
const starts = {};
const root = mkdtempSync(resolve(tmpdir(), "omar-team-ui-"));
const env = { ...process.env, HOME: root, OMAR_HOME: join(root, "omar"), TMUX_TMPDIR: join(root, "tmux") };
delete env.TMUX;
delete env.TMUX_PANE;
mkdirSync(env.TMUX_TMPDIR);

function omar(...args) {
  const result = spawnSync(OMAR_BIN, args, { encoding: "utf8", env, timeout: 60_000 });
  assert.equal(result.status, 0, `omar ${args.join(" ")}: ${result.stderr}`);
  return result.stdout;
}

function bundled() {
  if (!existsSync(OMAR_BIN)) return false;
  const probe = spawnSync(OMAR_BIN, ["serve", "--ui", "--no-ea", "--port", "0"], { encoding: "utf8", env, timeout: 20_000 });
  return !`${probe.stdout}${probe.stderr}`.includes("no UI in it");
}

const AVAILABLE = bundled();

/** `omar user add` prints the temporary password alone on its second line. */
const temporary = (output) => output.split("\n")[1].trim();

async function shot(page, name) {
  if (SHOTS) await page.screenshot({ path: join(SHOTS, `${name}.png`) });
}

async function signIn(browser, user, password, next) {
  const size = { width: 1280, height: 800 };
  starts[user] = Date.now();
  const context = await browser.newContext({ viewport: size, ...(VIDEO ? { recordVideo: { dir: join(VIDEO, user), size } } : {}) });
  const page = await context.newPage();
  await page.goto(ORIGIN);
  await page.getByLabel("Account").fill(user);
  await page.getByLabel("Password").fill(password);
  if (user === "alice") await shot(page, "1-sign-in");
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.getByRole("heading", { name: "Choose your password" }).waitFor();
  if (user === "alice") await shot(page, "2-choose-password");
  await page.getByLabel("Temporary password").fill(password);
  await page.getByLabel("New password", { exact: true }).fill(next);
  await page.getByLabel("New password again").fill(next);
  await page.getByRole("button", { name: "Save password" }).click();
  await page.getByRole("heading", { name: "Workspaces" }).waitFor();
  if (user === "alice") await shot(page, "3-workspaces");
  await page.getByRole("link", { name: /demo/ }).click();
  await page.getByRole("tab", { name: "My workflows" }).waitFor();
  return page;
}

describe("omar gateway", { skip: AVAILABLE ? false : "no bundled runtime" }, () => {
  let gateway;
  let browser;
  let passwords;

  before(async () => {
    omar("up", "-s", "demo", "--no-ea");
    passwords = { alice: temporary(omar("user", "add", "alice")), bob: temporary(omar("user", "add", "bob")) };
    omar("-s", "demo", "member", "add", "alice", "--admin");
    omar("-s", "demo", "member", "add", "bob");
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
    if (VIDEO) writeFileSync(join(VIDEO, "starts.json"), JSON.stringify(starts));
    gateway?.kill();
    spawnSync(OMAR_BIN, ["down", "-s", "demo", "--force", "--timeout", "5"], { env, timeout: 30_000 });
    for (const dir of readdirSync(env.TMUX_TMPDIR)) {
      for (const socket of readdirSync(join(env.TMUX_TMPDIR, dir))) {
        spawnSync("tmux", ["-S", join(env.TMUX_TMPDIR, dir, socket), "kill-server"], { env });
      }
    }
    rmSync(root, { recursive: true, force: true });
  });

  test("members see their own workflows, what others share, and admins the counts", async () => {
    // Nothing behind the gateway answers before signing in.
    assert.equal((await fetch(`${ORIGIN}/s/demo/v1/chats`)).status, 401);

    // A member's first visit opens a workflow of their own.
    const alice = await signIn(browser, "alice", passwords.alice, "alice's own secret");
    const visibility = alice.getByLabel("Who can see this workflow");
    await visibility.waitFor();
    // With no backend signed in here, an admin is shown the Backends page
    // first; a member never is (checked for bob below).
    const backends = alice.getByRole("dialog", { name: "Backends" });
    await backends.waitFor({ timeout: 10_000 });
    await backends.getByRole("button", { name: "Close" }).click();
    assert.equal(await visibility.inputValue(), "private");

    // Private: bob sees nothing of alice's, even under All workflows.
    const bob = await signIn(browser, "bob", passwords.bob, "bob's own secret!");
    await bob.getByRole("tab", { name: "All workflows" }).click();
    const rows = bob.getByRole("list", { name: "Saved chats" }).getByRole("listitem");
    await assert.rejects(rows.filter({ hasText: "alice" }).first().waitFor({ timeout: 3000 }));

    // Shared with the team: bob sees it, marked hers and read-only.
    await visibility.selectOption("team");
    await alice.getByRole("list", { name: "Saved chats" }).getByText("Team").waitFor();
    await shot(alice, "4-shared-with-team");
    await rows.filter({ hasText: "alice" }).first().waitFor({ timeout: 10_000 });
    await rows.filter({ hasText: "alice" }).first().getByRole("button").first().click();
    await bob.getByText("alice's · read-only").waitFor();
    assert.equal(await bob.getByLabel("Who can see this workflow").count(), 0);
    await shot(bob, "5-teammate-read-only");
    await bob.getByRole("tab", { name: "My workflows" }).click();
    await assert.rejects(rows.filter({ hasText: "alice" }).first().waitFor({ timeout: 2000 }));

    // The admin's team page: counts, no content.
    assert.equal(await bob.getByRole("dialog", { name: "Backends" }).count(), 0);
    await alice.getByRole("button", { name: "Settings" }).click();
    await alice.getByRole("menuitem", { name: "Team…" }).click();
    const members = alice.getByRole("table", { name: "Members" });
    await members.getByRole("row", { name: /alice/ }).waitFor();
    const row = async (user) => {
      const cells = members.getByRole("row", { name: new RegExp(user) }).getByRole("cell");
      return [await cells.nth(0).innerText(), await cells.nth(1).getByRole("combobox").inputValue(), await cells.nth(2).innerText(), await cells.nth(3).innerText()];
    };
    // Counts: chats, then live workflows.
    assert.deepEqual(await row("alice"), ["alice (you)", "admin", "1", "0"]);
    assert.deepEqual(await row("bob"), ["bob", "member", "1", "0"]);
    await shot(alice, "6-admin-team-page");

    // Bob is no admin: no Team page for him.
    await bob.getByRole("button", { name: "Settings" }).click();
    assert.equal(await bob.getByRole("menuitem", { name: "Team…" }).count(), 0);
    assert.equal(await bob.getByRole("menuitem", { name: "Sign out" }).count(), 1);
  });
});
