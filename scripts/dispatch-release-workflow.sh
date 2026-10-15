#!/bin/bash
# Starts the release workflow for a version (after checking it against
# Cargo.toml and plugin.toml), e.g.: scripts/dispatch-release-workflow.sh 1.0.0
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION="${1:?usage: $0 <version> [eov ref]}"
EOV_REF="${2:-}"
python3 scripts/release.py validate-source --root . --version "$VERSION"
REPO=eosin-platform/eov-annotations-plugin
echo "Dispatching the release workflow of $REPO for version $VERSION"
gh workflow run release.yml \
  --repo "$REPO" \
  --ref master \
  -f version="$VERSION" \
  -f eov_ref="$EOV_REF"
