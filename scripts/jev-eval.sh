#!/usr/bin/env bash
set -euo pipefail
if [[ $# -lt 4 || $# -gt 5 ]]; then
  echo "Usage: bash scripts/jev-eval.sh MAX_REQUESTS MAX_ESTIMATED_USD INPUT_USD_PER_MILLION REPORT_PATH [development|held-out]" >&2
  echo "Opt-in live calls; requires TYPESAFE_API_KEY. Supply a verified current input price. The cost bound is an estimate, not a provider-side spending cap." >&2
  exit 2
fi
export OMAR_JEV_LIVE_EVAL=1
export OMAR_JEV_MAX_REQUESTS="$1"
export OMAR_JEV_MAX_ESTIMATED_USD="$2"
export OMAR_JEV_INPUT_USD_PER_MILLION="$3"
export OMAR_JEV_REPORT="$4"
export OMAR_JEV_EVAL_SPLIT="${5:-held-out}"
cargo test --bin omar --features decision-support decisions::enabled::evaluation::live_evaluation -- --ignored --exact --nocapture
