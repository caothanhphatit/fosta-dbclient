#!/usr/bin/env sh
# Waits until the MongoDB container from dev/compose/mongo.yml has run its init script and restarted for real.
# The image has no healthcheck, so `docker compose up --wait` returns before the seed is loaded; while seeding, the
# entrypoint runs a temporary localhost-only mongod, so a ping alone could succeed too early.
set -e
cd "$(dirname "$0")/.."
COMPOSE="docker compose -f dev/compose/mongo.yml"
for attempt in $(seq 1 90); do
  if $COMPOSE logs mongo 2>/dev/null | grep -q "MongoDB init process complete"; then break; fi
  sleep 2
done
for attempt in $(seq 1 60); do
  if $COMPOSE exec -T mongo mongosh --quiet -u root -p studio-root --authenticationDatabase admin \
    --eval 'quit(db.getSiblingDB("events").getCollectionNames().length > 0 ? 0 : 1)' >/dev/null 2>&1; then
    echo "MongoDB is ready"
    exit 0
  fi
  sleep 2
done
echo "MongoDB did not become ready" >&2
exit 1
