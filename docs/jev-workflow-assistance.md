# Optional Jev workflow assistance

Three profiles extend the existing `review-owner-v1` pilot:

| Profile | Evidence | Operator action |
|---|---|---|
| `template-fit-v1` | Confirmed brief, eligible starter catalog | Choose or reject a template; selection opens a proposal |
| `scenario-coverage-v1` | Exact proposal and selected `review-actual-output@1` scenario | Inspect handoffs and instructions before deployment |
| `artifact-requirement-v1` | Immutable saved text revision and confirmed requirement | Inspect the selected excerpt and decide what to change |

Build with `decision-support`; use the configuration in [decision support](decision-support.md).
The build feature and runtime configuration default off. Every new subject also
starts off, regardless of the legacy run default. Opening the panel, selecting a
template, editing evidence, or polling records never dispatches a model call.

In Mission Control, open **Optional workflow assistance**. Confirm the evidence,
**Prepare review locally**, inspect **Exact evidence and criterion**, then
explicitly enable suggestions for that revision and request an evaluation.
Shadow stores locally without provider calls. Ordinary authoring and deployment
remain available when advice fails. Choosing a template compiles it and opens a
proposal; the ordinary **Deploy → Confirm deploy** action is still required.

## Boundaries

- Jev answers bounded questions. Fixed UI text explains outcomes; no generated
  rationale, source code, recipient identity or control-plane command is accepted.
- Compiler rejection, missing named agents/ports/handoffs and absent required
  inputs block a positive scenario result in code. The v1 scenario targets the
  starter catalog's `flow.writer`, `flow.reviewer`, `brief/draft/review/result`
  names. An arbitrary topology needs an explicitly versioned scenario adapter;
  it does not silently inherit these requirements.
- Templates require a successful installed Codex or Claude version probe and
  tmux availability. This is local executable eligibility, not proof of provider
  credentials or successful execution. All three are finite, two-agent,
  three-reaction workflows. Supplied research uses supplied text only.
- Artifact review uses existing workspace Git snapshots, not a new editor.
  Markdown, UTF-8 plain text, `.log`/`.test.txt` reports and static HTML text are
  supported. Files are limited to 64 KiB; selection to 16 KiB. HTML is parsed,
  never executed. Hidden/script/style text is excluded; styles, images, dynamic
  content or parse errors make completeness uninspectable. Offsets refer to
  Unicode scalar positions in extracted text. This does not assess visual design.
- Recognized failed Rust, TAP and pytest-style summaries in `.log`/`.test.txt`
  remain deterministic concerns. Unknown reports are text, not a test execution
  result. No tests are run by the artifact review endpoint.
- A completeness requirement cannot pass on partial/uninspectable evidence.
  Any model evidence reference must choose a supplied excerpt ID.
- Choice confidence, option probability and Noul evidence sufficiency are
  displayed separately. The initial 0.90 thresholds are conservative policy
  defaults, not measured accuracy or calibrated correctness probabilities.

## Identity, state and limits

Schema-v2 records contain a real draft, proposal, run-invocation or artifact
subject, server-derived chat/workspace association, content revision and an
evaluation digest binding the exact evidence, criterion, catalog and profile.
The client never supplies an authoritative owner or compiler result. Pre-run
subjects have no run ID. Template/criterion/evidence changes stale earlier cards
and reset consent on the next registration; unsaved browser edits immediately
label the displayed card stale and disable evaluation. The server cannot observe
an unsaved editor revision until it is submitted. Selecting an older immutable
artifact snapshot is allowed and displays that exact revision.

Records use the existing private atomic store, two workers, queue depth 32,
three-second provider timeout and 100 MiB store ceiling. Limits are shared:
10 enrolled runs/subjects; 100 requests per subject across profiles; 100 stored
subjects; 1,000 admitted evaluations per UTC day across legacy and new profiles.
Daily admission is persisted before queuing, including deterministic abstentions
and failed attempts. There are no automatic paid retries. Store/retention limits
refuse new work instead of silently deleting evidence. No hosted account/tenant
system is provided by this local chat isolation.

Turning off waits for bounded in-flight calls and cancels queued calls. Revision
changes use the same gate. Restart restores records as historical, marks
interrupted work unavailable and leaves modes off. Fresh explicit requests can
reevaluate historical evidence. Request IDs cannot be rebound to different data;
identical current selections are deduplicated within their owner/subject.

Schema-v1 run records remain readable. A persisted owner scope is now required
for historical API access. Old records without owner metadata may be bound only
while the owning chat has that live run; guessing an old ID cannot claim it.
No automatic cross-chat migration is attempted.

## API

Use the existing `/chats/{chat-id}` prefix. Identity comes from that server context.

- `GET /v1/assist/templates`: eligible versioned starter catalog and source.
- `GET /v1/assist/artifact-snapshots`: this chat EA's saved workspaces/revisions.
- `POST /v1/assist/artifact-preview`: bounded saved text, no provider call.
- `POST /v1/assist/subjects`: confirmed payload → subject/evidence/mode/records.
- `GET /v1/assist/subjects/{id}`: local state only.
- `POST /v1/assist/subjects/{id}/mode`: `{sha256, mode}`.
- `POST /v1/assist/subjects/{id}/evaluations`: `{sha256, request_id}`.
- `POST /v1/assist/subjects/{id}/decisions/{decision_id}/feedback`:
  `{request_id, verdict, note?}`; verdict useful/not_useful/dismissed/wrong_owner.

Registration uses `{id: UUID, profile_id, confirmed: true, criterion?, payload}`.
Payload is one of:

```json
{"kind":"draft","text":"Write a launch brief for freelance designers."}
```

```json
{"kind":"proposal","program":"...","inputs":{"flow.brief":"..."},"scenario_id":"review-actual-output","scenario_version":"1"}
```

```json
{"kind":"artifact","workspace_id":"UUID","snapshot_id":"UUID","path":"result.md","start":0,"end":120}
```

Artifact criteria are operator-confirmed `{id, version, text, requires_complete}`.
For configurable owner routing, use `review-owner-v1` with a `run` payload:
`{run_id, source_id, source_sha256, start, end, responsibility_map:{version,roles}}`.
Roles map stable IDs to responsibilities (1–12); multiple/uncertain remain reserved.
This API leaves the legacy four-role UI and stored records unchanged. It accepts
only this chat's live run and a current captured source. It cannot message a role.

## Evaluation and release

[Corpus](../eval/jev-v1/corpus.json): 30 cases per new profile, 20 development and
10 held-out per profile. Includes multilingual, ambiguous, contradictory,
unsupported and adversarial evidence. Labels are synthetic and provisional;
the split is an engineering evaluation split, not an independent benchmark.

The fixture tests exercise production schemas, policy and lifecycle. They do not
measure Jev quality. [Evaluation status](../eval/jev-v1/evaluation-status.json)
records the unavailable live run. No TypeSafe credential was available in this
implementation environment; profiles remain disabled by default.

To opt in later, supply a verified current input price and explicit limits:

```sh
bash scripts/jev-eval.sh MAX_REQUESTS MAX_ESTIMATED_USD INPUT_USD_PER_MILLION /tmp/jev-report.json held-out
```

`TYPESAFE_API_KEY` stays in the caller's environment. The command uses the same
provider request, schema validator and policy as production, without retries.
The report includes profile/model versions, case IDs, label status, surfaced
precision, coverage/abstention, false-positive covered/satisfied rate, per-case
latency/tokens, estimated cost and an always-abstain baseline. Invoice cost is
unavailable. The estimate uses UTF-8 request bytes as a conservative token proxy;
unknown provider overhead means it is not a provider-side spending cap. Use a
provider account cap for a strict dollar ceiling. Zero denominators are null.

The checked-in skipped report uses test limits and a placeholder price solely to
exercise the zero-call path; it is not a price quote. Before product enablement,
have humans review labels and measure precision, coverage and false reassurance
on representative workflows. Synthetic fixture success cannot authorize release.

## Demo

1. Enable the optional build/config locally. Keep default mode off. No public
   hosting or permissions are changed by this feature.
2. Enter a launch brief, browse the catalog, prepare it and explicitly request
   advice. Inspect/reject it; choose a template manually if unavailable.
3. Select **Use** to open its proposal. In the source editor, change
   `reviewer(draft)` to `reviewer(brief)` and the matching `$(draft)` reference to
   `$(brief)`. Select **Check scenarios before running** and confirm the named
   scenario. Its local structural concern appears without a provider call or run.
4. Restore the original source, prepare a new revision and explicitly check it.
   With credentials, Jev may surface coverage or abstain; never promise a label.
   Old cards remain stale. Deploy only through the ordinary user confirmation.
5. Use a saved workspace text result missing the requested audience guidance.
   Load its snapshot and path, preview text, confirm that requirement, and request
   review. Inspect the selected excerpt and exact revision.
6. Edit the file through the existing workspace tools, save a new snapshot, select
   it and request a fresh review. Advice itself never edits or publishes.

Without credentials, demonstrate steps 2–6 with the committed browser fixtures
and real compiler tests. Label that demonstration as software behavior, not live
Jev output. The existing workspace snapshot CLI supplies saved revisions until
the independent artifact/editor work is integrated.
