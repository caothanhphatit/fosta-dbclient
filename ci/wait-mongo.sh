#!/usr/bin/env sh
# Waits until the MongoDB container from dev/compose/mongo.yml has run its init script (the `events` database exists).
# The image has no healthcheck, so `docker compose up --wait` returns before the seed is loaded.
set -e
cd "$(dirname "$0")/.."
for attempt in $(seq 1 60); do
  if docker compose -f dev/compose/mongo.yml exec -T mongo mongosh --quiet -u root -p studio-root --authenticationDatabase admin \
    --eval 'quit(db.getSiblingDB("events").getCollectionNames().length > 0 ? 0 : 1)' >/dev/null 2>&1; then
    echo "MongoDB is ready"
    exit 0
  fi
  sleep 2
done
echo "MongoDB did not become ready" >&2
exit 1
