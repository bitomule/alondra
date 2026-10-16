#!/bin/sh
# Runs the eval suites of the node and orchestrator skills with `claude plugin eval`: each case
# runs with and without the plugin, so the report shows what the skill changes. Results go to a
# temporary folder; the MCP servers are mocked, nothing reaches the daemon.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
out=${SKILL_EVALS_DIR:-$(mktemp -d)}
status=0
for plugin in plugin plugin-orchestrator; do
    claude plugin eval "$root/$plugin" --trust-plugin --no-publish --ablation with-without \
        --output-dir "$out/$plugin" --report "$out/$plugin/report.html" "$@" || status=1
done
echo "Reports in $out"
exit $status
