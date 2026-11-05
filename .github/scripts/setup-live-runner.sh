#!/usr/bin/env bash
# Register this machine as the `omar-live` self-hosted runner that
# .github/workflows/topology-live.yml runs on, as a systemd service.
#
# Run as the user whose Claude Code and Codex logins the agents should use,
# with `gh` logged in as a repo admin and sudo available. Idempotent: an
# existing registration is left as it is.
set -euo pipefail

repo=${OMAR_LIVE_REPO:-omar-os/omar}
dir=${OMAR_LIVE_RUNNER_DIR:-$HOME/actions-runner}
version=2.338.0

claude auth status >/dev/null
codex login status

mkdir -p "$dir"
cd "$dir"
if [[ ! -x ./config.sh ]]; then
  curl -fsSL "https://github.com/actions/runner/releases/download/v$version/actions-runner-linux-x64-$version.tar.gz" | tar xz
fi

if [[ ! -f .runner ]]; then
  token=$(gh api -X POST "repos/$repo/actions/runners/registration-token" --jq .token)
  ./config.sh --unattended --url "https://github.com/$repo" --token "$token" \
    --name "$(hostname)-omar-live" --labels omar-live --replace
fi

# A public repo: no workflow from an outside contributor runs until a
# maintainer approves it.
gh api -X PUT "repos/$repo/actions/permissions/fork-pr-contributor-approval" \
  -f approval_policy=all_external_contributors

if ! systemctl is-active --quiet "actions.runner.*omar-live*"; then
  sudo ./svc.sh install "$USER"
  sudo ./svc.sh start
fi
