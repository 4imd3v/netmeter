#!/bin/sh
# NetMeter installer — downloads a prebuilt static binary from GitHub Releases,
# verifies its SHA-256, installs it, and sets up the systemd service.
#
#   curl -fsSL https://raw.githubusercontent.com/4imd3v/netmeter/main/install.sh | sh
#
# Options:
#   --version <v>   release version to install (default: latest)
#   --prefix <dir>  install prefix (default: /usr/bin, or ~/.local/bin with --user)
#   --user          per-user install; no system daemon (TOTAL-only)
#   --no-daemon     install the binary only; skip service setup
#   -h, --help      show this help

set -eu

REPO="4imd3v/netmeter"
PREFIX=""
VERSION="latest"
MODE="system"
NO_DAEMON=0

usage() {
	cat <<'EOF'
NetMeter installer

Usage: install.sh [options]

  --version <v>   release version to install (default: latest)
  --prefix <dir>  install prefix (default: /usr/bin, or ~/.local/bin with --user)
  --user          per-user install; no system daemon (TOTAL-only)
  --no-daemon     install the binary only; skip service setup
  -h, --help      show this help

Examples:
  curl -fsSL https://raw.githubusercontent.com/4imd3v/netmeter/main/install.sh | sh
  sh install.sh --version 0.2.0
EOF
}

while [ $# -gt 0 ]; do
	case "$1" in
	--version)
		VERSION="${2:?--version needs a value}"
		shift 2
		;;
	--version=*)
		VERSION="${1#*=}"
		shift
		;;
	--prefix)
		PREFIX="${2:?--prefix needs a value}"
		shift 2
		;;
	--prefix=*)
		PREFIX="${1#*=}"
		shift
		;;
	--user)
		MODE="user"
		shift
		;;
	--no-daemon)
		NO_DAEMON=1
		shift
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		echo "error: unknown option: $1" >&2
		usage >&2
		exit 1
		;;
	esac
done

if [ "$(uname -s)" != "Linux" ]; then
	echo "error: NetMeter is Linux-only (found $(uname -s))" >&2
	exit 1
fi

case "$(uname -m)" in
x86_64 | amd64) TARGET="x86_64-unknown-linux-musl" ;;
aarch64 | arm64) TARGET="aarch64-unknown-linux-musl" ;;
*)
	echo "error: unsupported architecture: $(uname -m)" >&2
	exit 1
	;;
esac

case "$VERSION" in
latest) BASE="https://github.com/$REPO/releases/latest/download" ;;
v*) BASE="https://github.com/$REPO/releases/download/$VERSION" ;;
*) BASE="https://github.com/$REPO/releases/download/v$VERSION" ;;
esac
# Mirrors / air-gapped hosts: point at any directory holding the assets.
BASE="${NETMETER_INSTALL_BASE:-$BASE}"

if command -v curl >/dev/null 2>&1; then
	fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
	fetch() { wget -qO "$2" "$1"; }
else
	echo "error: need curl or wget" >&2
	exit 1
fi

run_root() {
	if [ "$MODE" = "system" ] && [ "$(id -u)" -ne 0 ]; then
		if ! command -v sudo >/dev/null 2>&1; then
			echo "error: need root (or sudo) to install the system service" >&2
			exit 1
		fi
		sudo "$@"
	else
		"$@"
	fi
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

ASSET="netmeter-$TARGET.tar.gz"
echo "Downloading $ASSET ($VERSION)…"
fetch "$BASE/$ASSET" "$TMP/$ASSET"
fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS"

echo "Verifying checksum…"
(cd "$TMP" && sha256sum -c --ignore-missing SHA256SUMS)

tar -xzf "$TMP/$ASSET" -C "$TMP"
if [ ! -x "$TMP/netmeter" ]; then
	echo "error: archive does not contain a netmeter binary" >&2
	exit 1
fi

if [ -z "$PREFIX" ]; then
	if [ "$MODE" = "user" ]; then
		PREFIX="$HOME/.local/bin"
	else
		PREFIX="/usr/bin"
	fi
fi

run_root install -Dm755 "$TMP/netmeter" "$PREFIX/netmeter"
echo "installed $PREFIX/netmeter"

if [ "$MODE" = "user" ]; then
	MANDIR="${XDG_DATA_HOME:-$HOME/.local/share}/man/man1"
else
	MANDIR="/usr/share/man/man1"
fi
if "$TMP/netmeter" manpage >"$TMP/netmeter.1" 2>/dev/null && [ -s "$TMP/netmeter.1" ]; then
	run_root install -Dm644 "$TMP/netmeter.1" "$MANDIR/netmeter.1"
	echo "installed $MANDIR/netmeter.1"
fi

if [ "$NO_DAEMON" -eq 1 ]; then
	echo "Skipped service setup (--no-daemon)."
elif [ "$MODE" = "user" ]; then
	cat <<EOF
Per-user install: no system service. Record TOTAL-only with:
  $PREFIX/netmeter daemon run
LAN/WAN split and per-app capture need root; re-run without --user, or
  sudo $PREFIX/netmeter daemon install
EOF
else
	run_root "$PREFIX/netmeter" daemon install
fi

echo
echo "Done. Try: netmeter status && netmeter show --period day"
if [ "$MODE" = "user" ]; then
	case ":$PATH:" in
	*":$PREFIX:"*) ;;
	*) echo "Note: add $PREFIX to your PATH." ;;
	esac
fi
