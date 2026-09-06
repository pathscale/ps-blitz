#!/usr/bin/env sh
#
# Drive the fixture pages under `packages/blitz-script/tests/fixtures` with
# ps-qa, against *this* checkout of the engine.
#
# The host is built here rather than installed. `qa-inspect-host` depends on the
# published engine, so a `cargo install`ed one judges whatever was last released
# and a change to this workspace cannot fail it -- which is the whole point of
# running it from here. Cargo's `--config` injects the patch on the command
# line, so nothing in the host's repository is edited and there is no manifest
# to remember to revert.
#
# ps-qa itself needs no engine: it speaks the control protocol over a socket, so
# the published binary drives whatever host it is pointed at.
set -eu

readonly ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
readonly FIXTURES="$ROOT/packages/blitz-script/tests/fixtures"
readonly CHECKS="$ROOT/tests/ps-qa"
readonly PACKAGES="$ROOT/packages"

export PATH="$HOME/.cargo/bin:$PATH"

# `QA_PS_QA` wins, for someone iterating on the harness and the fixtures
# together; otherwise the published binary. It carries no engine of its own, so
# the installed one drives whatever host it is pointed at.
qa="${QA_PS_QA:-$(command -v ps-qa || true)}"
if [ -z "$qa" ] || [ ! -x "$qa" ]; then
  echo "ps-qa is not on PATH; cargo install ps-qa" >&2
  echo "  (or set QA_PS_QA to a local build)" >&2
  exit 1
fi

# A checkout of the host's repository. `QA_HOST` skips the build entirely, for
# someone iterating on the host and the fixtures together.
host="${QA_HOST:-}"
if [ -z "$host" ]; then
  observability="${PS_OBSERVABILITY:-$ROOT/../ps-observability}"
  if [ ! -d "$observability" ]; then
    echo "no ps-observability checkout at $observability" >&2
    echo "  set PS_OBSERVABILITY, or QA_HOST to a prebuilt qa-inspect-host" >&2
    exit 1
  fi
  echo "building qa-inspect-host against $PACKAGES"
  ( cd "$observability" && cargo build --release -p qa-inspect-host \
      --config "patch.crates-io.ps-blitz-dom.path='$PACKAGES/blitz-dom'" \
      --config "patch.crates-io.ps-blitz-script.path='$PACKAGES/blitz-script'" \
      --config "patch.crates-io.ps-blitz-traits.path='$PACKAGES/blitz-traits'" \
      --config "patch.crates-io.ps-blitz-paint.path='$PACKAGES/blitz-paint'" \
      --config "patch.crates-io.ps-blitz-shell.path='$PACKAGES/blitz-shell'" )
  host="$observability/target/release/qa-inspect-host"
fi

if [ ! -x "$host" ]; then
  echo "no qa-inspect-host at $host" >&2
  exit 1
fi

# One page per group, named alike, so adding a fixture is adding two files and
# nothing else. A page with no checks is a mistake rather than a skip.
status=0
for page in "$FIXTURES"/*.html; do
  group="$(basename "$page" .html)"
  if [ ! -f "$CHECKS/$group.ron" ]; then
    echo "FAIL $group: no checks at $CHECKS/$group.ron" >&2
    status=1
    continue
  fi
  if "$qa" --app "$ROOT/ps-qa.ron" qa-hosted \
      --host "$host" --page "$page" --checks "$CHECKS" "$group"; then
    echo "PASS $group"
  else
    echo "FAIL $group"
    status=1
  fi
done

exit "$status"
