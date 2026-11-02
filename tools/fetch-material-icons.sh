#!/bin/sh
# Refresh assets/material-icons from the Material Icon Theme VSIX on Open VSX (MIT).
set -eu
VERSION="${1:-$(curl -sL https://open-vsx.org/api/PKief/material-icon-theme/latest | python3 -c 'import sys,json; print(json.load(sys.stdin)["version"])')}"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
curl -sL -o "$TMP/m.vsix" "https://open-vsx.org/api/PKief/material-icon-theme/$VERSION/file/PKief.material-icon-theme-$VERSION.vsix"
unzip -q "$TMP/m.vsix" -d "$TMP/x"
DEST="$(dirname "$0")/../assets/material-icons"
rm -rf "$DEST"
mkdir -p "$DEST"
cp -r "$TMP/x/extension/icons" "$DEST/icons"
cp "$TMP/x/extension/dist/material-icons.json" "$TMP/x/extension/LICENSE.txt" "$DEST/"
echo "$VERSION" > "$DEST/VERSION"
echo "Material icons $VERSION -> $DEST"
