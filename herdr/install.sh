#!/bin/sh
set -eu

NAME="herdr-claude-code-agent-teams"
REPO="kotarella1110/herdr-claude-code-agent-teams"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="$ROOT/bin"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/herdr-plugin.toml" | head -n 1)"

say() {
  echo "$NAME: $*"
}

build_from_source() {
  if ! command -v cargo >/dev/null 2>&1; then
    say "cannot build from source: cargo not found" >&2
    return 1
  fi
  say "building from source"
  cargo build --release --locked --manifest-path "$ROOT/Cargo.toml"
  mkdir -p "$BIN_DIR"
  install -m 0755 "$ROOT/target/release/$NAME" "$BIN_DIR/$NAME"
}

target_triple() {
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) echo "aarch64-apple-darwin" ;;
    Darwin-x86_64) echo "x86_64-apple-darwin" ;;
    Linux-aarch64 | Linux-arm64) echo "aarch64-unknown-linux-musl" ;;
    Linux-x86_64) echo "x86_64-unknown-linux-musl" ;;
    *) return 1 ;;
  esac
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

download() {
  for _ in 1 2 3 4 5; do
    curl --proto '=https' --tlsv1.2 -fsSL "$1" -o "$2" && return 0
    sleep 3
  done
  return 1
}

install_release() {
  target="$(target_triple)" || {
    say "no prebuilt binary for $(uname -s)-$(uname -m)" >&2
    return 1
  }
  base="https://github.com/$REPO/releases/download/v$VERSION"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT

  say "downloading v$VERSION for $target"
  download "$base/$NAME-$target.tar.gz" "$tmp/$NAME.tar.gz" || return 1
  download "$base/$NAME-$target.sha256" "$tmp/$NAME.sha256" || return 1

  expected="$(awk '{print $1}' "$tmp/$NAME.sha256")"
  actual="$(sha256 "$tmp/$NAME.tar.gz")"
  if [ "$expected" != "$actual" ]; then
    say "checksum mismatch (expected $expected, got $actual)" >&2
    exit 1
  fi

  if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    say "verifying build provenance"
    if ! gh attestation verify "$tmp/$NAME.tar.gz" --repo "$REPO" >/dev/null; then
      say "build provenance verification failed" >&2
      exit 1
    fi
  else
    say "skipping build provenance verification: sign in to the GitHub CLI (gh) to enable it"
  fi

  tar -xzf "$tmp/$NAME.tar.gz" -C "$tmp" || exit 1
  mkdir -p "$BIN_DIR" || exit 1
  install -m 0755 "$tmp/$NAME" "$BIN_DIR/$NAME" || exit 1
}

if [ "${1:-}" = "--source" ]; then
  build_from_source
elif ! install_release; then
  say "prebuilt binary unavailable, falling back to a source build"
  build_from_source
fi
say "installed $BIN_DIR/$NAME"
