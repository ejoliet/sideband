#!/usr/bin/env sh
#
# sideband installer.
#
#   curl -fsSL https://raw.githubusercontent.com/ejoliet/sideband/main/scripts/install.sh | sh
#
# Downloads the release archive for this machine, checks it against the
# published SHA256SUMS, and installs one binary. Homebrew is the recommended
# route on macOS; this exists for everyone else and for CI.
#
# Environment:
#   SIDEBAND_VERSION   tag to install (default: latest release)
#   SIDEBAND_BIN_DIR   install directory (default: ~/.local/bin, or
#                      /usr/local/bin when it is writable)

set -eu

repo=ejoliet/sideband
version=${SIDEBAND_VERSION:-latest}

die() {
	echo "install.sh: $*" >&2
	exit 1
}

need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required"; }
need curl
need tar

case "$(uname -s)" in
Darwin) os=apple-darwin ;;
Linux) os=unknown-linux-gnu ;;
*) die "unsupported OS $(uname -s); build from source with \`cargo install --path crates/sideband\`" ;;
esac

case "$(uname -m)" in
arm64 | aarch64) arch=aarch64 ;;
x86_64 | amd64) arch=x86_64 ;;
*) die "unsupported architecture $(uname -m)" ;;
esac

target="$arch-$os"
# No aarch64 Linux artifact is published yet; say so instead of 404ing.
if [ "$target" = "aarch64-unknown-linux-gnu" ]; then
	die "no prebuilt aarch64 Linux binary yet; build from source with \`cargo install --path crates/sideband\`"
fi

if [ "$version" = latest ]; then
	base="https://github.com/$repo/releases/latest/download"
else
	base="https://github.com/$repo/releases/download/$version"
fi

archive="sideband-$target.tar.gz"
tmp=$(mktemp -d)
# Leaving a half-downloaded archive behind would make a retry look corrupt.
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "downloading $archive"
curl -fsSL "$base/$archive" -o "$tmp/$archive" ||
	die "could not download $base/$archive"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" ||
	die "could not download $base/SHA256SUMS"

if command -v sha256sum >/dev/null 2>&1; then
	sum=$(sha256sum "$tmp/$archive" | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then
	sum=$(shasum -a 256 "$tmp/$archive" | cut -d' ' -f1)
else
	die "no sha256sum or shasum; refusing to install an unverified binary"
fi
grep -q "^$sum  *$archive\$" "$tmp/SHA256SUMS" ||
	die "checksum mismatch for $archive; refusing to install"
echo "sha256 ok"

# Signature check when the user has minisign and we published a .minisig.
if command -v minisign >/dev/null 2>&1 &&
	curl -fsSL "$base/SHA256SUMS.minisig" -o "$tmp/SHA256SUMS.minisig" 2>/dev/null; then
	if [ -n "${SIDEBAND_PUBKEY:-}" ]; then
		minisign -Vm "$tmp/SHA256SUMS" -P "$SIDEBAND_PUBKEY" ||
			die "signature verification failed"
		echo "signature ok"
	else
		echo "note: SHA256SUMS.minisig found; set SIDEBAND_PUBKEY to verify it"
	fi
fi

tar -xzf "$tmp/$archive" -C "$tmp"
[ -f "$tmp/sideband" ] || die "archive did not contain a sideband binary"

if [ -n "${SIDEBAND_BIN_DIR:-}" ]; then
	bin_dir=$SIDEBAND_BIN_DIR
elif [ -w /usr/local/bin ]; then
	bin_dir=/usr/local/bin
else
	bin_dir="$HOME/.local/bin"
fi
mkdir -p "$bin_dir"
install -m 755 "$tmp/sideband" "$bin_dir/sideband"

echo "installed $bin_dir/sideband"
case ":$PATH:" in
*":$bin_dir:"*) ;;
*) echo "add it to your PATH:  export PATH=\"$bin_dir:\$PATH\"" ;;
esac
echo
echo "next:  sideband doctor"
