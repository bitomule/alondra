#!/bin/sh
# Usage: doc_review.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
# Reviews a document in the real TUI against a fresh sandbox; see tools/e2e-doc-review.py.
set -eu
exec python3 tools/e2e-doc-review.py "$@"
