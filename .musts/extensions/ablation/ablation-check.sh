#!/usr/bin/env bash
# Accepts the log of `make ablation` only when its verdict is PASS for the Rust files as they
# are on disk now: a log about other code, a failing run or a hand-written summary is refused.
set -euo pipefail

mode="${1:-}"
request="$(cat)"

case "$mode" in
  resolve)
    jq -n --argjson req "$request" '{
      protocol_version: 1,
      tasks: [
        $req.checks[] as $check
        | {
            id: ("ablation-" + ($check.local_id | gsub("[^A-Za-z0-9_-]"; "-"))),
            extension: $req.capability,
            title: "Prove the new and edited tests fail without the change",
            satisfies: [$check.id],
            parallelizable: false,
            instructions: [
              "Run `make ablation > ablation.log 2>&1` from the workspace root (`make ablation BASE=<branch>` when the PR is not against main). Uncommitted changes count.",
              "It must end with `ABLATION PASS base=… ref=… tree=…`. On FAIL, fix the tests it marks with ✗: a test that passes without the change does not prove it. A test that passes on the base on purpose (a rename, a refactor) needs a `// ablation: passes-on-base: <reason>` comment right above it.",
              ("Record evidence with: musts evidence ablation-" + ($check.local_id | gsub("[^A-Za-z0-9_-]"; "-")) + " --text \"<summary>\" --asset ablation.log, then delete ablation.log."),
              "Paste the table from the log into your delivery instead of describing the proof by hand."
            ],
            evidence_contract: {
              text: { required: true, description: "One line: how many tests were proven." },
              assets: [{ kind: "log", required: true }]
            }
          }
      ],
      ignored_checks: [],
      notes: []
    }'
    ;;
  evidence)
    root="$(jq -r '.workspace_root' <<<"$request")"
    verdict=""
    log_path=""
    while IFS= read -r path; do
      [[ "$path" = /* ]] || path="$root/$path"
      [[ -f "$path" ]] || continue
      line="$(grep -E '^ABLATION (PASS|FAIL)' "$path" | tail -n 1 || true)"
      if [[ -n "$line" ]]; then
        verdict="$line"
        log_path="$path"
      fi
    done < <(jq -r '.submission.assets[]?.path' <<<"$request")

    reject() {
      jq -n --arg message "$1" '{
        protocol_version: 1, accepted: false, satisfies: [],
        missing: [{ kind: "log", message: $message }],
        message: $message
      }'
      exit 0
    }

    [[ -n "$(jq -r '.submission.text // ""' <<<"$request")" ]] || reject "Provide a one-line summary."
    [[ -n "$verdict" ]] || reject "Attach the output of make ablation; no ABLATION verdict line was found."
    [[ "$verdict" =~ ^ABLATION\ PASS\ base=([0-9a-f]{40})\ ref=([^ ]+)\ tree=([0-9a-f]+)$ ]] \
      || reject "The log says '$verdict'. Fix what make ablation marks and run it again."
    base="${BASH_REMATCH[1]}"
    ref="${BASH_REMATCH[2]}"
    tree="${BASH_REMATCH[3]}"

    # The base must be where this branch leaves a remote branch other than itself: a base
    # picked by hand (HEAD, a later commit, a ref that already holds the change) proves nothing.
    [[ "$ref" == origin/* ]] \
      || reject "The base '$ref' is not a remote branch. Run make ablation against origin/main or the pushed branch this PR targets (BASE=<branch>)."
    git -C "$root" rev-parse --verify --quiet "$ref^{commit}" >/dev/null \
      || reject "The base branch '$ref' does not exist here."
    branch="$(git -C "$root" rev-parse --abbrev-ref HEAD)"
    [[ "$ref" != "origin/$branch" ]] \
      || reject "The base '$ref' is this branch itself. Run make ablation against the branch this PR targets."
    expected="$(git -C "$root" merge-base "$ref" HEAD)"
    [[ "$base" == "$expected" ]] \
      || reject "The log's base $base is not where this branch leaves $ref ($expected). Run make ablation again."

    pathspec=(crates fixtures Cargo.toml Cargo.lock MODULE.bazel .bazelrc .bazelversion
      ':(glob)**/BUILD.bazel' ':(glob)**/BUILD' ':(glob)**/*.bzl')
    now="$(
      cd "$root"
      { git diff --name-only --no-renames "$base" -- "${pathspec[@]}"
        git ls-files --others --exclude-standard -- "${pathspec[@]}"; } \
        | LC_ALL=C sort -u \
        | while IFS= read -r path; do
            if [[ -f "$path" ]]; then
              printf '%s %s\n' "$path" "$(git hash-object -- "$path")"
            else
              printf '%s deleted\n' "$path"
            fi
          done \
        | git hash-object --stdin
    )"
    [[ "$now" == "$tree" ]] \
      || reject "The log is about other code: Rust, build or fixture files changed since make ablation ran. Run it again."

    jq -n --argjson req "$request" --arg path "$log_path" '{
      protocol_version: 1,
      accepted: true,
      satisfies: $req.task.satisfies,
      summary: "ablation passed against the base branch for the files as they are now",
      normalized_assets: [{ kind: "log", path: $path }]
    }'
    ;;
  *)
    echo "ablation-check.sh: expected resolve or evidence, got $mode" >&2
    exit 2
    ;;
esac
