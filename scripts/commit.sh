#!/usr/bin/env bash
# Run the same gate CI runs, bump the version, and commit.
#
#   ./scripts/commit.sh "message"        stage everything, check, commit
#   ./scripts/commit.sh -p "message"     ... and push
#   ./scripts/commit.sh -n "message"     skip the checks (docs-only typo fix)
#
# The checks mirror .github/workflows/ci.yml, so a green run of this script is a
# good predictor of a green PR. It commits with --no-verify because it has
# already done everything .pre-commit-config.yaml would do, including the version
# bump; running the hooks again would just repeat the work.
set -euo pipefail

cd "$(dirname "$0")/.."

PUSH=0
SKIP_CHECKS=0

while [ $# -gt 0 ]; do
    case "$1" in
        -p|--push) PUSH=1; shift ;;
        -n|--no-checks) SKIP_CHECKS=1; shift ;;
        -h|--help)
            sed -n '2,6p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        --) shift; break ;;
        -*) echo "unknown option: $1" >&2; exit 2 ;;
        *) break ;;
    esac
done

MESSAGE="${1:-}"
if [ -z "$MESSAGE" ]; then
    echo "usage: $0 [-p] [-n] \"commit message\"" >&2
    exit 2
fi

step() { printf '\n=== %s\n' "$*"; }

step "staging everything"
git add -A
if git diff --cached --quiet; then
    echo "nothing staged, nothing to commit"
    exit 0
fi
git diff --cached --name-only | sed 's/^/  /'

if [ "$SKIP_CHECKS" = "0" ]; then
    step "cargo fmt"
    cargo fmt
    git add -A

    step "cargo clippy"
    cargo clippy --all-targets --all-features --locked -- -D warnings

    step "cargo test"
    cargo test --locked

    step "strictrs check"
    report="$(mktemp)"
    strictrs check . | tee "$report" | head -5
    python3 -c "
import json, sys
sys.exit(0 if json.load(open('$report'))['ok'] else 1)
"
    rm -f "$report"

    step "shellcheck"
    shellcheck --severity=warning scripts/*.sh

    step "yaml parses"
    python3 - <<'PY'
import pathlib, sys, yaml
targets = [
    *pathlib.Path(".github/workflows").glob("*.yml"),
    pathlib.Path(".pre-commit-config.yaml"),
]
failed = False
for path in targets:
    try:
        list(yaml.safe_load_all(path.read_text()))
    except yaml.YAMLError as exc:
        print(f"  {path}: {exc}")
        failed = True
sys.exit(1 if failed else 0)
PY
else
    step "checks skipped (-n)"
fi

step "version"
python3 scripts/bump_version.py

step "committing"
git commit --no-verify -m "$MESSAGE"
git --no-pager log --oneline -1

if [ "$PUSH" = "1" ]; then
    step "pushing"
    git push origin HEAD
fi
