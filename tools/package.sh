#!/usr/bin/env bash
#
# Build a loadstone release and pack it into a distributable archive.
#
#   tools/package.sh [target-triple]
#
# With no argument the host target is used. The archive is written to
# dist/loadstone-<target>.tar.gz (plus a .zip when `zip` is available) and
# contains the server binary, the README, the LICENSE and the Pterodactyl egg,
# so a download is ready to run with `./loadstone`.
#
# Environment:
#   SKIP_BUILD=1   package the binary already in target/<target>/release
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

target="${1:-}"
if [[ -z "$target" ]]; then
    target="$(rustc -vV | sed -n 's/^host: //p')"
    # Prefer a static musl build on Linux so the archive runs on any distro,
    # including the older Debian base used by the Pterodactyl yolks image.
    if [[ "$target" == *-linux-gnu ]]; then
        musl="${target%-gnu}-musl"
        if rustup target list --installed 2>/dev/null | grep -qx "$musl"; then
            target="$musl"
            echo "package: defaulting to static target $target"
        fi
    fi
fi

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
    echo "package: building loadstone for $target"
    cargo build --release --target "$target" --bin loadstone
fi

suffix=""
[[ "$target" == *windows* ]] && suffix=".exe"
binary="target/$target/release/loadstone$suffix"
if [[ ! -f "$binary" ]]; then
    echo "package: $binary not found (run without SKIP_BUILD, or build it first)" >&2
    exit 1
fi

stage="dist/loadstone-$target"
archive="dist/loadstone-$target.tar.gz"
rm -rf "$stage"
mkdir -p "$stage/pterodactyl"

cp "$binary" "$stage/loadstone$suffix"
cp README.md LICENSE "$stage/"
[[ -f pterodactyl/egg-loadstone.json ]] &&
    cp pterodactyl/egg-loadstone.json "$stage/pterodactyl/"

# Unix launcher so a downloaded directory also works when invoked from a copy
# that keeps the binary next to it.
if [[ "$target" != *windows* ]]; then
    cat >"$stage/start.sh" <<'EOF'
#!/usr/bin/env bash
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
exec ./loadstone "$@"
EOF
    chmod +x "$stage/start.sh"
fi

chmod +x "$stage/loadstone" 2>/dev/null || true

mkdir -p dist
rm -f "$archive" "dist/loadstone-$target.zip"
tar -czf "$archive" -C dist "loadstone-$target"
echo "package: wrote $archive"

if command -v zip >/dev/null 2>&1; then
    (cd dist && zip -qr "loadstone-$target.zip" "loadstone-$target")
    echo "package: wrote dist/loadstone-$target.zip"
fi
