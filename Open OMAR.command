#!/bin/sh
set -eu

# OMAR owns saved work; the selected assistant never chooses the data directory.
omar_data_root="${OMAR_HOME:-$HOME/.omar}"
PATH="$omar_data_root/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export PATH

omar_executable="${OMAR_BIN:-}"
if [ -z "$omar_executable" ]; then
    omar_executable="$(command -v omar || true)"
fi
if [ -z "$omar_executable" ]; then
    omar_source_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
    for omar_candidate in "$omar_source_dir/target/release/omar" "$omar_source_dir/target/debug/omar"; do
        if [ -x "$omar_candidate" ]; then
            omar_executable="$omar_candidate"
            break
        fi
    done
fi
if [ -z "$omar_executable" ] || [ ! -x "$omar_executable" ]; then
    echo 'OMAR is not installed. Install omar and omarc, or set OMAR_BIN to your built OMAR executable.' >&2
    exit 1
fi
exec "$omar_executable" open "$@"
