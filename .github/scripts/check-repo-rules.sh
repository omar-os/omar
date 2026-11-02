#!/usr/bin/env bash
# CONTRIBUTING.md rules 1 and 2, for files added or changed since BASE: no
# documentation beyond the allowed files, and no binary files.
# Usage: check-repo-rules.sh BASE
set -euo pipefail

base=$1
fail=0

while IFS= read -r path; do
  case $path in
    README.md | AGENT.md | CLAUDE.md | CONTRIBUTING.md | lang/spec.md | prompts/*) ;;
    docs/* | */docs/* | *.md | *.mdx | *.rst | *.adoc)
      echo "::error file=$path::documentation belongs on omar.rs, not in the repo (CONTRIBUTING.md rule 1)"
      fail=1
      ;;
  esac
done < <(git diff --name-only --no-renames --diff-filter=A "$base"...HEAD)

# numstat prints "-" for a binary file's line counts.
while IFS=$'\t' read -r added _ path; do
  if [[ $added == - ]]; then
    echo "::error file=$path::binary file; host it elsewhere (CONTRIBUTING.md rule 2)"
    fail=1
  fi
done < <(git diff --numstat --no-renames --diff-filter=AM "$base"...HEAD)

exit "$fail"
