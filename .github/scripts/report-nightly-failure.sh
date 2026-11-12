#!/usr/bin/env bash
# Report a failed nightly run on the one tracking issue: a comment naming the
# failed jobs, linking the run and mentioning the issue's assignees; reopens
# the issue if closed. Never opens an issue.
# Needs GH_TOKEN, GH_REPO and the GITHUB_* run variables.
set -euo pipefail

title="Nightly Live Topology failures"
issue=$(gh issue list --state all --search "in:title \"$title\"" --json number,title \
  --jq "[.[] | select(.title == \"$title\")] | sort_by(.number) | first | .number // empty")
if [[ -z $issue ]]; then
  echo "::error::no issue titled '$title'; open it once and assign whoever should hear" >&2
  exit 1
fi

run="$GITHUB_SERVER_URL/$GH_REPO/actions/runs/$GITHUB_RUN_ID"
failed=$(gh api --paginate "repos/$GH_REPO/actions/runs/$GITHUB_RUN_ID/attempts/$GITHUB_RUN_ATTEMPT/jobs" \
  --jq '.jobs[] | select(.conclusion == "failure") | "- \(.name)"')
read -r state who < <(gh issue view "$issue" --json state,assignees \
  --jq '[.state] + [.assignees[].login | "@" + .] | join(" ")')
if [[ $state == CLOSED ]]; then
  gh issue reopen "$issue"
fi
gh issue comment "$issue" --body "Nightly $(date -u +%F) failed: $run
$failed
$who"
