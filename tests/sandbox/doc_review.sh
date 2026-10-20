#!/bin/sh
# Usage: doc_review.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox> <node> <persistent.cjs>
# Reviews a document in the real TUI against a fresh sandbox; see tools/e2e-doc-review.py.
set -eu
export SDK_NODE="$5" SDK_HOST="$6"
exec python3 tools/e2e-doc-review.py "$1" "$2" "$3" "$4"
