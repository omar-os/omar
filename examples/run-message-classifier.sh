#!/usr/bin/env bash
set -euo pipefail
message=${1:-My package arrived broken, and I am unhappy with the service.}
exec bash "$(dirname "${BASH_SOURCE[0]}")/run-schema-example.sh" \
    examples/message-classifier.omar review.message "$message"
