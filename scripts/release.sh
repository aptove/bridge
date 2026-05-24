#!/usr/bin/env bash
set -e

VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')

echo "Releasing v$VERSION..."

git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to $VERSION"
git tag "v$VERSION"
git push && git push origin "v$VERSION"

echo "Done. CI will build and publish v$VERSION."
