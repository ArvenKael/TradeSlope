#!/bin/sh
# Stage local, host-platform extension packages; no npm or host-app build.
set -eu

usage() {
    printf '%s\n' 'Usage: sh scripts/build-extensions.sh [--release | --debug]'
}

profile=release
case $# in
    0) ;;
    1)
        case $1 in
            --release) ;;
            --debug) profile=debug ;;
            --help) usage; exit 0 ;;
            *) usage >&2; exit 1 ;;
        esac
        ;;
    *) usage >&2; exit 1 ;;
esac

root=$(CDPATH= cd -P "$(dirname "$0")/.." && pwd)
cd "$root"

# Use explicit public assets, not a recursive package copy that might include
# credentials, local configuration, tests, or unrelated developer files.
frontend_files='extension.json index.html card.css card.js README.md vendor.sha256
vendor/LICENSE vendor/NOTICE
vendor/openalgo-charts.standalone.js
vendor/openalgo-charts.draw.standalone.js
vendor/openalgo-charts.indicators.standalone.js
vendor/openalgo-charts.widget.standalone.js'
for file in $frontend_files; do
    if [ ! -f "extensions/openalgo/$file" ]; then
        printf 'Missing frontend asset: extensions/openalgo/%s\n' "$file" >&2
        exit 1
    fi
done

# Pin output to the workspace even when the caller sets CARGO_TARGET_DIR.
# Packages are for the current host, not a cross-compiled target.
if [ "$profile" = release ]; then
    cargo build -p tradeslope-ourbit --bin tradeslope-ourbit --release \
        --target-dir "$root/target"
else
    cargo build -p tradeslope-ourbit --bin tradeslope-ourbit \
        --target-dir "$root/target"
fi

executable=tradeslope-ourbit
if [ -f "target/$profile/$executable.exe" ]; then
    executable=$executable.exe
fi
provider="$root/dist/extensions/ourbit"
frontend="$root/dist/extensions/openalgo"
mkdir -p "$provider/bin" "$frontend/vendor"
cp "target/$profile/$executable" "$provider/bin/$executable"
"$provider/bin/$executable" --manifest > "$provider/extension.json"
for file in $frontend_files; do
    cp "extensions/openalgo/$file" "$frontend/$file"
done
printf 'Extension packages prepared in %s/dist/extensions\n' "$root"
