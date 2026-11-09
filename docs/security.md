# Security and privacy

What Fosta Studio stores, what it sends where, and how it keeps you from changing the wrong database. To report a vulnerability,
see [SECURITY.md](../SECURITY.md).

## What leaves your machine

Fosta Studio has no account and no telemetry. It connects only to:

- **Your databases**, and the SSH hosts you configure for tunnels.
- **The AI provider you choose**, only when you use the assistant or embeddings, with your own key. See [The assistant](#the-assistant).
- **GitHub, once a day,** to check for a new release (`api.github.com`, with no data about you or your databases). Turn it off under
  *Settings → Check for updates*. The installed app downloads updates from the project's GitHub releases and installs them only if
  their signature matches the key built into the app.
- **download.oracle.com**, only if you ask the app to download Oracle Instant Client; the file must match a pinned SHA-256.

The optional MCP server listens only on `127.0.0.1` and sends nothing on its own; what an MCP client reads goes wherever that client
sends it. See [MCP clients](#mcp-clients).

## What is stored, and where

Everything lives next to the executable (the portable app), in the app's data folder (the installed app), or in the `/data` volume
(the web edition).

| File | Holds | Secrets? |
|---|---|---|
| `fosta-studio.json` | Workspaces, connections, commands, settings | No: passwords and keys are never written here |
| Windows Credential Manager (desktop) | Saved database passwords, SSH passphrases, AI and embedding keys | Yes, protected by Windows |
| `fosta-studio-secrets.enc` + `fosta-studio-secret.key` (web edition) | The same secrets, encrypted with XChaCha20-Poly1305 | Yes; set `FOSTA_STUDIO_SECRET_KEY` to keep the key out of the volume |
| `fosta-studio-history.jsonl` | Query history (text, time, duration, row count) | Statement text as you typed it; turn history off or clear it under *Settings* |
| Windows Credential Manager, `app:mcp-token` (desktop) | The MCP server's bearer token | Yes |
| `fosta-studio-audit.jsonl` | Blocked, confirmed and relaxed operations, assistant runs and every MCP call | Statement excerpts |
| Log folder | Application log | No passwords; statements are truncated |

A saved password is tied to its server. If you change a connection's host, port, user, SSH route, TLS settings or command without
typing the password again, the saved one is deleted instead of being sent to the new target.

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

**For production databases, also connect with a database user that can only read.** The app's rules are a safety net, not a
replacement for database permissions.

## The assistant

The model sees table and column names, the editor text, errors and task logs you share. Statements run only when you press *Run*,
under the workspace's rules. The optional read-only query tool lets the assistant read up to 50 rows per query (10 seconds, one
statement); those rows go to your provider. It is off for Production and Read-only workspaces unless you allow it, and every run is
shown in the chat and written to the audit log. Database content is passed to the model as untrusted data.

## MCP clients

The MCP server ([mcp.md](mcp.md)) lets AI clients such as Claude Code, Claude Desktop or Cursor use saved connections. It is off
until you turn it on in *Settings → MCP server*, and each workspace is invisible to clients until *Edit workspace → MCP clients*
allows it.

What a client **can see**: the connections of those workspaces (id, name, engine, workspace), their databases, tables, columns and
relations, and the rows its read-only queries return (100 by default, 500 at most). It never sees passwords, SSH or TLS secrets,
hosts, users, or the confirmation token.

What a client **can do**:

- **Read-only** workspaces: read the schema and run one read-only statement at a time, in a read-only session, for 30 seconds at most.
  Anything the engine does not classify as a read is refused.
- **Ask before writes** workspaces: also ask to run a statement that changes data. You see the statement, its category and the
  client's name in a dialog and approve it, typing the table or workspace name where the rules ask for that; denying, closing or 120
  seconds without an answer runs nothing. What the rules block stays blocked.
- **Production and Read-only** workspaces allow at most read-only access.

How it is protected: the desktop server binds `127.0.0.1` only, refuses non-loopback peers, foreign `Host` names and foreign
`Origin` headers, and needs a random 256-bit bearer token kept in Credential Manager (rotate it in Settings). Results that contain
database content are marked as untrusted data for the client's model. Every call, approval, denial and timeout is written to the
audit log with caller `mcp` and the name the client reported; that name is self-reported, so approve what a statement does, not
who asks.

On the **web edition**, `/mcp` takes the server's access token as a bearer token and never the session cookie, and is off when
authentication is off. That token also signs into the UI, so a client holding it could open the app and approve its own requests:
give MCP access on the web edition only to clients you would trust with the token.

## Network security

- **TLS** from `disable` to `verifyFull`, with your own CA, a client certificate, a server name or a pinned certificate (the pin is
  checked against the server's own certificate).
- **SSH tunnels** check host keys (accept new, strict or off) and support a jump host.

## The web edition

- Sign-in with an access token exchanged for an `HttpOnly`, `SameSite=Strict` session cookie; one token per server.
- Requests must carry a custom header and a same-origin `Origin`; responses set a strict Content Security Policy.
- The image is distroless and runs as a non-root user.
- Put it behind HTTPS for network access and set `FOSTA_STUDIO_SECURE_COOKIE=on`. See [web-edition.md](web-edition.md).
