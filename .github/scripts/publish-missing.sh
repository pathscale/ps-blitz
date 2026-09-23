#!/usr/bin/env bash
# Publish every workspace crate whose current version is not on crates.io yet.
#
# `cargo publish --workspace` aborts on the first crate already published and
# uploads nothing, so a release that leaves one crate's version unchanged never
# ships. This asks the index about each publishable member, then hands cargo
# the missing ones in a single `cargo publish -p ... -p ...`, which orders them
# by dependency itself.
#
#   PUBLISH_DRY_RUN=1 .github/scripts/publish-missing.sh   prints, uploads nothing
set -euo pipefail

# The sparse index path for a crate name, as crates.io lays it out.
index_path() {
    local name
    name=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')
    case ${#name} in
        1) printf '1/%s' "$name" ;;
        2) printf '2/%s' "$name" ;;
        3) printf '3/%s/%s' "${name:0:1}" "$name" ;;
        *) printf '%s/%s/%s' "${name:0:2}" "${name:2:2}" "$name" ;;
    esac
}

# `name vX.Y.Z (/path)` per workspace member, from cargo rather than a list.
members=$(cargo tree --workspace --depth 0 --prefix none -e normal \
    | grep -E '^[A-Za-z0-9_-]+ v[0-9][^ ]* \(/' | sort -u)

missing=()
while read -r name version path; do
    manifest="$(printf '%s' "$path" | tr -d '()')/Cargo.toml"
    if grep -qE '^publish *= *false' "$manifest"; then
        continue
    fi
    version=${version#v}
    # A crate never published answers 404, which is "missing" too.
    if curl -fsS "https://index.crates.io/$(index_path "$name")" 2>/dev/null \
        | grep -qF "\"vers\":\"$version\""; then
        echo "$name $version is already on crates.io"
    else
        echo "$name $version will be published"
        missing+=(-p "$name")
    fi
done <<EOF
$members
EOF

if [ ${#missing[@]} -eq 0 ]; then
    echo "Every publishable crate is already on crates.io at its current version."
    exit 0
fi

if [ -n "${PUBLISH_DRY_RUN:-}" ]; then
    echo "cargo publish ${missing[*]}"
    exit 0
fi
cargo publish "${missing[@]}"
