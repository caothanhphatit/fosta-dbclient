-- Sample keys for the Redis test engine (dev/docker-compose.engines-nosql.yml). Loaded by dev/seed-nosql.sh with
--   redis-cli EVAL "$(cat redis.lua)" 0
-- Idempotent: every run replaces the same keys.

redis.call('FLUSHDB')

-- users:<id> hashes, with a JSON profile per user (RedisJSON is built into redis:8).
local cities = { 'Hanoi', 'Berlin', 'Lima', 'Osaka', 'Austin' }
for i = 1, 200 do
  local key = 'users:' .. i
  redis.call('HSET', key, 'name', 'User ' .. i, 'email', 'user' .. i .. '@example.com', 'city', cities[(i % 5) + 1], 'visits', i * 3)
  if i <= 20 then
    redis.call('JSON.SET', 'users:' .. i .. ':profile', '$',
      '{"id":' .. i .. ',"name":"User ' .. i .. '","tags":["t' .. (i % 3) .. '","beta"],"address":{"city":"' .. cities[(i % 5) + 1] .. '","zip":"' .. (10000 + i) .. '"},"active":' .. tostring(i % 2 == 0) .. '}')
  end
end

-- sessions:<token> strings with a TTL.
for i = 1, 100 do
  redis.call('SET', 'sessions:' .. string.format('%08x', i * 2654435761 % 4294967296), '{"user":' .. i .. ',"ip":"10.0.0.' .. i .. '"}', 'EX', 86400)
end

-- cache:* strings: text, JSON, a counter, and binary bytes.
redis.call('SET', 'cache:homepage', '<html><body>Hello</body></html>')
redis.call('SET', 'cache:config', '{"theme":"dark","features":{"search":true,"beta":false}}')
redis.call('SET', 'cache:counter', 42)
redis.call('SET', 'cache:binary', '\0\255\254binary\1')
redis.call('SET', 'cache:big', string.rep('x', 2 * 1024 * 1024))

-- A sorted set.
for i = 1, 500 do
  redis.call('ZADD', 'leaderboard', (i * 7919) % 10007, 'player:' .. i)
end

-- A stream with a consumer group.
for i = 1, 1000 do
  redis.call('XADD', 'events', '*', 'type', (i % 3 == 0) and 'click' or 'view', 'user', 'users:' .. ((i % 200) + 1), 'n', i)
end
redis.call('XGROUP', 'CREATE', 'events', 'analytics', '0')
redis.call('XREADGROUP', 'GROUP', 'analytics', 'worker-1', 'COUNT', 10, 'STREAMS', 'events', '>')

-- Big collections for paging.
for i = 1, 20000 do
  redis.call('RPUSH', 'big:list', 'item-' .. i)
end
for i = 1, 20000 do
  redis.call('HSET', 'big:hash', 'field-' .. i, 'value-' .. i)
end
for i = 1, 5000 do
  redis.call('SADD', 'big:set', 'member-' .. i)
end

-- Sets and a key in another database.
redis.call('SADD', 'tags', 'red', 'green', 'blue')
redis.call('SELECT', 1)
redis.call('SET', 'db1:hello', 'world')
redis.call('SELECT', 0)

return redis.call('DBSIZE')
