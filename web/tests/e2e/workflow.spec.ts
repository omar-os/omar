import { expect, test } from "@playwright/test";
import { FAKE_SERVE_PORT } from "../../playwright.config";
import { startFakeServe } from "../fake-serve.mjs";
import { readFile } from "node:fs/promises";

for (const status of ["current", "expired"] as const) {
  test(`workflow profile facts show ${status} evidence without inferring run cost`, async ({page}) => {
    const fake=await startFakeServe({port:FAKE_SERVE_PORT,workflowDesign:true,workflowFacts:status,workflowFactsLifetimeSeconds:4});
    try {
      await page.goto("/");
      await page.getByLabel("Describe a workflow").fill("Review the release plan");
      await page.getByLabel("Draft workflow").click();
      await expect(page.locator(".messages")).toContainText("Which agent should own");
      await page.getByLabel("Describe a workflow").fill("The planner");
      await page.getByLabel("Draft workflow").click();
      const review=page.getByRole("region",{name:"Workflow review"});
      await review.getByRole("button",{name:"View affected step: Review evidence"}).click();
      const inspector=page.getByRole("complementary",{name:"Step details"});
      await inspector.getByText("Limits and pricing",{exact:true}).click();
      await expect(inspector).toContainText(`Advertised limits: ${status}`);
      await expect(inspector).toContainText("do not establish this run’s cost");
      if(status==="current") {
        await expect(inspector).toContainText("Context window: 8,000 tokens");
        await expect(inspector).toContainText("$0.001 per 1,000 input token(s)");
        await expect(inspector).toContainText("Quota observation: 0 remaining / 10 fixture calls");
        await expect(inspector).toContainText("Synthetic data for UI testing only.");
      } else {
        await expect(inspector).toContainText("Context window: Unknown tokens");
        await expect(inspector).toContainText("No current unit prices are known.");
        await expect(inspector).not.toContainText("$0.001");
        await expect(inspector).toContainText("Remaining quota is unknown.");
      }
      await inspector.getByText("Pricing and quota evidence:",{exact:false}).scrollIntoViewIfNeeded();
      await page.screenshot({path:`/private/tmp/omar-workflow-profile-facts-${status}.png`,fullPage:true});
      if(status==="current") {
        await expect(inspector).toContainText("Pricing and quota evidence: expired", {timeout:6000});
        await expect(inspector).not.toContainText("$0.001");
        await expect(inspector).toContainText("Context window: Unknown tokens");
      }
    } finally {await fake.close();}
  });
}

test("workflow tool access blocks launch and remains visible after a model override", async ({page}) => {
  const fake=await startFakeServe({port:FAKE_SERVE_PORT,workflowDesign:true,workflowToolBlocked:true});
  try {
    await page.goto("/");
    await page.getByLabel("Describe a workflow").fill("Research the release plan");
    await page.getByLabel("Draft workflow").click();
    await expect(page.locator(".messages")).toContainText("Which agent should own");
    await page.getByLabel("Describe a workflow").fill("The planner");
    await page.getByLabel("Draft workflow").click();
    const review=page.getByRole("region",{name:"Workflow review"});
    await review.getByRole("checkbox").check();
    await review.getByLabel("Who will use this brief?").fill("Product team");
    await review.getByRole("button",{name:"Save review",exact:true}).click();
    await review.getByRole("button",{name:"Update workflow from answers"}).click();
    await expect(review.getByLabel("Who will use this brief?")).toHaveCount(0);
    await review.getByRole("checkbox").check();
    await review.getByRole("button",{name:"Save review",exact:true}).click();
    await page.locator(".omar-reaction").first().press("Enter");
    const inspector=page.getByRole("complementary",{name:"Step details"});
    await expect(inspector).toContainText("Tool access needs review before running.");
    await inspector.getByLabel("Step model profile").selectOption("codex-economy");
    await inspector.getByRole("button",{name:"Apply model choice"}).click();
    await expect(inspector).toContainText("Saved: gpt-6-luna");
    await expect(inspector).toContainText("no reviewed connection attempt");
    await expect(page.getByRole("button",{name:"Deploy",exact:true})).toBeDisabled();
    await inspector.getByText("Selection evidence",{exact:true}).click();
    await expect(inspector).toContainText("web: unknown");
    await inspector.getByText("web: unknown",{exact:true}).scrollIntoViewIfNeeded();
    await page.screenshot({path:"/private/tmp/omar-workflow-tool-access.png",fullPage:true});
    await inspector.getByRole("button",{name:"Close step details"}).click();
    await review.getByText("Agents and model choices",{exact:true}).click();
    await expect(review).toContainText("Required tool 'web'");
  } finally { await fake.close(); }
});

test("workflow review persists answers, requires redesign, binds model settings and shows receipts", async ({page}) => {
  const fake=await startFakeServe({port:FAKE_SERVE_PORT,workflowDesign:true,workflowRepositoryChecks:true});
  const errors:string[]=[];page.on("pageerror", error=>errors.push(error.message));
  try {
    await page.goto("/");
    await page.getByLabel("Describe a workflow").fill("Review the release plan");
    await page.getByLabel("Draft workflow").click();
    await expect(page.locator(".messages")).toContainText("Which agent should own");
    await page.getByLabel("Describe a workflow").fill("The planner");
    await page.getByLabel("Draft workflow").click();
    const review=page.getByRole("region",{name:"Workflow review"});
    await expect(review).toContainText("A grounded release recommendation");
    await expect(review).toContainText("System proposed");
    await review.getByText("Research used",{exact:true}).click();
    await expect(review).toContainText("Catalog reads: 2/2. Source reads: 2/3.");
    await expect(review).toContainText("Designer cost is unknown.");
    await expect(review).toContainText("External retrieval is unavailable");
    await expect(review).toContainText("partial source (20,000 bytes total");
    await expect(review).toContainText("Research gap: external:");
    await page.screenshot({path:"/private/tmp/omar-workflow-research-budget.png",fullPage:true});
    await review.getByText("Research used",{exact:true}).click();
    await review.getByText("What a good result looks like",{exact:true}).click();
    await expect(review).toContainText("Source observed · input flow.request");
    await expect(review).toContainText("Recorded with this revision");
    await review.getByText("What a good result looks like",{exact:true}).click();
    await review.getByRole("button",{name:"View affected step: Review evidence"}).click();
    await expect(page.getByRole("complementary",{name:"Step details"})).toBeVisible();
    await expect(page.getByRole("complementary",{name:"Step details"}).getByLabel("Step prompt")).toBeVisible();
    await page.getByRole("complementary",{name:"Step details"}).getByText("Data and limits",{exact:true}).click();
    await expect(page.getByRole("complementary",{name:"Step details"})).toContainText("Separate evidence review");
    await expect(page.getByRole("complementary",{name:"Step details"})).toContainText("input ≤ 262,144 bytes");
    await expect(page.getByRole("complementary",{name:"Step details"})).toContainText("native session history");
    await page.getByRole("complementary",{name:"Step details"}).getByText("Per invocation:",{exact:false}).scrollIntoViewIfNeeded();
    await page.screenshot({path:"/private/tmp/omar-workflow-step-contract.png",fullPage:true});
    await page.getByRole("complementary",{name:"Step details"}).getByText("Data and limits",{exact:true}).click();
    await page.getByRole("complementary",{name:"Step details"}).getByText("Checks and recovery",{exact:true}).click();
    await expect(page.getByRole("complementary",{name:"Step details"})).toContainText("Up to 1 invocation(s) per run.");
    await expect(page.getByRole("complementary",{name:"Step details"})).toContainText("No automatic repair is declared for this step.");
    await page.getByRole("button",{name:"Close step details"}).click();
    await expect(page.getByRole("button",{name:"Deploy",exact:true})).toBeDisabled();
    await review.getByRole("checkbox").check();
    await review.getByLabel("Who will use this brief?").fill("Product team");
    await review.getByRole("button",{name:"Save review",exact:true}).click();
    await expect(review).toContainText("Revision 2");
    await expect(review).toContainText("User confirmed · saved review revision 2");
    await page.reload();
    await expect(review.getByLabel("Who will use this brief?")).toHaveValue("Product team");
    await expect(review.getByRole("checkbox")).toBeChecked();
    await review.getByRole("button",{name:"Update workflow from answers"}).click();
    await expect(review.getByLabel("Who will use this brief?")).toHaveCount(0);
    await review.getByRole("checkbox").check();
    await review.getByText("Agents and model choices",{exact:true}).click();
    await review.getByRole("combobox").first().selectOption("codex-economy");
    await review.getByRole("button",{name:"Save review",exact:true}).click();
    await expect(page.getByRole("button",{name:"Deploy",exact:true})).toBeEnabled();
    await page.locator(".omar-reaction").first().press("Enter");
    const inspector=page.getByRole("complementary",{name:"Step details"});
    await expect(inspector.getByLabel("Step prompt")).toBeVisible();
    await expect(inspector.getByLabel("Step model profile")).toHaveValue("codex-economy");
    await inspector.getByText("Model access: unknown",{exact:true}).click();
    await expect(inspector).toContainText("Installation and login status do not verify model access.");
    await inspector.getByText("Model access: unknown",{exact:true}).click();
    await expect(inspector).toContainText("This agent runs 2 steps");
    await inspector.getByLabel("Step model profile").selectOption("codex-quality");
    await expect(page.getByRole("button",{name:"Deploy",exact:true})).toBeDisabled();
    await inspector.getByRole("button",{name:"Apply model choice"}).click();
    await expect(inspector).toContainText("Saved: gpt-6.1-sol");
    await inspector.getByLabel("Step model profile").selectOption("codex-economy");
    await inspector.getByRole("button",{name:"Apply model choice"}).click();
    await expect(inspector).toContainText("Saved: gpt-6-luna");
    await page.screenshot({path:"/private/tmp/omar-workflow-model-choice.png",fullPage:true});
    await inspector.getByRole("button",{name:"Close step details"}).click();
    await page.getByRole("button",{name:"Files & versions"}).click();
    const files=page.getByRole("dialog",{name:"Files & versions"});
    const downloading=page.waitForEvent("download");
    await files.getByRole("button",{name:"Export workflow bundle"}).click();
    const downloaded=await downloading;
    const bundlePath="/private/tmp/omar-workflow-roundtrip.omar-workflow.json";
    await downloaded.saveAs(bundlePath);
    await files.getByRole("button",{name:"Close",exact:true}).click();
    await page.getByRole("button",{name:"New workflow",exact:true}).click();
    await page.getByRole("button",{name:"Files & versions"}).click();
    const incompatible=JSON.parse(await readFile(bundlePath,"utf8"));
    incompatible.profiles["flow.planner"].model="different-model";
    await files.getByLabel("Workflow bundle file").setInputFiles({name:"incompatible.json",mimeType:"application/json",buffer:Buffer.from(JSON.stringify(incompatible))});
    await expect(files.getByRole("alert")).toContainText("differs from destination");
    await files.getByLabel("Workflow bundle file").setInputFiles(bundlePath);
    await expect(files).toBeHidden();
    await expect(review).toContainText("Revision 1");
    await review.getByText("Research used",{exact:true}).click();
    await expect(review).toContainText("No metered research record is attached");
    await review.getByText("Research used",{exact:true}).click();
    await expect(review.getByRole("checkbox")).not.toBeChecked();
    await review.getByText("Agents and model choices",{exact:true}).click();
    await expect(review.getByRole("combobox").first()).toHaveValue("codex-economy");
    await review.getByRole("checkbox").check();
    await review.getByRole("button",{name:"Save review",exact:true}).click();
    await review.getByRole("button",{name:"Refresh model choices",exact:true}).click();
    await expect(review).toContainText("Revision 3");
    await expect(page.getByRole("button",{name:"Deploy",exact:true})).toBeEnabled();
    await page.screenshot({path:"/private/tmp/omar-workflow-review-desktop.png",fullPage:true});
    await page.getByRole("button",{name:"Deploy",exact:true}).click();
    await page.getByRole("button",{name:"Confirm deploy"}).click();
    await page.getByText("Execution evidence",{exact:true}).click();
    await expect(page.locator(".workflow-review")).toContainText(["gpt-6-luna"]);
    await page.getByText("Repository regression checks",{exact:true}).click();
    await expect(page.locator("details.workflow-review")).toContainText("passed · 15 declared cases");
    await expect(page.locator("details.workflow-review")).toContainText("does not establish overall code quality");
    await page.getByText("Model access evidence",{exact:true}).click();
    await page.getByRole("button",{name:"Inspect access evidence for flow.planner",exact:true}).click();
    const executionEvidence=page.locator("details.workflow-review");
    await expect(executionEvidence).toContainText("insufficient evidence");
    await expect(executionEvidence).toContainText("Native requests made by this inspection: 0");
    await expect(executionEvidence).toContainText("Reported model: Unknown");
    await page.getByText("Model access evidence",{exact:true}).scrollIntoViewIfNeeded();
    await page.screenshot({path:"/private/tmp/omar-model-access-evidence.png",fullPage:true});
    await page.setViewportSize({width:390,height:844});
    await page.screenshot({path:"/private/tmp/omar-workflow-review-mobile.png",fullPage:true});
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    expect(errors).toEqual([]);
  } finally {await fake.close();}
});

test("workflow research issues keep launch blocked after the other review steps are complete", async ({page}) => {
  const fake=await startFakeServe({port:FAKE_SERVE_PORT,workflowDesign:true,workflowIssues:[{
    id:"missing-policy",kind:"missing_information",description:"The applicable release policy has not been supplied.",source_ids:[],blocking:true,resolution:null,
  }]});
  try {
    await page.goto("/");
    await page.getByLabel("Describe a workflow").fill("Review the release plan");
    await page.getByLabel("Draft workflow").click();
    await expect(page.locator(".messages")).toContainText("Which agent should own");
    await page.getByLabel("Describe a workflow").fill("The planner");
    await page.getByLabel("Draft workflow").click();
    const review=page.getByRole("region",{name:"Workflow review"});
    await review.getByRole("checkbox").check();
    await review.getByLabel("Who will use this brief?").fill("Product team");
    await review.getByRole("button",{name:"Save review",exact:true}).click();
    await review.getByRole("button",{name:"Update workflow from answers"}).click();
    await expect(review.getByLabel("Who will use this brief?")).toHaveCount(0);
    await review.getByRole("checkbox").check();
    await review.getByRole("button",{name:"Save review",exact:true}).click();
    await expect(review).toContainText("User confirmed");
    await expect(review).toContainText("Missing information · needs a decision");
    await expect(review.getByRole("button",{name:"Resolve research issues with OMAR"})).toBeEnabled();
    await expect(page.getByRole("button",{name:"Deploy",exact:true})).toBeDisabled();
    await review.getByText("What a good result looks like",{exact:true}).click();
    await page.screenshot({path:"/private/tmp/omar-workflow-provenance.png",fullPage:true});
  } finally {await fake.close();}
});


test("published benchmark details retain source scope and expire without qualifying a default", async ({page}) => {
  const fake=await startFakeServe({port:FAKE_SERVE_PORT,workflowDesign:true,workflowBenchmark:true});
  try {
    await page.goto("/");
    await page.getByLabel("Describe a workflow").fill("Review the release plan");
    await page.getByLabel("Draft workflow").click();
    await expect(page.locator(".messages")).toContainText("Which agent should own");
    await page.getByLabel("Describe a workflow").fill("The planner");
    await page.getByLabel("Draft workflow").click();
    const review=page.getByRole("region",{name:"Workflow review"});
    await review.getByRole("button",{name:"View affected step: Review evidence"}).click();
    const inspector=page.getByRole("complementary",{name:"Step details"});
    await inspector.getByText("Selection evidence",{exact:true}).click();
    await inspector.getByText("Published benchmark: candidate",{exact:true}).click();
    await expect(inspector).toContainText("Published score: 42 percent");
    await expect(inspector).toContainText("They do not qualify a default");
    await expect(inspector).toContainText("Effort: Unknown");
    await expect(inspector).toContainText("Synthetic result row 1");
    await inspector.getByText("Published benchmark: candidate",{exact:true}).scrollIntoViewIfNeeded();
    await page.screenshot({path:"/private/tmp/omar-benchmark-candidate.png",fullPage:true});
    await expect(inspector).toContainText("Published benchmark: stale",{timeout:6000});
    await expect(inspector).not.toContainText("Published score: 42 percent");
    await expect(inspector).toContainText("No current reviewed numerical result is available");
  } finally {await fake.close();}
});
