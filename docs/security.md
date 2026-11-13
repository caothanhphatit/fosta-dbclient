# Security and privacy

What Fosta Studio stores, what it sends where, and how it keeps you from changing the wrong database. To report a vulnerability,
see [SECURITY.md](../SECURITY.md).

## What leaves your machine

Fosta Studio has no account and no telemetry. It connects only to:

- **Your databases**, and the SSH hosts you configure for tunnels.
- **The AI provider you choose**, only when you use the assistant or embeddings, with your own key. See [The assistant](#the-assistant).
- **GitHub, once a day,** to check for a new release (`api.github.com`, with no data about you or your databases). Turn it off under
  _Settings → Check for updates_. The installed app downloads updates from the project's GitHub releases and installs them only if
  their signature matches the key built into the app.
- **download.oracle.com**, only if you ask the app to download Oracle Instant Client; the file must match a pinned SHA-256.

The optional MCP server listens only on `127.0.0.1` on the desktop (on the web edition it is `/mcp` on the server's own port) and
sends nothing on its own; what an MCP client reads goes wherever that client sends it. See [MCP clients](#mcp-clients).

## What is stored, and where

Everything lives next to the executable (the portable app), in the app's data folder (the installed app), or in the `/data` volume
(the web edition).

| File                                                                                                | Holds                                                                                     | Secrets?                                                                          |
| --------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| `fosta-studio.json`                                                                                 | Workspaces, connections, commands, settings                                               | No: passwords and keys are never written here                                     |
| Windows Credential Manager (desktop)                                                                | Saved database passwords, SSH passphrases, AI and embedding keys                          | Yes, protected by Windows                                                         |
| `fosta-studio-secrets.enc` + `fosta-studio-secret.key` (web edition)                                | The same secrets, encrypted with XChaCha20-Poly1305                                       | Yes; set `FOSTA_STUDIO_SECRET_KEY` to keep the key out of the volume              |
| `fosta-studio-history.jsonl`                                                                        | Query history (text, time, duration, row count)                                           | Statement text as you typed it; turn history off or clear it under _Settings_     |
| `fosta-studio-tabs.json`                                                                            | The open tabs of each workspace, with the text of query tabs (restored on the next start) | Query text as you typed it, also with history off; closing a tab removes its text |
| `fosta-studio-tabs.json.corrupt`                                                                    | The last tabs file that did not parse, moved aside (only when that happened)              | The same query text; delete it when you no longer need it                         |
| `app:mcp-token` in Windows Credential Manager (desktop) or `fosta-studio-secrets.enc` (web edition) | The MCP server's bearer token, for MCP clients only                                       | Yes                                                                               |
| `fosta-studio-audit.jsonl`                                                                          | Blocked, confirmed and relaxed operations, assistant runs and every MCP call              | Statement excerpts                                                                |
| Log folder                                                                                          | Application log                                                                           | No passwords; statements are truncated                                            |

On Linux and macOS (and the web edition's `/data` volume) the app writes these files readable by their owner only
(mode `0600`), the key files included; files written by an earlier version are restricted on the next start. On Windows a
file takes the permissions of its folder: the installed app keeps its data under your user profile, the portable app next to the executable.

A saved password is tied to its server. If you change a connection's host, port, user, SSH route, TLS settings or command without
typing the password again, the saved one is deleted instead of being sent to the new target.

API keys work the same way. An AI provider's key stays with its provider and endpoint, the embedding key with its endpoint
URL and the way it is sent; the model, the dimensions and the name do not matter. Instead of typing a key again you can
pick one already saved for the same API (same scheme, host and port, one path inside the other, such as
`https://api.openai.com/v1` and `…/v1/embeddings`): the dialog only gets a reference and a label, and the app copies the
secret itself. Keys saved for another host are not offered, and a reference to one is refused.

Saving embedding settings for a new endpoint without a key is refused ("The endpoint changed, enter the API key again or
pick a saved key"), so requests never go out unauthenticated by accident; choose "No key" for a server that needs none, or
use a local URL (`localhost`, `*.localhost`, a loopback address). A failed request that sent no key says so next to the HTTP
status.

## Safety rules

Every statement the app runs (query tabs, grid edits, filters, imports, consoles, the assistant) is classified as read, write, DDL,
destructive or administrative, then checked against the workspace's rules:

- **Open**: everything runs; `DROP`, `TRUNCATE` and `UPDATE`/`DELETE` without `WHERE` ask once first.
- **Guarded**: writes and DDL ask for confirmation; destructive and administrative statements ask you to type the name.
- **Read-only**: only reads run. A workspace marked **Production** starts here, and making its rules looser needs its name typed.
- **Custom**: your own matrix.

Read-only is also enforced by the database, not only by the app: read-only sessions on PostgreSQL, MySQL/MariaDB, ClickHouse and
SQLite (re-applied before every run), a read-only transaction on Oracle, a rolled-back transaction on SQL Server, and a session check
on MongoDB, Redis and Elasticsearch/OpenSearch. Statements that run something else (`EXPLAIN ANALYZE`, `EXECUTE`, dynamic SQL) are
rated by what they run, and functions with side effects (`pg_terminate_backend`, `dblink`, `LOAD_FILE`, `OPENQUERY`, `UTL_HTTP`…)
count as administrative.

Grid saves check that the row has not changed since you loaded it; a conflict shows both versions instead of overwriting.
MongoDB document edits are saved the same way (one save, the same rules) and compare only the paths you edited.

MongoDB bulk changes from the collection workspace are rated by what they do: an update preview reads (it simulates the
update on a few documents and refuses operators it cannot simulate exactly), "Update matching documents" is a write, or
destructive when the filter matches every document, and "Delete matching documents" is always destructive, so a Guarded
workspace asks for the collection's name and a Read-only one blocks it. "Matches every document" covers an empty filter and the
trivially true forms (`{ $expr: true }`, `{ _id: { $exists: true } }`, `{ $and: [{}] }`, `{ $or: [ …, {} ] }`,
`{ $jsonSchema: {} }`, a lone `$comment`); an expression the server would have to evaluate, such as `{ $expr: { $eq: [1, 1] } }`,
is not caught, so this guards against mistakes and pasted filters rather than being a boundary. An aggregation with `$out`
(which drops and replaces its target, possibly the source itself) and a TTL index (`expireAfterSeconds`, which deletes
documents on its own) are destructive too; `$merge` is a write. Change streams (`$changeStream`) never end and are refused by
the workspace's aggregations and explain. Server-side JavaScript (`$where`, `$function`, `$accumulator`) is refused in filters and
updates whatever the rules. Database and collection names are used exactly as given: a name with leading or trailing spaces is
refused, so the rules and the call always see the same collection.

Reading credentials or other sessions' data is administration, whatever the command: `system.users`, `system.roles`,
`system.keys`, `system.backup_users` and `system.new_users`, the profiler (`system.profile`), the oplog and the startup log
(`oplog.rs`, `startup_log`), every collection of the `local` database, `usersInfo`, `currentOp`, `getLog`, `hostInfo`,
`getCmdLineOpts`, and the stages `$currentOp`, `$listSessions`, `$listLocalSessions`, `$planCacheStats`,
`$listSampledQueries` and `$queryStats`. In a query tab, where the command does not name its database, the `local` database is
recognized by those collection names only. A view is rated by its own name: a view defined over one of these collections
(which needs an administrative `create` first, in the same database) reads as a plain read in the collection workspace. The
assistant never opens or reads the `admin`, `local` or `config` databases or any `system.*` collection.

**For production databases, also connect with a database user that can only read.** The app's rules are a safety net, not a
replacement for database permissions.

## The assistant

The model sees table and column names, the editor text, errors and task logs you share. Statements run only when you press _Run_,
under the workspace's rules. The optional read-only query tool lets the assistant read up to 50 rows per query (10 seconds, one
statement); those rows go to your provider. It is off for Production and Read-only workspaces unless you allow it, and every run is
shown in the chat and written to the audit log. Database content is passed to the model as untrusted data.

Rows and documents the read tools return (`run_read_query`, `sample_documents`) are redacted before they reach the
model, and the chat shows the same redacted values: a column or field named like a credential or sensitive identifier
(`password`, `pass`, `pw`, `user_pass`, `secret`, `token`, `api_key`, `hash`, `pin`, `ssn`, `card`, `iban`, date of
birth, activation and reset keys, and the usual words for "password" in German, Spanish, French, Portuguese, Italian,
Dutch, Vietnamese and Russian…, see the list under [Learn this database](#learn-this-database)) keeps its name and shows
`[redacted]` instead of its value (a null stays null), at any depth: nested documents, dotted keys, arrays, JSON columns
and JSON kept as a string inside a value (a logged request body). Secret-looking text in any value is replaced by
`[redacted]` too: API keys, tokens, password hashes (`$2b$…`, `$argon2…`, WordPress `$P$…` and `$wp$…`, Drupal `$S$…`,
phpBB `$H$…`, Apache `$apr1$…`, Spring `{bcrypt}…` and `{noop}…`, LDAP `{SSHA}…`), PEM blocks, the user information of
URLs (`user:password@host`), and the value after a credential key in text (`Password=…;` in a connection string,
`?password=…&` in a URL, `DB_PASSWORD: …`, `--password=…`). Error messages from `run_read_query` and `sample_documents`
get the same text checks. Contact data (emails, names, phone numbers, addresses) is not redacted there: when you allow
reads, answering questions about your data needs it.

Redaction goes by column and field names. A value the query renames or computes from such a column (an alias such as
`SELECT password AS p`, an expression such as `concat(password)` or `left(password, 3)`, a cast, a whole-row value, a
MongoDB `$project` that renames the field) is caught only by the value checks, which miss plain-text passwords, and a
filter on a redacted column (`WHERE password LIKE 'a%'`) still tells the model about its values. Error messages are
checked for secret-looking text only, so an error that quotes a plain value (`invalid input syntax for type integer:
"hunter2"`) passes. Allow reads only where the assistant may see the data. None of this is ever kept in a learned profile
(below).

On MongoDB the same setting covers sampled documents: `sample_documents` reads at most 20 documents of a collection in the
chat's database (or what leaves a pipeline stage, on a bounded sample), redacted as above and with long values cut,
through the same read-only gateway check, shown in the chat and audited; and the collection workspace sends a few documents (cut the same way) with the context,
only when reading that collection is a plain read for the rules (not a system collection, not `admin`, `local` or `config`, not
administration). Where the setting is off, neither reaches the model: it sees field names, types, the filter you typed and the
errors, with the values cut out of the errors (the key of a duplicate-key error, every quoted literal). An error message spread
over several lines may keep values on its later lines. Whatever the setting, the last error, the workspace context and a
task log sent with "Ask assistant" (its command line and last lines) get the text checks above, so `-e
POSTGRES_PASSWORD=…`, `--password=…`, connection strings and tokens in them are redacted.

Filters and pipelines it suggests, in the chat or through `open_collection_workspace`, are only pre-filled for you to review;
they never run on their own, and a workspace the assistant opens does not run its first page either (Documents waits for
_Find_). Both collection tools are pinned to the chat's connection and database, and refuse system collections, the `admin`,
`local` and `config` databases, stages that write (`$out`, `$merge`), server-side JavaScript and stages naming another database
(`{ db, coll }` in `$lookup`, `$unionWith` or `$merge`). A `mongo-pipeline` block in the chat that writes or names another
database is flagged under the block, and its buttons ask before applying it.

### Learn this database

"Learn this database" reads the structure of one database through the open connection: object names and kinds, columns
and types, keys, indexes, foreign keys, comments and the engine's row estimates (never a count of rows). System and
credential objects (`mysql.user`, `pg_authid`, `sys.*`, Elasticsearch `.security*` and the like) are left out. For
MongoDB the collection's top-level field names and types come from the same sample of up to 100 documents the object tree
uses to describe a collection; no value is kept.

Data is read only when the assistant may read data on that connection (the setting above, per workspace; off by default
for Read-only and Production workspaces). Then, as the assistant and through the same read-only gateway check:

- MongoDB samples up to 500 random documents per collection (`mongo.schema`, at most 200 collections) to learn nested
  field paths, their types, how often they occur and references between collections (path segments that are values
  used as keys, such as emails, ids, UUIDs, phone numbers or tokens, become `{key}`);
- Redis scans up to 5,000 key names with their types and TTLs (never values) and keeps them as key patterns such as
  `user:{id}:profile`: numbers, UUIDs, hashes, emails, IP addresses, phone numbers and token-like segments become
  placeholders, a segment that differs between many otherwise equal keys becomes `{name}`, and secret-looking text is
  redacted (a plain word seen in only one or two keys, such as `cart:alice`, is kept as it is);
- with "Sample column values" ticked in the dialog, a few distinct values per short column are read.

Without reads, learning is metadata only: the structure above, MongoDB collection options and index specs, and
Elasticsearch mappings, aliases and data streams (which are metadata, so they are always read). MongoDB then has no
nested paths and Redis no key patterns, and the build says so. Value hints learned earlier are dropped by a refresh that
may not sample them (reads off now, or "Sample column values" unticked). Columns whose names look like credentials or
sensitive identifiers (`password`, `pass`, `pw`, `secret`, `token`, `api_key`, `salt`, `hash`, `pin`, `otp`, `session`,
`ssn`, `card`, `cvv`, `iban`, `dob`, `activation_key`, `reset_key`, `passwort`, `contraseña`, `mot_de_passe`, `senha`,
`mat_khau`, `пароль`…) are never sampled and lose their comments and defaults. Columns and fields named like personal
data (`email`, `phone`, `address`, person names such as `full_name`, `first_name` or `customerName`, `ip_address`,
`mac_address`, `passport`; names ending in `login`, `user`, `author`, `owner`, `contact`, `mobile`, `tel` or `mail`;
`created_by` and the like; `tax_id`, `national_id`, `account_number`), and network address types (`inet`, `cidr`,
`macaddr`, `IPv4`), get no value read and no hint either (a yes/no flag such as `email_verified` does, but not a MongoDB
field that is only mostly a boolean; MongoDB reads whole sample documents to learn field paths, but keeps no value of
these fields). A plain `name` column is sampled, since it often names things rather than people. Values are kept as a
list only when there are at most 20 and each stands for at least 3 rows: counted in the sample on SQL engines and from
MongoDB's field statistics, and estimated from the row count for MongoDB's distinct reads; yes/no values are always
listed. So a small table never lists the values of a column that tells its rows apart, whatever its name. Values that are
emails, phone numbers, IP or MAC addresses, URLs, UUIDs or JSON are never kept as hints: a column where all of them share
such a format keeps only the format, and one where they are mixed with other values keeps nothing. Secret-looking text
(API keys, tokens, password hashes, PEM blocks) is replaced by `[redacted]` throughout the profile.

Learning sends nothing to a model. The summary step does, and only when you choose "Summarise" after seeing the estimate:
it sends the structure and your notes to the active LLM provider (with the model you pick), plus the value hints only when
the profile has them and reads are still allowed. What comes back is treated as untrusted text: fences and tags are
stripped, descriptions are cut to 200 characters, and your own descriptions are never replaced. In chats the profile is
passed to the model as untrusted data like the rest of the database context, without value hints where reads are off.
Builds, refreshes, summaries (provider, model, objects, tokens, whether data was sent) and deletions are written to the
audit log. The reads of a build are written as one entry per phase (the engine's reads, value sampling) with what was
read and the counts, so that a build does not push older entries out; a read that fails, and every refusal, gets an
entry of its own. Settings → "Assistant may learn databases" and Edit workspace → "Assistant may learn databases" turn learning and
summaries off; what was learned before stays until you delete it.

Profiles are stored in `fosta-studio-profiles/` next to the other files, one `<sha256 of connection and database>.json` per
database (at most 4 MB), owner-only like the files above: the folder has mode `0700` and the files `0600` on Linux and
macOS; on Windows they take the per-user permissions of the app's data folder. A file that does not parse is moved aside
as `.json.corrupt`. They hold structure, your notes and descriptions and, if sampled, short value lists; they are not
encrypted. Deleting a connection deletes its profiles; "What the assistant knows" → Delete removes one. Both also remove
the moved-aside and half-written files, and a deletion while the assistant is learning wins: that build saves nothing.

## MCP clients

The MCP server ([mcp.md](mcp.md)) lets AI clients such as Claude Code, Claude Desktop or Cursor use saved connections. It is off
until you turn it on in _Settings → MCP server_, and each workspace is invisible to clients until _Edit workspace → MCP clients_
allows it.

What a client **can see**: the connections of those workspaces (id, name, engine, workspace), their databases, tables, columns and
relations, and the rows its read-only queries return (100 by default, 500 at most). It never sees passwords, SSH or TLS secrets,
hosts, users, or the confirmation token.

Rows from `run_read_query` are redacted the way the in-app assistant's are ([The assistant](#the-assistant)): columns and
fields named like credentials show `[redacted]`, secret-looking text and password hashes are replaced, and its error
messages get the same text checks, with the same limits (a renamed or computed column is caught only by the value checks).
Results of `run_query`, the statements you approve one by one, are returned as they are.

What a client **can do**:

- **Read-only** workspaces: read the schema and run one read-only statement at a time, in a read-only session, for 30 seconds at most.
  Anything the engine does not classify as a read is refused.
- **Ask before writes** workspaces: also ask to run a statement that changes data. You see the whole statement (at most 20,000
  characters; longer ones are refused), its category and the client's name in a dialog and approve it, typing the table or
  workspace name where the rules ask for that; denying, closing or 120 seconds without an answer runs nothing. What the rules block
  stays blocked, and an approval is checked again against the settings at the moment it runs: turning the server off, lowering the
  workspace's MCP access or tightening its rules while the dialog is open means nothing runs.
- **Production and Read-only** workspaces allow at most read-only access.
- A client never starts a workspace command; a connection that needs one works only while you have it running.

How it is protected: the desktop server binds `127.0.0.1` only, refuses non-loopback peers, foreign `Host` names and foreign
`Origin` headers, and needs a random 256-bit bearer token kept in Credential Manager (rotate it in Settings; rotating, turning the
server off or changing its port ends every session and withdraws waiting approvals). Sessions expire after 30 minutes unused.
Results that contain database content are marked as untrusted data for the client's model. Every call, refusals included, and every
approval, denial and timeout is written to the audit log with caller `mcp` and the name the client reported; that name is
self-reported, so approve what a statement does, not who asks.

On the **web edition**, `/mcp` takes a separate random 256-bit **MCP token**, kept in the encrypted secret store and shown and
rotated in _Settings → MCP server_. It is never the access token: `/mcp` refuses the access token and the session cookie, and the
MCP token cannot sign in or call `/api`, so a client holding it cannot open the app, approve its own statements or change a
workspace's MCP access. An `Origin` header must name the server itself, and the endpoint is off when authentication is off.

## Network security

- **TLS** from `disable` to `verifyFull`, with your own CA, a client certificate, a server name or a pinned certificate (the pin is
  checked against the server's own certificate).
- **SSH tunnels** check host keys (accept new, strict or off) and support a jump host.

## The web edition

- Sign-in with an access token exchanged for an `HttpOnly`, `SameSite=Strict` session cookie; one token per server.
- Requests must carry a custom header and a same-origin `Origin`; responses set a strict Content Security Policy.
- The image is distroless and runs as a non-root user.
- Put it behind HTTPS for network access and set `FOSTA_STUDIO_SECURE_COOKIE=on`. See [web-edition.md](web-edition.md).
