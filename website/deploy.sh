#!/usr/bin/env sh
# Publishes website/ to the web server behind https://db.fosta.app.
#   website/deploy.sh <ssh-key> [user@host]
# The key is passed on the command line and never stored in the repository.
# The server serves /var/www/db.fosta.app with nginx (see website/README.md).
set -eu

KEY="${1:?usage: website/deploy.sh <ssh-key> [user@host]}"
TARGET="${2:-ubuntu@13.229.95.37}"
ROOT=/var/www/db.fosta.app
HERE="$(cd "$(dirname "$0")" && pwd)"

# Upload to a fresh directory, then swap it in, so visitors never see a half-copied site.
STAGE="$ROOT.new"
tar -C "$HERE" --exclude=deploy.sh --exclude=README.md -czf - . |
  ssh -i "$KEY" -o BatchMode=yes "$TARGET" "sudo rm -rf $STAGE && sudo mkdir -p $STAGE && sudo tar -xzf - -C $STAGE &&
    sudo chown -R root:root $STAGE && sudo find $STAGE -type d -exec chmod 755 {} + && sudo find $STAGE -type f -exec chmod 644 {} + &&
    sudo rm -rf $ROOT.old && { [ -d $ROOT ] && sudo mv $ROOT $ROOT.old || true; } && sudo mv $STAGE $ROOT && sudo rm -rf $ROOT.old"
echo "Published to $TARGET:$ROOT"
