#!/usr/bin/env bash
# ost-mcp installer for macOS.
#
# Builds and installs the `ost-mcp` binary with cargo, then drops
# `skills/ost-mcp/SKILL.md` into the Claude skills directory so a model can
# query the mailbox without registering an MCP server.
#
# The first build compiles the DuckDB amalgamation, which takes a few minutes
# and is the slowest part by far.
#
# Nothing here touches a mailbox. The reader maps an OST/PST read-only and
# opens a Mac Outlook profile's Outlook.sqlite with SQLite's read-only flag;
# neither backend has a code path that writes to a store.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/3rg0n/ost-mcp/main/install.sh | bash
#   curl -fsSL .../install.sh | bash -s -- --install-prereqs --force
#
# Options:
#   --ref <branch>          Branch to install from (default: main)
#   --skill-scope <scope>   "user" (~/.claude/skills, default) or "project"
#                           (.claude/skills in --project-path)
#   --project-path <dir>    Where a project-scoped skill goes (default: .)
#   --install-prereqs       Install a missing Rust toolchain with rustup, and
#                           prompt for Xcode Command Line Tools. Off by
#                           default: installing a compiler is not something a
#                           one-line command should do without being asked.
#   --force                 Reinstall even when the same version is present,
#                           and continue past the Command Line Tools check.
#   --skip-skill            Install the binary only.

set -euo pipefail

REF="main"
SKILL_SCOPE="user"
PROJECT_PATH="."
INSTALL_PREREQS=0
FORCE=0
SKIP_SKILL=0

while [ $# -gt 0 ]; do
  case "$1" in
    --ref) REF="$2"; shift 2 ;;
    --skill-scope) SKILL_SCOPE="$2"; shift 2 ;;
    --project-path) PROJECT_PATH="$2"; shift 2 ;;
    --install-prereqs) INSTALL_PREREQS=1; shift ;;
    --force) FORCE=1; shift ;;
    --skip-skill) SKIP_SKILL=1; shift ;;
    *) echo "unknown option: $1" >&2; exit 64 ;;
  esac
done

case "$SKILL_SCOPE" in
  user|project) ;;
  *) echo "--skill-scope must be 'user' or 'project', got: $SKILL_SCOPE" >&2; exit 64 ;;
esac

REPO_URL="https://github.com/3rg0n/ost-mcp"
RAW_BASE="https://raw.githubusercontent.com/3rg0n/ost-mcp/$REF"

# ------------------------------------------------------------------ output

step() { printf '\033[36m==> %s\033[0m\n' "$1"; }
ok()   { printf '\033[32m    ok  %s\033[0m\n' "$1"; }
note() { printf '\033[90m    --  %s\033[0m\n' "$1"; }
warn() { printf '\033[33m    !!  %s\033[0m\n' "$1"; }

fail() {
  local text="$1"; shift
  printf '\n\033[31most-mcp install failed: %s\033[0m\n' "$text" >&2
  if [ $# -gt 0 ]; then
    printf '\n\033[31mTo fix it:\033[0m\n' >&2
    for line in "$@"; do printf '  %s\n' "$line" >&2; done
  fi
  printf '\n' >&2
  exit 1
}

# ------------------------------------------------------------- preflight

step "Checking this machine"

if [ "$(uname -s)" != "Darwin" ]; then
  fail "this installer is for macOS." \
    "On Windows, use install.ps1 instead:" \
    "irm $RAW_BASE/install.ps1 | iex"
fi
ok "macOS $(sw_vers -productVersion 2>/dev/null || echo unknown)"

# Rust. Respect CARGO_HOME, because it is not always under the home directory.
CARGO_HOME_DIR="${CARGO_HOME:-$HOME/.cargo}"
CARGO_BIN="$CARGO_HOME_DIR/bin"

CARGO="$(command -v cargo || true)"
if [ -z "$CARGO" ] && [ -x "$CARGO_BIN/cargo" ]; then
  CARGO="$CARGO_BIN/cargo"
  export PATH="$CARGO_BIN:$PATH"
fi

if [ -z "$CARGO" ] && [ "$INSTALL_PREREQS" = "1" ]; then
  step "Installing the Rust toolchain with rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
  export PATH="$CARGO_BIN:$PATH"
  CARGO="$(command -v cargo || true)"
fi

if [ -z "$CARGO" ]; then
  fail "no Rust toolchain found (cargo is not on PATH)." \
    'curl --proto '"'"'=https'"'"' --tlsv1.2 -sSf https://sh.rustup.rs | sh' \
    "then re-run this installer in a new shell." \
    "" \
    "Or re-run with --install-prereqs to let this script do it."
fi
ok "cargo at $CARGO"

# Xcode Command Line Tools. Building DuckDB needs a C++ compiler and a linker,
# and neither exists on a fresh macOS install without them.
if xcode-select -p >/dev/null 2>&1; then
  ok "Xcode Command Line Tools at $(xcode-select -p)"
else
  if [ "$INSTALL_PREREQS" = "1" ]; then
    step "Requesting Xcode Command Line Tools (this opens a system dialog)"
    xcode-select --install 2>/dev/null || true
    note "finish the dialog, then re-run this installer"
    if [ "$FORCE" != "1" ]; then
      fail "Command Line Tools install was just requested; it is not done yet." \
        "Finish the dialog that just opened, then re-run this installer." \
        "" \
        "Or re-run with --force to attempt the build anyway."
    fi
  elif [ "$FORCE" != "1" ]; then
    fail "no Xcode Command Line Tools found, and DuckDB will not compile without them." \
      "xcode-select --install" \
      "" \
      "Or re-run with --install-prereqs to let this script request it," \
      "or with --force to try the build anyway."
  else
    warn "no Command Line Tools detected; continuing because --force was passed"
  fi
fi

# ---------------------------------------------------------------- binary

# cargo's own git client cannot always read the macOS Keychain credential
# helper, so a private or enterprise fork can fail to authenticate. The git
# CLI can, and is already installed (it ships with the Command Line Tools).
if [ -z "${CARGO_NET_GIT_FETCH_WITH_CLI:-}" ] && command -v git >/dev/null 2>&1; then
  export CARGO_NET_GIT_FETCH_WITH_CLI=true
fi

step "Installing the ost-mcp binary (the first build compiles DuckDB; allow a few minutes)"

INSTALL_ARGS=(install --git "$REPO_URL" --branch "$REF" --locked ost-mcp)
if [ "$FORCE" = "1" ]; then
  INSTALL_ARGS+=(--force)
fi

# Stream cargo's output. An exit code on its own tells the user nothing, and
# the compiler error is the whole diagnosis.
if ! "$CARGO" "${INSTALL_ARGS[@]}"; then
  fail "cargo install failed (the error is above)." \
    "A git authentication failure means the repository is not readable by" \
    "this machine. Authenticate to GitHub first, for example with:" \
    "  gh auth login"
fi

EXE="$CARGO_BIN/ost-mcp"
if [ ! -x "$EXE" ]; then
  fail "cargo reported success but $EXE is missing." \
    "Check where cargo installs binaries: cargo install --list"
fi
ok "installed $EXE"

case ":$PATH:" in
  *":$CARGO_BIN:"*) ;;
  *)
    warn "$CARGO_BIN is not on your PATH. Add it, or the skill cannot find the binary:"
    note "echo 'export PATH=\"$CARGO_BIN:\$PATH\"' >> ~/.zshrc && source ~/.zshrc"
    ;;
esac

# ----------------------------------------------------------------- skill

if [ "$SKIP_SKILL" != "1" ]; then
  step "Installing the Claude Code skill"

  if [ "$SKILL_SCOPE" = "user" ]; then
    SKILL_DIR="$HOME/.claude/skills/ost-mcp"
  else
    SKILL_DIR="$(cd "$PROJECT_PATH" && pwd)/.claude/skills/ost-mcp"
  fi

  mkdir -p "$SKILL_DIR"
  SKILL_FILE="$SKILL_DIR/SKILL.md"
  SKILL_URL="$RAW_BASE/skills/ost-mcp/SKILL.md"

  if ! curl -fsSL "$SKILL_URL" -o "$SKILL_FILE"; then
    fail "could not download the skill from $SKILL_URL" \
      "The binary is installed and works; only the skill is missing." \
      "Copy it from a clone instead:" \
      "  git clone $REPO_URL" \
      "  cp -r ost-mcp/skills/ost-mcp \"$(dirname "$SKILL_DIR")\""
  fi

  # A proxy or a login page answers with 200 and the wrong body, and a skill
  # without frontmatter loads as nothing at all.
  FIRST_LINE="$(head -n 1 "$SKILL_FILE")"
  if [ "$FIRST_LINE" != "---" ]; then
    fail "the file downloaded from $SKILL_URL is not a skill (it has no frontmatter)." \
      "Something between you and GitHub answered instead of GitHub." \
      "Inspect it: $SKILL_FILE"
  fi

  ok "skill at $SKILL_FILE"
fi

# ---------------------------------------------------------------- verify

step "Checking that it works"

STORES="$("$EXE" --list 2>/dev/null || true)"
STORE_COUNT="$(printf '%s\n' "$STORES" | grep -c '[^[:space:]]' || true)"

if [ "$STORE_COUNT" -gt 0 ] 2>/dev/null; then
  ok "$STORE_COUNT store(s) found"
  printf '%s\n' "$STORES" | while IFS= read -r line; do
    [ -n "$line" ] && note "$line"
  done
else
  warn "no store found automatically. The binary works, but there is nothing to read."
  note "Point it at one explicitly:"
  note "  ost-mcp \"/path/to/Outlook 15 Profiles/<identity>/Data\" --info"
fi

MOUNTED=0
INFO_JSON="$("$EXE" --info 2>/dev/null || true)"
if [ -n "$INFO_JSON" ]; then
  KIND="$(printf '%s' "$INFO_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("kind",""))' 2>/dev/null || true)"
  FOLDERS="$(printf '%s' "$INFO_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("folders",""))' 2>/dev/null || true)"
  if [ -n "$KIND" ]; then
    ok "mounted backend $KIND, $FOLDERS folders"
    MOUNTED=1
  else
    note "could not parse --info output; not fatal"
  fi
else
  note "could not open a store automatically; not fatal"
fi

if [ "$MOUNTED" = "1" ]; then
  SQL_JSON="$("$EXE" --sql "SELECT count(*) AS n FROM messages" 2>/dev/null || true)"
  N="$(printf '%s' "$SQL_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["n"])' 2>/dev/null || true)"
  if [ -n "$N" ]; then
    ok "queried $N messages"
  else
    warn "the store opened but a query could not be parsed"
  fi
fi

# ------------------------------------------------------------------ done

printf '\n\033[32most-mcp is installed.\033[0m\n\n'
printf '  ost-mcp --info                     what is mounted\n'
printf '  ost-mcp --sql "SELECT ..."          query it\n'
printf '  ost-mcp --message <nid>            read one message\n\n'
if [ "$SKIP_SKILL" != "1" ]; then
  printf 'Restart your Claude Code session to pick up the skill, then just ask about\n'
  printf 'your mail: "what came in from finance this week".\n\n'
fi
