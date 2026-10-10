#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
export PATH="$PWD/target/debug:$PWD/lang/.lake/build/bin:$PATH"
export OMARC_BIN="$PWD/lang/.lake/build/bin/omarc"
# Noninteractive WSL shells may not load an existing nvm installation.
if ! command -v codex >/dev/null 2>&1 && [[ -s "${NVM_DIR:-$HOME/.nvm}/nvm.sh" ]]; then
    set +u
    source "${NVM_DIR:-$HOME/.nvm}/nvm.sh"
    set -u
fi
real_codex=$(command -v codex)
wrapper_dir=$(mktemp -d /tmp/omar-classifier-cli.XXXXXX)
cleanup() {
    if tmux list-sessions >/dev/null 2>&1; then
        tmux set-environment -g PATH "${PATH#"$wrapper_dir:"}"
    fi
    rm -f "$wrapper_dir/codex"
    rmdir "$wrapper_dir"
}
trap cleanup EXIT
printf '#!/usr/bin/env bash\nexec %q -c check_for_update_on_startup=false "$@"\n' "$real_codex" > "$wrapper_dir/codex"
chmod +x "$wrapper_dir/codex"
export PATH="$wrapper_dir:$PATH"
if tmux list-sessions >/dev/null 2>&1; then
    tmux set-environment -g PATH "$PATH"
fi
program=${1:?usage: run-schema-example.sh PROGRAM INPUT_PORT INPUT_TEXT}
input_port=${2:?missing input port}
input_text=${3:?missing input text}
target/debug/omar run "$program" \
    --input "$input_port=$input_text" --timeout-seconds 120 --fast
