#!/bin/zsh
# Writes the public tree for a release to $1 (default dist/public): the committed HEAD minus
# internal material (docs/, spikes/, spike-only tools), with the workspace pointed at the
# app crates only. Used by tools/release/publish.sh.
set -e
cd "${0:A:h}/../.."
OUT=${1:-dist/public}
rm -rf $OUT && mkdir -p $OUT
git archive HEAD | tar -x -C $OUT
rm -rf $OUT/docs $OUT/spikes $OUT/tools/bundle_mac.sh $OUT/tools/release/publish.sh
# Workspace members: the crates only.
sed -i '' 's|^members = \["crates/\*", "spikes/\*"\]|members = ["crates/*"]|' $OUT/Cargo.toml
grep -q '^members = \["crates/\*"\]' $OUT/Cargo.toml || { echo "snapshot: couldn't rewrite workspace members" >&2; exit 1; }
# Nothing personal or secret may slip through.
# The public repo's own address (satchlj/puzil) and the app identifier (com.satchlj.puzil)
# are allowed; any other match is not.
PAT="satya|satchlj|thesatchlj|benson\.earth|1215456|gho_[A-Za-z0-9]{20}"
if grep -rIniE "$PAT" $OUT --exclude=Cargo.lock --exclude=snapshot.sh | sed -E 's#satchlj/puzil##g; s#com\.satchlj\.puzil##g' | grep -iE ":[0-9]+:.*($PAT)"; then
  echo "snapshot: personal or secret strings found (above); not publishing" >&2; exit 1
fi
echo $OUT
