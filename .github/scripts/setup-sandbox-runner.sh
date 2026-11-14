#!/usr/bin/env bash
# Register this machine as the `omar-sandbox` self-hosted runner that
# .github/workflows/sandbox-live.yml runs on, as a systemd service.
#
# Needs: KVM, `sbx login` done as this user, `gh` logged in as a repo admin,
# sudo, and sbx's global policy deny-all (it applies to every local sandbox,
# so this does not change it). Idempotent: an existing registration is left.
set -euo pipefail

repo=${OMAR_SANDBOX_REPO:-omar-os/omar}
dir=${OMAR_SANDBOX_RUNNER_DIR:-$HOME/actions-runner-sandbox}
version=2.338.0

sbx diagnose --json | jq -e '[.checks[] | select(.status == "fail")] | length == 0' >/dev/null
sbx policy ls | grep -q '^default-deny-all ' ||
  { echo "global sbx policy is not deny-all; see sbx policy reset / init deny-all" >&2; exit 1; }

mkdir -p "$dir"
cd "$dir"
if [[ ! -x ./config.sh ]]; then
  curl -fsSL "https://github.com/actions/runner/releases/download/v$version/actions-runner-linux-x64-$version.tar.gz" | tar xz
fi

if [[ ! -f .runner ]]; then
  token=$(gh api -X POST "repos/$repo/actions/runners/registration-token" --jq .token)
  ./config.sh --unattended --url "https://github.com/$repo" --token "$token" \
    --name "$(hostname)-omar-sandbox" --labels omar-sandbox --no-default-labels --replace
fi

# A public repo: no workflow from an outside contributor runs until a
# maintainer approves it.
gh api -X PUT "repos/$repo/actions/permissions/fork-pr-contributor-approval" \
  -f approval_policy=all_external_contributors

if ! systemctl is-active --quiet "actions.runner.*omar-sandbox*"; then
  sudo ./svc.sh install "$USER"
  sudo ./svc.sh start
fi
