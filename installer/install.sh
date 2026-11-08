#!/bin/sh
# Installs Remote Volume for the current user on Linux or macOS, with no admin rights:
#   curl -fsSL https://github.com/justereseau/companion-remote-volume/releases/latest/download/install.sh | sh
#
# Downloads the program of the latest release, checks it against the release's checksums, and lets
# it install itself (`remote-volume --install`): a per-user folder and the start at login.
# To remove it again: run the installed program with --uninstall.
set -eu

releases="https://github.com/justereseau/companion-remote-volume/releases/latest/download"

fail() {
	echo "Remote Volume: $1" >&2
	exit 1
}

case "$(uname -s)/$(uname -m)" in
Linux/x86_64) file="remote-volume-linux-x64" ;;
Darwin/arm64) file="remote-volume-macos-arm64" ;;
*) fail "no release for $(uname -s) on $(uname -m)" ;;
esac

if command -v curl >/dev/null 2>&1; then
	download() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
	download() { wget -q -O "$2" "$1"; }
else
	fail "curl or wget is needed"
fi

if command -v sha256sum >/dev/null 2>&1; then
	checksum() { sha256sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null 2>&1; then
	checksum() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
else
	fail "sha256sum or shasum is needed"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT INT TERM

echo "Downloading $file..."
download "$releases/$file" "$work/remote-volume" || fail "could not download $file"
download "$releases/SHA256SUMS.txt" "$work/SHA256SUMS.txt" || fail "could not download the checksums"

expected="$(awk -v name="$file" '$2 == name || $2 == "*" name { print $1 }' "$work/SHA256SUMS.txt")"
[ -n "$expected" ] || fail "the release has no checksum for $file"
[ "$(checksum "$work/remote-volume")" = "$expected" ] || fail "the download of $file is corrupt"

chmod +x "$work/remote-volume"
if ! "$work/remote-volume" --version >/dev/null 2>&1; then
	[ "$(uname -s)" = Linux ] && echo "On Linux it needs PulseAudio's client library: sudo apt install libpulse0" >&2
	fail "the program does not start on this computer"
fi
"$work/remote-volume" --install
