#!/usr/bin/env bash
# Stands in for bazel in the ablation e2e tests. $FAKE_DIR holds the scenario:
#   tests            labels `tests(...)` answers
#   code             labels `kind(...)` answers
#   srcs/<label>     files `labels(srcs, <label>)` answers (label with / and : as _)
#   block            when present, `build` marks `building` and hangs
#   test-hook        script run for `test`, writing testlogs/<pkg>/<name>/test.log
set -euo pipefail

cmd="$1"
shift
case "$cmd" in
  info)
    echo "$FAKE_DIR/testlogs"
    ;;
  query)
    expr="$1"
    case "$expr" in
      tests\(*) cat "$FAKE_DIR/tests" ;;
      kind\(*) cat "$FAKE_DIR/code" 2>/dev/null || true ;;
      labels\(srcs,*)
        label="${expr#labels(srcs, }"
        label="${label%%)*}"
        cat "$FAKE_DIR/srcs/$(printf '%s' "$label" | tr '/:' '__')" 2>/dev/null || true
        ;;
    esac
    ;;
  build)
    if [[ -f "$FAKE_DIR/block" ]]; then
      touch "$FAKE_DIR/building"
      sleep 20
    fi
    ;;
  test)
    bash "$FAKE_DIR/test-hook"
    ;;
esac
