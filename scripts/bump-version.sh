#!/bin/sh
# Prepare a release: set the workspace version, the "current version" lines and
# the version on the naive.dev home page (site/index.html).
#
#   scripts/bump-version.sh 0.1.22
#
# Then open a pull request with the result. When it merges, the release
# workflow tags the merge commit (v0.1.22) and builds, signs and publishes it.
set -eu

version="${1:?usage: scripts/bump-version.sh X.Y.Z}"
case "$version" in
  *[!0-9.]* | .* | *. | *..*) echo "version must look like 1.2.3" >&2; exit 1 ;;
esac

cd "$(dirname "$0")/.."
old=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[ -n "$old" ] || { echo "cannot find the workspace version in Cargo.toml" >&2; exit 1; }

# Only the version lines, not historical mentions such as "done as of v0.1.21".
sed -i.bak "s/^version = \"$old\"$/version = \"$version\"/" Cargo.toml
sed -i.bak "s/^- \*\*Current version:\*\* $old$/- **Current version:** $version/" CLAUDE.md
sed -i.bak "s/^\*\*Current version:\*\* $old$/**Current version:** $version/" README.md
sed -i.bak "s/^\*\*Version:\*\* $old$/**Version:** $version/" docs/ARCHITECTURE.md
# The naive.dev hero badge (site/ publishes itself from master).
sed -i.bak "s/Open Source Game Engine · v$old</Open Source Game Engine · v$version</" site/index.html
rm -f Cargo.toml.bak CLAUDE.md.bak README.md.bak docs/ARCHITECTURE.md.bak site/index.html.bak

# Refresh the workspace crates' versions in the lockfile without upgrading
# any dependency.
cargo update --workspace --offline >/dev/null 2>&1 || cargo update --workspace >/dev/null

echo "Version $old -> $version"
git --no-pager diff --stat
