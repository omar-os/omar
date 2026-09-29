import catalog from "./template-catalog.json" with { type: "json" };

export type Template = (typeof catalog)[number];
export const templates: Template[] = catalog;

type Instructions = { work: string; review: string; finish: string };

// Each entry is an executable, bounded OMAR workflow. The instructions describe
// the evidence the agents must collect; they do not turn agent claims into
// verified test results or validated financial calculations.
const instructions: Record<string, Instructions> = {
  test: {
    work: "Reproduce the named failing test in the local workspace, inspect the cause, make the smallest patch, and run the same test again. Record the exact command and exit statuses. If baseline passes, say it could not be reproduced.",
    review: "Inspect the actual diff and test output. Challenge any unobserved claim of a fix, modified test, or unrelated change.",
    finish: "Report the patch files, exact before and after commands and exits, unresolved failures, and where the user can inspect changes.",
  },
  understand: {
    work: "Map the local repository's real entry points, configuration, major modules and relevant tests. Cite paths for every substantive claim and distinguish source from possibly stale documentation.",
    review: "Check cited paths against the workspace, identify unsupported behavior claims and missing areas.",
    finish: "Write a concise repository guide with clickable relative paths, setup commands found but not run, and open questions.",
  },
  review: {
    work: "Capture the requested local diff and revision, inspect nearby code, and identify only actionable defects with concrete triggers and changed-line locations.",
    review: "Recheck each candidate against the actual diff. Remove speculative findings and state when checks were not run.",
    finish: "Return severity, file and line, trigger, consequence, evidence, and remedy for each surviving finding; an empty list means no findings in this review.",
  },
  upgrade: {
    work: "Identify the exact package and target version. Check the manifest and lockfile, inspect available migration evidence, update only this dependency and necessary code, then run available checks. Do not guess when exact version or registry access is missing.",
    review: "Compare requested and installed versions and inspect lockfile churn, compatibility and observed check output.",
    finish: "Summarize version change, changed files, exact checks and exits, migration evidence, and remaining issues.",
  },
  docs: {
    work: "Inspect the current code and requested docs, update only the selected documentation files, verify local links and only claim snippets were tested if executed.",
    review: "Compare new documentation claims with actual symbols, files and examples; check for unsupported setup assertions.",
    finish: "Return the documentation paths, key changes, source references, checks actually run and unresolved gaps.",
  },
  issue: {
    work: "Read the bounded issue and acceptance criteria, inspect relevant code, implement a small patch and run the listed tests or explain why unavailable.",
    review: "Inspect the actual patch against each acceptance criterion and check test evidence rather than trusting a summary.",
    finish: "Provide criterion-by-criterion status, changed files, exact check results and remaining work.",
  },
  security: {
    work: "Use only the supplied local finding and fixture. Reproduce the specific weakness if possible, patch it, and rerun exploit and valid-behavior regression checks. Do not probe unrelated hosts.",
    review: "Inspect reproduction and regression evidence; challenge claims of broad security or fixes when reproduction did not succeed.",
    finish: "Report finding, precise remediation, before and after observed checks, diff paths and residual risk. Mark unreproduced cases unverified.",
  },
  release: {
    work: "Read the selected local revision range, commits and diff; draft changelog, migration notes and a readiness checklist without publishing anything.",
    review: "Map every release claim to change evidence; flag breaking changes and readiness items not established by code.",
    finish: "Provide draft release notes, migration steps, source references and open readiness checks.",
  },
  research: {
    work: "Read only supplied local source documents, preserve their names and relevant sections, and prepare a comparison grounded in citations. Explicitly retain disagreement and unanswered questions.",
    review: "Check that cited passages exist and support each claim; remove unsupported synthesis.",
    finish: "Return a cited brief, comparison, conflicting evidence and unresolved questions. Do not claim web research.",
  },
  launch: {
    work: "Extract approved facts from the supplied product brief; draft landing copy, email and social variants without inventing metrics, testimonials or customers.",
    review: "Trace factual claims to supplied material and flag any missing claim support or audience mismatch.",
    finish: "Return editable copy drafts by channel and a claim checklist with missing evidence.",
  },
  "dependency-security": {
    work: "Inspect exact dependency versions and the supplied scanner report or run an available local scanner. Use real advisory identifiers only from observed output. Deduplicate aliases and leave reachability unknown unless tested.",
    review: "Validate version/advisory mapping against source evidence, scanner timestamp and deployment context. Remove invented CVEs.",
    finish: "Return a prioritized fix queue with package/version, advisory evidence, confidence, unknowns and proposed next action.",
  },
  permissions: {
    work: "Inspect only the selected local authorization fixture and policy, enumerate allowed and forbidden identity/resource cases, run available tests, and identify cross-tenant failures. Do not contact live third-party systems.",
    review: "Check each allow/deny expectation against policy and observed test output. Flag untested identities and unauthorized-access claims without evidence.",
    finish: "Return the permission matrix, exact test commands and exits, failing cases, proposed fix and untested cases.",
  },
  infrastructure: {
    work: "Review the supplied local infrastructure files against the approved baseline. Identify resource locations and proposed changes; do not apply cloud configuration.",
    review: "Check that each finding refers to an actual resource and that any proposed patch parses when tooling is available.",
    finish: "Report resource-linked findings, baseline rule, proposed patch and exceptions; state whether live posture was inspected.",
  },
  questionnaire: {
    work: "Read the supplied customer questions and approved policy documents, preserve every question ID and order, and draft answers only from cited evidence. Mark unknowns explicitly.",
    review: "Verify citation support and catch unsupported yes answers, invented certifications and missing questions.",
    finish: "Return a question-by-question draft with supported/unknown state, sources and owner follow-ups. Do not submit it.",
  },
  pipeline: {
    work: "Analyze supplied opportunity/activity records using the stated aging and missing-field rules. Keep record IDs and do not invent win probabilities.",
    review: "Check dates, duplicate IDs and each flagged record against the supplied export; mark uncertain joins.",
    finish: "Return an explained follow-up and cleanup queue with record IDs and rule evidence. Do not update CRM or contact anyone.",
  },
  proposal: {
    work: "Map discovery needs to approved catalog services and prices. Calculate line items explicitly; leave out-of-catalog work unpriced and assumptions visible.",
    review: "Recalculate amounts and check every priced service against supplied catalog evidence. Flag missing currency, tax and terms.",
    finish: "Return an editable proposal draft, price table, assumptions and unpriced requests. Do not send or accept commitments.",
  },
  rfp: {
    work: "Parse supplied RFP requirements in original ID order, draft each response from approved product evidence and flag unmet mandatory requirements.",
    review: "Check coverage, citations and response limits; reject invented capabilities, references or certifications.",
    finish: "Return a requirement/response matrix, mandatory gaps and owner questions. Do not submit a bid.",
  },
  renewals: {
    work: "Join supplied renewal, usage and support exports by stable account ID and comparable periods. Describe trends without predicting churn.",
    review: "Check joins, date windows, trend arithmetic and missing-data states against source rows.",
    finish: "Return account briefs, evidence-backed discussion agendas and missing data. Do not message customers.",
  },
  "cloud-cost": {
    work: "Reconcile supplied billing totals, inspect supplied utilization and commitments, and identify conditional cost opportunities without counting overlaps twice. A bill alone does not prove idleness.",
    review: "Recompute arithmetic and challenge missing utilization, commitment assumptions and claimed realized savings.",
    finish: "Return spend breakdown and qualified opportunities with assumptions, overlap exclusions and unknown savings. Do not change cloud resources.",
  },
  invoices: {
    work: "Use supplied invoice and payment exports to match exact references within currency. Handle partials, duplicates and ambiguous payments separately; keep original row IDs.",
    review: "Recalculate invoice and payment control totals and challenge unsupported matches or double counts.",
    finish: "Return matched ledger and exception queue with amounts, currency, row IDs and reconciliation math. Do not post entries or move money.",
  },
  support: {
    work: "Apply the supplied routing policy to ticket IDs and draft replies only from approved knowledge-base material. Mark unknown diagnoses for clarification.",
    review: "Check policy matches, citation support and whether any draft assumes an unverified fix.",
    finish: "Return routing queue, reply drafts, knowledge-base gaps and escalation cases. Do not edit tickets or send messages.",
  },
  feedback: {
    work: "Deduplicate feedback by original source, group themes, count unique sources and preserve serious minority issues under supplied criteria.",
    review: "Recheck unique-source counts, theme citations and priority weights; do not infer representative demand from a sample.",
    finish: "Return evidence-linked opportunities, counts, weighting and dissenting cases. Do not change the roadmap.",
  },
  "weekly-review": {
    work: "Use supplied period exports and explicit metric definitions to calculate the weekly KPIs. Exclude duplicates, state timezone and withhold metrics whose source is missing.",
    review: "Recalculate every reported number from source rows and flag undefined denominators or unsupported causal explanations.",
    finish: "Return a KPI report with formulas, inputs, incomplete metrics and clearly labeled hypotheses.",
  },
};

function omarString(value: string): string {
  // Instructions are single-line static text. Keep quotes and backslashes valid
  // for the OMAR string lexer; user text is supplied as a port value instead.
  return JSON.stringify(value);
}

export function templateTeam(template: Template): string {
  // Team names are OMAR identifiers, while chat titles remain human-readable.
  if (template.id === "docs") return "Documentation";
  return template.title
    .split(/[^A-Za-z0-9]+/)
    .filter(Boolean)
    .map((word) => word[0].toUpperCase() + word.slice(1))
    .join("");
}

export function templateProgram(template: Template, backend = "codex"): string {
  const steps = instructions[template.id];
  if (!steps) throw new Error(`Template ${template.id} is not an agent workflow.`);
  if (!/^(agy|claude|codex|cursor|opencode|pi)$/i.test(backend)) {
    throw new Error("Choose a supported agent backend.");
  }
  const team = templateTeam(template);
  const first = `Work only on the local workspace and request. Scope: ${template.scope} ${steps.work} Request: $(request) Set draft to your work and evidence references.`;
  const second = `${steps.review} Inspect the actual local work and draft: $(draft) Set review to a concise critique that includes the original draft or its artifact paths.`;
  const third = `${steps.finish} Resolve the critique using actual workspace evidence: $(review) Set result to the final user-facing report. Distinguish observed facts from suggestions.`;
  return `team ${team}[worker: ${backend}, reviewer: ${backend}] {\n  input request: string\n  action draft: string\n  action review: string\n  output result: string\n  prompt worker(request) -> draft ${omarString(first)}\n  prompt reviewer(draft) -> review ${omarString(second)}\n  prompt worker(review) -> result ${omarString(third)}\n}\nmain { flow = ${team}() }\n`;
}

export function templateInputs(request: string): Record<string, string> {
  return { "flow.request": request };
}

export type SecretFinding = { line: number; kind: string; redacted: string };

/** Browser-local screening: only redacted findings should leave this function. */
export function scanSecrets(source: string): SecretFinding[] {
  const patterns: { kind: string; pattern: RegExp }[] = [
    { kind: "GitHub token", pattern: /\bgh[pousr]_[A-Za-z0-9_]{20,}\b/g },
    { kind: "AWS access key ID", pattern: /\bAKIA[0-9A-Z]{16}\b/g },
    { kind: "Private key header", pattern: /-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/g },
  ];
  const found: SecretFinding[] = [];
  for (const [index, line] of source.split(/\r?\n/).entries()) {
    for (const { kind, pattern } of patterns) {
      pattern.lastIndex = 0;
      for (const match of line.matchAll(pattern)) {
        found.push({ line: index + 1, kind, redacted: `${match[0].slice(0, 4)}…[redacted]` });
      }
    }
  }
  return found;
}
