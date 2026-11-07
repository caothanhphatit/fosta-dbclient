#!/bin/sh
# Loads sample data into the NoSQL test engines of dev/docker-compose.engines-nosql.yml. Re-runnable: it replaces what it
# created before. Needs curl and awk (Git Bash on Windows is fine).
#
#   sh dev/seed-nosql.sh            # Redis + Elasticsearch (+ OpenSearch when it answers on 9201)
#
# Redis:          users:* hashes, users:*:profile RedisJSON docs, sessions:* with TTL, cache:* strings (text, JSON,
#                 binary, a 2 MiB value), leaderboard (zset), events (stream + consumer group), big:list / big:hash
#                 (20,000 elements) and big:set for paging, a key in db 1.
# Elasticsearch:  products (nested variants, geo_point, dates, dense_vector, multi-fields; 300 docs) with the alias
#                 products-current, ILM policy logs-policy, ingest pipeline logs-pipeline, component template
#                 fosta-logs-mappings, index template logs-template, data stream logs-app with 20,000 time-series docs.
# OpenSearch:     the same products index (without the vector field), ISM policy logs-policy, data stream logs-app
#                 with 2,000 docs.
set -eu

# Native path on Git Bash (docker.exe gets no path conversion below).
here=$(cd "$(dirname "$0")" && (pwd -W 2>/dev/null || pwd))
ES=${ES_URL:-https://127.0.0.1:9200}
ES_AUTH=${ES_AUTH:-elastic:studio-elastic}
OS=${OS_URL:-http://127.0.0.1:9201}
export MSYS_NO_PATHCONV=1
# curl.exe and docker.exe are native on Git Bash: no /dev/null there.
if pwd -W >/dev/null 2>&1; then devnull=NUL; else devnull=/dev/null; fi

echo "== Redis"
docker cp "$here/seed/redis.lua" fosta-dev-nosql-redis-1:/tmp/seed.lua
docker exec fosta-dev-nosql-redis-1 redis-cli -a studio-redis --no-auth-warning --eval /tmp/seed.lua

# es METHOD PATH [BODY]  (BODY "-" reads NDJSON from stdin)
es() {
  if [ "${3:-}" = "-" ]; then
    curl -sS -k -u "$ES_AUTH" -X "$1" "$ES$2" -H 'Content-Type: application/x-ndjson' --data-binary @- -o "$devnull" -w "$1 $2 -> %{http_code}\n"
  elif [ -n "${3:-}" ]; then
    curl -sS -k -u "$ES_AUTH" -X "$1" "$ES$2" -H 'Content-Type: application/json' -d "$3" -o "$devnull" -w "$1 $2 -> %{http_code}\n"
  else
    curl -sS -k -u "$ES_AUTH" -X "$1" "$ES$2" -o "$devnull" -w "$1 $2 -> %{http_code}\n"
  fi
}
os() {
  if [ "${3:-}" = "-" ]; then
    curl -sS -X "$1" "$OS$2" -H 'Content-Type: application/x-ndjson' --data-binary @- -o "$devnull" -w "$1 $2 -> %{http_code}\n"
  elif [ -n "${3:-}" ]; then
    curl -sS -X "$1" "$OS$2" -H 'Content-Type: application/json' -d "$3" -o "$devnull" -w "$1 $2 -> %{http_code}\n"
  else
    curl -sS -X "$1" "$OS$2" -o "$devnull" -w "$1 $2 -> %{http_code}\n"
  fi
}

products_mapping() {
  vector=$1
  cat <<EOF
{
  "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
  "mappings": {
    "properties": {
      "name": { "type": "text", "fields": { "keyword": { "type": "keyword", "ignore_above": 256 } } },
      "description": { "type": "text", "analyzer": "english" },
      "brand": { "type": "keyword" },
      "tags": { "type": "keyword" },
      "price": { "type": "scaled_float", "scaling_factor": 100 },
      "rating": { "type": "half_float" },
      "in_stock": { "type": "boolean" },
      "created": { "type": "date" },
      "location": { "type": "geo_point" },
      "supplier": { "properties": { "name": { "type": "keyword" }, "country": { "type": "keyword" } } },
      "variants": { "type": "nested", "properties": { "sku": { "type": "keyword" }, "color": { "type": "keyword" }, "stock": { "type": "integer" } } }
      $vector
    }
  }
}
EOF
}

# 300 products as _bulk NDJSON; $1 = 1 to include the embedding.
products_bulk() {
  awk -v with_vector="$1" 'BEGIN {
    split("acme globex initech umbrella hooli", brands, " ");
    split("red green blue black white", colors, " ");
    split("Hanoi Berlin Lima Osaka Austin", cities, " ");
    for (i = 1; i <= 300; i++) {
      b = brands[(i % 5) + 1];
      printf "{\"index\":{\"_index\":\"products\",\"_id\":\"%d\"}}\n", i;
      vector = with_vector == 1 ? sprintf(",\"embedding\":[%.3f,%.3f,%.3f,%.3f]", (i % 7) / 7, (i % 11) / 11, (i % 13) / 13, 0.5) : "";
      printf "{\"name\":\"%s widget %d\",\"description\":\"A sturdy widget from %s, model %d, with fast shipping\",\"brand\":\"%s\",\"tags\":[\"t%d\",\"widget\"],\"price\":%.2f,\"rating\":%.1f,\"in_stock\":%s,\"created\":\"2026-%02d-%02dT10:00:00Z\",\"location\":{\"lat\":%.4f,\"lon\":%.4f},\"supplier\":{\"name\":\"%s supply\",\"country\":\"%s\"},\"variants\":[{\"sku\":\"%d-A\",\"color\":\"%s\",\"stock\":%d},{\"sku\":\"%d-B\",\"color\":\"%s\",\"stock\":%d}]%s}\n",
        b, i, b, i, b, i % 4, 5 + (i * 37 % 500) / 3, 1 + (i % 5), (i % 3 == 0) ? "false" : "true", (i % 12) + 1, (i % 28) + 1,
        10 + (i % 50), 100 + (i % 60), b, cities[(i % 5) + 1], i, colors[(i % 5) + 1], i % 20, i, colors[((i + 2) % 5) + 1], (i * 3) % 20, vector;
    }
  }'
}

# $1 = count, $2 = action line
logs_bulk() {
  awk -v n="$1" 'BEGIN {
    split("info info info warn error debug", levels, " ");
    split("api worker scheduler auth", services, " ");
    for (i = 0; i < n; i++) {
      t = 1793000000 + i * 30;
      printf "{\"create\":{}}\n";
      printf "{\"@timestamp\":\"%s\",\"level\":\"%s\",\"service\":\"%s\",\"msg\":\"request %d handled\",\"duration_ms\":%d,\"status\":%d,\"host\":{\"name\":\"node-%d\"}}\n",
        strftime("%Y-%m-%dT%H:%M:%SZ", t, 1), toupper(levels[(i % 6) + 1]), services[(i % 4) + 1], i, (i * 7919) % 2000, (i % 17 == 0) ? 500 : 200, i % 3;
    }
  }'
}

if curl -sS -k -u "$ES_AUTH" "$ES/" -o "$devnull" 2>/dev/null; then
  echo "== Elasticsearch ($ES)"
  es DELETE "/_data_stream/logs-app" || true
  es DELETE "/products" || true
  es DELETE "/_index_template/logs-template" || true
  es DELETE "/_component_template/fosta-logs-mappings" || true
  es PUT "/products" "$(products_mapping ',"embedding": { "type": "dense_vector", "dims": 4, "similarity": "cosine" }')"
  products_bulk 1 | es POST "/_bulk?refresh=true" -
  es POST "/_aliases" '{"actions":[{"add":{"index":"products","alias":"products-current","is_write_index":true}}]}'
  es PUT "/_ilm/policy/logs-policy" '{"policy":{"phases":{"hot":{"actions":{"rollover":{"max_primary_shard_size":"1gb","max_age":"7d"}}},"warm":{"min_age":"7d","actions":{"forcemerge":{"max_num_segments":1}}},"delete":{"min_age":"30d","actions":{"delete":{}}}}}}'
  es PUT "/_ingest/pipeline/logs-pipeline" '{"description":"Normalise log events","processors":[{"lowercase":{"field":"level"}},{"rename":{"field":"msg","target_field":"message","ignore_missing":true}},{"set":{"field":"event.ingested","value":"{{_ingest.timestamp}}"}}]}'
  es PUT "/_component_template/fosta-logs-mappings" '{"template":{"mappings":{"properties":{"@timestamp":{"type":"date"},"level":{"type":"keyword"},"service":{"type":"keyword"},"message":{"type":"text"},"duration_ms":{"type":"long"},"status":{"type":"short"},"host":{"properties":{"name":{"type":"keyword"}}},"event":{"properties":{"ingested":{"type":"date"}}}}}}}'
  es PUT "/_index_template/logs-template" '{"index_patterns":["logs-app*"],"data_stream":{},"priority":200,"composed_of":["fosta-logs-mappings"],"template":{"settings":{"number_of_replicas":0,"index.lifecycle.name":"logs-policy","index.default_pipeline":"logs-pipeline"}}}'
  logs_bulk 20000 | es POST "/logs-app/_bulk?refresh=true" -
else
  echo "Elasticsearch is not reachable on $ES; skipped."
fi

if curl -sS "$OS/" -o "$devnull" 2>/dev/null; then
  echo "== OpenSearch ($OS)"
  os DELETE "/_data_stream/logs-app" || true
  os DELETE "/products" || true
  os DELETE "/_plugins/_ism/policies/logs-policy" || true
  os PUT "/products" "$(products_mapping '')"
  products_bulk 0 | os POST "/_bulk?refresh=true" -
  os POST "/_aliases" '{"actions":[{"add":{"index":"products","alias":"products-current"}}]}'
  os PUT "/_plugins/_ism/policies/logs-policy" '{"policy":{"description":"Logs","default_state":"hot","states":[{"name":"hot","actions":[],"transitions":[{"state_name":"delete","conditions":{"min_index_age":"30d"}}]},{"name":"delete","actions":[{"delete":{}}],"transitions":[]}],"ism_template":[{"index_patterns":["logs-app*"],"priority":100}]}}'
  os PUT "/_ingest/pipeline/logs-pipeline" '{"description":"Normalise log events","processors":[{"lowercase":{"field":"level"}},{"rename":{"field":"msg","target_field":"message","ignore_missing":true}}]}'
  os PUT "/_index_template/logs-template" '{"index_patterns":["logs-app*"],"data_stream":{},"priority":200,"template":{"settings":{"number_of_replicas":0,"index.default_pipeline":"logs-pipeline"}}}'
  logs_bulk 2000 | os POST "/logs-app/_bulk?refresh=true" -
else
  echo "OpenSearch is not reachable on $OS; skipped."
fi
