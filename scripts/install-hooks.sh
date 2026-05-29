#!/usr/bin/env bash
# Configure git to use the repo's tracked hooks (scripts/hooks/) instead of the
# default `.git/hooks/`. Run once after cloning the repo:
#
#   bash scripts/install-hooks.sh
#
# To uninstall: `git config --unset core.hooksPath`.

set -e

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if [ ! -d scripts/hooks ]; then
    echo "ERROR: scripts/hooks/ not found. Run from a ccsight checkout." >&2
    exit 1
fi

git config core.hooksPath scripts/hooks
echo "Hooks installed: core.hooksPath = scripts/hooks"
echo "pre-commit: cargo fmt --check + test + clippy + lint (per commit)"
echo "pre-push:   same gate once before push (catches cherry-picked commits)"
