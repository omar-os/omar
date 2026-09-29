#!/bin/sh
set -eu

REPO="omar-os/omar"
INSTALL_DIR="${OMAR_INSTALL_DIR:-/usr/local/bin}"
TMP=
job_pid=
ui=0
unicode=0
reset= bold= muted= purple= lavender= violet= success=
esc=$(printf '\033')

# Match omar.rs: accent #8c63f2, bright #c3a6ff, dim #7049d8.
# NO_COLOR (including an empty value) also disables motion.
if [ -t 1 ] && [ "${TERM:-dumb}" != dumb ] && [ "${NO_COLOR+x}" != x ]; then
  reset="${esc}[0m"; bold="${esc}[1m"; muted="${esc}[2m"
  purple="${esc}[38;2;140;99;242m"
  lavender="${esc}[38;2;195;166;255m"
  violet="${esc}[38;2;112;73;216m"
  success="${esc}[38;2;173;219;197m"
  columns=${COLUMNS:-}
  if [ -z "$columns" ]; then
    columns=$(stty size 2>/dev/null </dev/tty | cut -d ' ' -f 2) || columns=80
  fi
  case "$columns" in ''|*[!0-9]*) columns=80 ;; esac
  if [ "$columns" -ge 60 ]; then ui=1; fi
  case "${LC_ALL:-${LC_CTYPE:-${LANG:-}}}" in
    *UTF-8*|*utf-8*|*UTF8*|*utf8*) unicode=1 ;;
  esac
fi

cleanup() {
  if [ -n "$job_pid" ]; then
    kill "$job_pid" 2>/dev/null || true
    wait "$job_pid" 2>/dev/null || true
  fi
  if [ "$ui" -eq 1 ]; then printf '\r%s[K%s%s[?25h' "$esc" "$reset" "$esc"; fi
  if [ -n "$TMP" ]; then rm -rf "$TMP"; fi
}
trap cleanup 0
trap 'exit 130' INT
trap 'exit 143' TERM

# Five rows, four letters. Build each frame before writing it to avoid flicker.
logo_frame() {
  frame=
  for row in 0 1 2 3 4; do
    case "$row" in
      0) o=' ███ '; m='█   █'; a=' ███ '; r='████ ' ;;
      1) o='█   █'; m='██ ██'; a='█   █'; r='█   █' ;;
      2) o='█   █'; m='█ █ █'; a='█████'; r='████ ' ;;
      3) o='█   █'; m='█   █'; a='█   █'; r='█  █ ' ;;
      4) o=' ███ '; m='█   █'; a='█   █'; r='█   █' ;;
    esac
    frame="${frame}${esc}[K  ${logo_o}${o}  ${logo_m}${m}  ${logo_a}${a}  ${logo_r}${r}${reset}\n"
  done
  printf '%b' "$frame"
}

intro() {
  printf '\n'
  if [ "$ui" -eq 1 ] && [ "$unicode" -eq 1 ]; then
    printf '%s[?25l' "$esc"
    # Reveal one letter at a time, then settle into the site's purple palette.
    for reveal in 0 1 2 3 4; do
      [ "$reveal" -eq 0 ] || printf '%s[5A' "$esc"
      logo_o=$violet; logo_m=$purple; logo_a=$lavender; logo_r=$purple
      # Undiscovered letters use the terminal's dim foreground.
      case "$reveal" in
        0) logo_o="${reset}${muted}"; logo_m=$logo_o; logo_a=$logo_o; logo_r=$logo_o ;;
        1) logo_m="${reset}${muted}"; logo_a=$logo_m; logo_r=$logo_m ;;
        2) logo_a="${reset}${muted}"; logo_r=$logo_a ;;
        3) logo_r="${reset}${muted}" ;;
      esac
      logo_frame
      sleep 0.12
    done
    printf '%s[?25h' "$esc"
  else
    printf '  %s%sOMAR%s\n' "$bold" "$purple" "$reset"
  fi
  printf '\n  %sDeterministic agent orchestration%s\n\n' "$muted" "$reset"
}

progress_frame() {
  step=$1; label=$2
  case $((step % 4)) in 0) spin='|' ;; 1) spin='/' ;; 2) spin='-' ;; 3) spin='\' ;; esac
  if [ "$unicode" -eq 1 ]; then
    full='━'; empty='─'
    case $((step % 4)) in 0) spin='⠋' ;; 1) spin='⠹' ;; 2) spin='⠴' ;; 3) spin='⠦' ;; esac
  else
    full='='; empty='-'
  fi
  bar=; cell=0; head=$((step % 27))
  while [ "$cell" -lt 22 ]; do
    age=$((head - cell))
    case "$age" in
      0|1) shade=$lavender; glyph=$full ;;
      2|3) shade=$purple; glyph=$full ;;
      4|5) shade=$violet; glyph=$full ;;
      *) shade=$muted; glyph=$empty ;;
    esac
    bar="${bar}${shade}${glyph}${reset}"
    cell=$((cell + 1))
  done
  printf '\r%s[K  %s%s%s %s  %s' "$esc" "$lavender" "$spin" "$reset" "$bar" "$label"
}

# A moving activity indicator, not a fabricated percentage. Keep tool output
# for failures; labels describe the real operation currently running.
run_stage() {
  label=$1; shift
  if [ "$ui" -eq 1 ]; then
    printf '%s[?25l' "$esc"
  else
    printf '  %s...\n' "$label"
  fi
  "$@" >"$TMP/stage.log" 2>&1 &
  job_pid=$!
  step=0
  while kill -0 "$job_pid" 2>/dev/null; do
    if [ "$ui" -eq 1 ]; then progress_frame "$step" "$label"; fi
    step=$((step + 1))
    sleep 0.08
  done
  status=0
  wait "$job_pid" || status=$?
  job_pid=
  if [ "$ui" -eq 1 ]; then printf '\r%s[K%s[?25h' "$esc" "$esc"; fi
  if [ "$status" -ne 0 ]; then
    printf '  Error: %s failed.\n' "$label" >&2
    cat "$TMP/stage.log" >&2
    return "$status"
  fi
  if [ "$ui" -eq 1 ]; then
    mark=ok; [ "$unicode" -eq 0 ] || mark='✓'
    printf '  %s%s%s %s\n' "$success" "$mark" "$reset" "$label"
  fi
}

intro

OS="$(uname -s)"
case "$OS" in
  Linux*) OS=linux ;;
  Darwin*) OS=darwin ;;
  *) printf 'Error: unsupported OS: %s\n' "$OS" >&2; exit 1 ;;
esac
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64) ARCH=amd64 ;;
  arm64|aarch64) ARCH=arm64 ;;
  *) printf 'Error: unsupported architecture: %s\n' "$ARCH" >&2; exit 1 ;;
esac

TMP="$(mktemp -d)"
if [ -z "${OMAR_VERSION:-}" ]; then
  run_stage 'Finding latest release' curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" -o "$TMP/release.json"
  OMAR_VERSION=$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\([^"]*\)".*/\1/p' "$TMP/release.json")
  if [ -z "$OMAR_VERSION" ]; then
    printf 'Error: could not determine latest version. Set OMAR_VERSION manually.\n' >&2
    exit 1
  fi
fi

TARBALL="omar-${OS}-${ARCH}.tar.gz"
URL="https://github.com/$REPO/releases/download/v${OMAR_VERSION}/${TARBALL}"
printf '  %s%somar v%s%s  %s%s/%s%s\n' "$bold" "$lavender" "$OMAR_VERSION" "$reset" "$muted" "$OS" "$ARCH" "$reset"
printf '  %sDestination%s  %s\n\n' "$muted" "$reset" "$INSTALL_DIR"

run_stage 'Downloading release' curl -fsSL "$URL" -o "$TMP/$TARBALL"
run_stage 'Unpacking binaries' tar xzf "$TMP/$TARBALL" -C "$TMP" --strip-components=1

# Authentication stays in the foreground with a visible cursor, so sudo can
# use /dev/tty when the installer itself is being piped into sh.
SUDO=""
if [ ! -w "$INSTALL_DIR" ]; then
  SUDO="sudo"
  printf '\n  Administrator access is needed for %s.\n' "$INSTALL_DIR"
  sudo -v
fi
printf '  Installing binaries...\n'
$SUDO install -d "$INSTALL_DIR"
for binary in omar omar-slack omar-computer; do
  $SUDO install "$TMP/$binary" "$INSTALL_DIR/"
done
# Preserve support for releases before the compiler was bundled.
if [ -f "$TMP/omarc" ]; then $SUDO install "$TMP/omarc" "$INSTALL_DIR/"; fi

printf '\n  %s%sInstalled successfully%s\n' "$bold" "$lavender" "$reset"
printf '  omar, omar-slack, omar-computer'
if [ -f "$TMP/omarc" ]; then printf ', omarc'; fi
printf '\n\n'
if ! command -v tmux >/dev/null 2>&1; then
  printf '  Install tmux before launching:\n'
  printf '    brew install tmux    # macOS\n'
  printf '    apt install tmux     # Debian/Ubuntu\n\n'
fi
printf '  %sLaunch Mission Control%s\n    omar serve --ui\n\n' "$bold" "$reset"
printf '  %sOr open the terminal UI%s\n    omar\n\n' "$muted" "$reset"
