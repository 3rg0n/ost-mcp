#!/usr/bin/env bash
# ost-mcp installer for macOS.
#
# Downloads the prebuilt `ost-mcp` binary from a GitHub release, checks it
# against its published SHA-256, then drops `skills/ost-mcp/SKILL.md` into the
# Claude skills directory so a model can query the mailbox without registering
# an MCP server.
#
# The prebuilt binary is arm64 only. On an Intel Mac, when no release asset is
# there, or with --from-source, the script builds with cargo instead — which
# needs a Rust toolchain and Xcode Command Line Tools, and compiles the DuckDB
# amalgamation, which takes a few minutes.
#
# Nothing here touches a mailbox. The reader maps an OST/PST read-only and
# opens a Mac Outlook profile's Outlook.sqlite with SQLite's read-only flag;
# neither backend has a code path that writes to a store.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/3rg0n/ost-mcp/main/install.sh | bash
#   curl -fsSL .../install.sh | bash -s -- --from-source --install-prereqs
#
# Options:
#   --release-tag <tag>     Release to download from (default: latest)
#   --from-source           Build with cargo instead of downloading a binary
#   --ref <branch>          Branch to build from (default: main). Any value
#                           other than main implies --from-source. Also selects
#                           which branch the skill comes from.
#   --skill-scope <scope>   "user" (~/.claude/skills, default) or "project"
#                           (.claude/skills in --project-path)
#   --project-path <dir>    Where a project-scoped skill goes (default: .)
#   --install-prereqs       Install a missing Rust toolchain with rustup, and
#                           prompt for Xcode Command Line Tools. Only the
#                           source build needs either. Off by default:
#                           installing a compiler is not something a one-line
#                           command should do without being asked.
#   --force                 Reinstall even when the same version is present,
#                           and continue past the Command Line Tools check.
#   --skip-skill            Install the binary only.

set -euo pipefail

RELEASE_TAG="latest"
FROM_SOURCE=0
REF="main"
SKILL_SCOPE="user"
PROJECT_PATH="."
INSTALL_PREREQS=0
FORCE=0
SKIP_SKILL=0

while [ $# -gt 0 ]; do
  case "$1" in
    --release-tag) RELEASE_TAG="$2"; shift 2 ;;
    --from-source) FROM_SOURCE=1; shift ;;
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
ok "macOS $(sw_vers -productVersion 2>/dev/null || echo unknown) $(uname -m)"

# Where the binary goes. cargo's bin directory when it exists, so a machine
# that already has a source install gets an upgrade in place rather than a
# second copy on PATH; ~/.local/bin otherwise.
CARGO_HOME_DIR="${CARGO_HOME:-$HOME/.cargo}"
CARGO_BIN="$CARGO_HOME_DIR/bin"

INSTALL_DIR="$CARGO_BIN"
if [ ! -d "$CARGO_BIN" ]; then
  INSTALL_DIR="$HOME/.local/bin"
fi
EXE="$INSTALL_DIR/ost-mcp"

# Only an arm64 asset is published. Cross-compiling DuckDB's C++ to x86_64 is
# untested, and a broken asset is worse than none, so an Intel Mac builds.
TARGET="aarch64-apple-darwin"

USE_SOURCE="$FROM_SOURCE"
if [ "$REF" != "main" ] && [ "$FROM_SOURCE" != "1" ]; then
  note "--ref $REF given, so the binary is built from that branch rather than downloaded"
  USE_SOURCE=1
fi
if [ "$(uname -m)" != "arm64" ] && [ "$USE_SOURCE" != "1" ]; then
  note "this is an Intel Mac and only an arm64 binary is published; building from source"
  USE_SOURCE=1
fi

# ------------------------------------------------------- binary: download

# Downloads, checks and unpacks the release asset for this platform. Returns 0
# on success and 1 when the release or the asset is not there — the caller then
# builds from source.
#
# A failed download is a fallback. A checksum mismatch is not: it means the
# bytes that arrived are not the bytes that were published, and running them
# anyway would defeat the point of publishing the hash.
install_from_release() {
  local asset="ost-mcp-$TARGET.tar.gz"
  local base
  if [ "$RELEASE_TAG" = "latest" ]; then
    base="$REPO_URL/releases/latest/download"
  else
    base="$REPO_URL/releases/download/$RELEASE_TAG"
  fi

  local tmp
  tmp="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" RETURN

  if ! curl -fsSL "$base/$asset" -o "$tmp/$asset" \
    || ! curl -fsSL "$base/$asset.sha256" -o "$tmp/$asset.sha256"; then
    warn "no release binary for this platform"
    return 1
  fi

  local expected actual
  expected="$(tr -d '[:space:]' < "$tmp/$asset.sha256")"
  actual="$(shasum -a 256 "$tmp/$asset" | cut -d ' ' -f 1)"
  if [ "$expected" != "$actual" ]; then
    fail "the downloaded binary does not match its published SHA-256." \
      "expected $expected" \
      "got      $actual" \
      "" \
      "Do not run it. Something between you and GitHub changed the bytes." \
      "Build from source instead: re-run with --from-source"
  fi
  ok "SHA-256 matches the published hash"

  tar -xzf "$tmp/$asset" -C "$tmp"
  if [ ! -f "$tmp/ost-mcp" ]; then
    fail "$asset does not contain ost-mcp." \
      "The release asset is malformed. Build from source: re-run with --from-source"
  fi

  mkdir -p "$INSTALL_DIR"
  if ! install -m 755 "$tmp/ost-mcp" "$EXE"; then
    fail "could not write $EXE" \
      "A running ost-mcp holds the file open. Quit any Claude Code session with" \
      "the MCP server attached, then re-run this installer."
  fi
  # curl does not set com.apple.quarantine, but a proxy or a wrapper might.
  xattr -d com.apple.quarantine "$EXE" 2>/dev/null || true

  # A checksum proves the bytes are the published ones, not that they run here.
  # Anything that stops a downloaded binary from starting — an OS too old, a
  # policy that blocks it — makes a source build the better answer, so treat it
  # as a failed download.
  if ! "$EXE" --version >/dev/null 2>&1; then
    warn "the downloaded binary does not run on this machine"
    return 1
  fi
  return 0
}

if [ "$USE_SOURCE" != "1" ]; then
  step "Downloading the ost-mcp binary ($RELEASE_TAG release, $TARGET)"
  if install_from_release; then
    ok "installed $EXE"
  else
    note "falling back to a source build"
    USE_SOURCE=1
  fi
fi

# --------------------------------------------------------- binary: source

if [ "$USE_SOURCE" = "1" ]; then
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
    fail "no Rust toolchain found (cargo is not on PATH), and this run needs a source build." \
      'curl --proto '"'"'=https'"'"' --tlsv1.2 -sSf https://sh.rustup.rs | sh' \
      "then re-run this installer in a new shell." \
      "" \
      "Or re-run with --install-prereqs to let this script do it."
  fi
  ok "cargo at $CARGO"

  # Xcode Command Line Tools. Building DuckDB needs a C++ compiler and a
  # linker, and neither exists on a fresh macOS install without them.
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

  # cargo's own git client cannot always read the macOS Keychain credential
  # helper, so a private or enterprise fork can fail to authenticate. The git
  # CLI can, and is already installed (it ships with the Command Line Tools).
  if [ -z "${CARGO_NET_GIT_FETCH_WITH_CLI:-}" ] && command -v git >/dev/null 2>&1; then
    export CARGO_NET_GIT_FETCH_WITH_CLI=true
  fi

  step "Building the ost-mcp binary (the first build compiles DuckDB; allow a few minutes)"

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

  # cargo installs where cargo wants to, whatever this script picked earlier.
  EXE="$CARGO_BIN/ost-mcp"
  INSTALL_DIR="$CARGO_BIN"
  if [ ! -x "$EXE" ]; then
    fail "cargo reported success but $EXE is missing." \
      "Check where cargo installs binaries: cargo install --list"
  fi
  ok "installed $EXE"
fi

# An `[ -n ... ] && ok ...` one-liner would exit the script under `set -e` when
# the version cannot be read, which is not worth failing over.
VERSION="$("$EXE" --version 2>/dev/null || true)"
if [ -n "$VERSION" ]; then
  ok "$VERSION"
fi

# ------------------------------------------------------------------ PATH

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    warn "$INSTALL_DIR is not on your PATH. Add it, or the skill cannot find the binary:"
    note "echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc && exec zsh"
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
