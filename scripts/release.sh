#!/usr/bin/env bash
# Usage: scripts/release.sh VERSION [-y]
# Releases VERSION (e.g. 0.1.1): bumps the workspace version, commits, tags vVERSION and pushes
# the branch and the tag. The tag push triggers .github/workflows/release.yml, which builds the
# Linux and Windows binaries and attaches them to the GitHub release.
# -y skips the confirmation prompt.
set -euo pipefail

die() { echo "error: $*" >&2; exit 1; }

VERSION="${1:-}"
ASSUME_YES="${2:-}"
[ -n "$VERSION" ] || die "usage: $0 VERSION [-y]   (e.g. $0 0.1.1)"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "VERSION must look like 1.2.3, got '$VERSION'"

cd "$(dirname "$0")/.."
TAG="v$VERSION"
BRANCH="$(git branch --show-current)"

[ -n "$BRANCH" ] || die "detached HEAD: check out a branch first"
[ -z "$(git status --porcelain)" ] || die "the working tree is not clean: commit or stash first"
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && die "tag $TAG already exists"

CURRENT="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p}' Cargo.toml | head -1)"
[ -n "$CURRENT" ] || die "cannot read the current version from Cargo.toml"
[ "$CURRENT" != "$VERSION" ] || die "Cargo.toml is already at $VERSION"

echo "Release $CURRENT -> $VERSION on branch '$BRANCH' (tag $TAG, push to origin)"
if [ "$ASSUME_YES" != "-y" ]; then
  read -r -p "Continue? [y/N] " ans
  [ "$ans" = "y" ] || die "aborted"
fi

# Workspace version, plus the version requirement of every path dependency between our crates.
sed -i "/^\[workspace.package\]/,/^\[/s/^version = \".*\"/version = \"$VERSION\"/" Cargo.toml
sed -i "s/\(dbzilla-[a-z]* = { version = \)\"[^\"]*\"/\1\"$VERSION\"/" crates/*/Cargo.toml

# Refreshes Cargo.lock for the workspace crates and checks that everything still resolves.
cargo update --workspace --quiet
cargo check --workspace --quiet

printf -- '- Release %s.\n' "$VERSION" >> CHANGES.md

git add Cargo.toml Cargo.lock CHANGES.md crates/*/Cargo.toml
git commit -m "chore: release $TAG"
git tag -a "$TAG" -m "Release $TAG"
git push origin "$BRANCH"
git push origin "$TAG"
echo "Done: CI will build and publish the binaries for $TAG."
