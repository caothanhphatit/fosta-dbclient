#!/usr/bin/env sh
# Publishes the website to the web server behind https://dbcl.fosta.app.
#   website/deploy.sh <ssh-key> [user@host]
# The key is passed on the command line and never stored in the repository.
# The server serves /var/www/db.fosta.app with nginx (see website/README.md).
# The pages are rendered first (website/build.mjs: English at /, translations at /vi/, /zh/, /ja/); only the
# rendered output is uploaded, never the template, the i18n/ strings or the build script.
set -eu

KEY="${1:?usage: website/deploy.sh <ssh-key> [user@host]}"
TARGET="${2:-ubuntu@13.229.95.37}"
ROOT=/var/www/db.fosta.app
HERE="$(cd "$(dirname "$0")" && pwd)"

# Render into a fresh temporary folder; the build fails (and nothing is uploaded) if a string is missing or unsafe.
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT
node "$HERE/build.mjs" "$OUT/site"

# Upload to a fresh directory, then swap it in, so visitors never see a half-copied site.
STAGE="$ROOT.new"
tar -C "$OUT/site" --exclude=.fosta-site --exclude=README.md -czf - . |
  ssh -i "$KEY" -o BatchMode=yes "$TARGET" "sudo rm -rf $STAGE && sudo mkdir -p $STAGE && sudo tar -xzf - -C $STAGE &&
    sudo chown -R root:root $STAGE && sudo find $STAGE -type d -exec chmod 755 {} + && sudo find $STAGE -type f -exec chmod 644 {} + &&
    sudo rm -rf $ROOT.old && { [ -d $ROOT ] && sudo mv $ROOT $ROOT.old || true; } && sudo mv $STAGE $ROOT && sudo rm -rf $ROOT.old"
echo "Published to $TARGET:$ROOT"
