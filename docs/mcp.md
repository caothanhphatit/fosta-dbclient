# MCP server

Fosta Studio can act as a [Model Context Protocol](https://modelcontextprotocol.io) server, so AI clients such as Claude Code,
Claude Desktop, Cursor, VS Code and Zed can use your saved connections. Clients never see passwords, hosts or users: they ask Fosta
Studio, which connects with the saved secrets, applies the workspace's safety rules and writes every call to the audit log.

It is **off by default**, twice over: the server is off until you turn it on, and each workspace stays invisible to clients until
you allow it.

## Turn it on

1. **Settings → MCP server…** → *Run the MCP server* → *Save*. The desktop app listens on `127.0.0.1:3910` (change the port there)
   and creates a random token, kept in Windows Credential Manager. The dialog shows it (*Show*, *Copy*, *Rotate…*) and the setup
   for each client below, with the token filled in.
2. **Edit workspace → MCP clients** for each workspace clients may use:
   - **Off** (default): its connections do not exist for clients.
   - **Read-only**: schema tools and `run_read_query`.
   - **Ask before writes**: also `run_query`; you approve every statement in Fosta Studio.

   Production workspaces (shield icon) and workspaces with Read-only rules allow at most **Read-only**, and so does a connection
   whose own rules are Read-only.

The status bar shows **MCP** with the number of tool calls while the server is on; click it to open the settings.

## Set up a client

Replace `<token>` with the token from *Settings → MCP server* (the dialog's snippets already contain it).

**Claude Code**

```sh
claude mcp add --transport http fosta http://127.0.0.1:3910/mcp --header "Authorization: Bearer <token>"
```

**Cursor** (`~/.cursor/mcp.json`), and other clients that take a URL and headers:

```json
{ "mcpServers": { "fosta": { "url": "http://127.0.0.1:3910/mcp", "headers": { "Authorization": "Bearer <token>" } } } }
```

**VS Code** (`.vscode/mcp.json`):

```json
{ "servers": { "fosta": { "type": "http", "url": "http://127.0.0.1:3910/mcp", "headers": { "Authorization": "Bearer <token>" } } } }
```

**Claude Desktop** and other clients that only start a command (stdio): use the bridge, which reads the port and the token itself.
It exits with a message when the MCP server is off, without sending the token anywhere.

```json
{ "mcpServers": { "fosta": { "command": "C:\\path\\to\\fosta-studio.exe", "args": ["mcp"] } } }
```

**Zed** (`settings.json` → `context_servers`): the same command and arguments as Claude Desktop.

Fosta Studio must be running for any of these to work; otherwise every call answers that it is not reachable.

## Tools

| Tool | What it does |
|---|---|
| `list_connections` | Connections in workspaces that allow MCP: id, name, engine, workspace, access, connected. No hosts, users or passwords. |
| `list_databases` | Databases (schemas) of a connection. |
| `list_tables` | Tables, views, collections or keys of a database with approximate row counts (key-value engines: at most 200 keys). |
| `describe_table` | Columns, keys, defaults and the definition of one table, view or collection. |
| `schema_overview` | Every table with its columns, and the foreign keys between them. |
| `run_read_query` | One read-only statement in a read-only session: 100 rows by default (`max_rows` up to 500), 30 seconds. |
| `run_query` | One statement that may change data, only in *Ask before writes* workspaces, after you approve it. |

Every tool except `list_connections` takes `connection` (an id from `list_connections`) and optionally `database` (default: the
connection's database). A connection that is not open yet is opened on demand with its saved password; without one, the client is
told to ask you to connect once. MCP clients **never start a workspace command**: if the connection needs one (a port-forward, for
example) and it is not running, the client is told to ask you to start it in Fosta Studio. A running command is reused.

`run_read_query` and `run_query` take one statement of at most 20,000 characters; longer ones are refused, so the approval dialog
always shows the whole statement that would run.

## Approving `run_query`

When a client calls `run_query`, Fosta Studio shows a dialog naming the client ("Claude Code wants to run a statement"), with the
full statement and its category (write, DDL, destructive, admin):

- *Run* runs it once. Where the workspace's rules ask for a typed confirmation (for example destructive statements under Guarded
  rules), you type the table or workspace name first.
- *Deny*, closing the dialog, or no answer within 120 seconds denies it; nothing runs.
- Statements the rules block are refused without a dialog. Every statement asks, even under Open rules that would run it directly in
  a query tab.
- After you approve, the settings are checked again: if meanwhile the MCP server was turned off, the workspace's MCP access is no
  longer *Ask before writes*, or its rules now block the statement or need a typed confirmation it did not get, nothing runs.
- Turning the server off, changing its port or rotating its token withdraws every waiting statement and ends every client session.

The name in the dialog is what the client reports about itself, so it is a label, not proof of identity: approve what the statement
does, not who sent it.

## Web edition

The web server serves the same endpoint at `/mcp` on its own port, still gated per workspace:

- Turn it on in *Settings → MCP server*. Clients authenticate with a separate **MCP token**, created when you turn the server on
  and kept in the server's encrypted secret store (`fosta-studio-secrets.enc`). The dialog shows it and rotates it, as on the
  desktop.
- The MCP token works only for `/mcp`: it cannot sign in (`/?token=`, the sign-in form) and `/api` refuses it, so a client holding
  it cannot open the app, approve its own statements or change a workspace. In turn `/mcp` refuses the access token and the session
  cookie, and an `Origin` header must name the server itself.
- With `FOSTA_STUDIO_AUTH=off` the endpoint refuses every request.
- `run_query` approvals appear in every open browser session; the first answer wins. With no browser open, it is denied after
  120 seconds.
- Stdio clients use `docker exec -i <container> /app/fosta-studio-server mcp`, which reads the MCP token from the secret store.

```sh
claude mcp add --transport http fosta https://studio.example.com/mcp --header "Authorization: Bearer <MCP token>"
```

## Security model

- **Loopback only (desktop).** The server binds `127.0.0.1`, refuses peers that are not loopback, a `Host` that is not
  `127.0.0.1` / `localhost`, and any `Origin` that is not a loopback page (DNS rebinding). Every request needs the bearer token,
  compared in constant time; rotate it in Settings, after which clients must be set up again.
- **A token for MCP only (web).** The web edition's MCP token is separate from its access token; neither works in the other's
  place.
- **Sessions.** At most 64 client sessions are kept; the least recently used goes first, and a session unused for 30 minutes
  expires. Either way the client initializes again (the stdio bridge does this by itself).
- **No secrets to clients.** Clients get connection ids, names and engines, never hosts, users, passwords or the confirmation
  token. Error messages from a database may still mention a host name.
- **The gateway decides.** Queries go through the same statement gateway as query tabs and the assistant. `run_read_query` accepts
  only statements the engine classifies as reads, in a read-only session, with the rules' own row limit and timeout when lower.
- **Untrusted data.** Table names, rows and database errors come back inside `<untrusted_data>` blocks, so a prompt hidden in a row
  is marked as data for the client's model.
- **Audit log.** Every tool call goes to `fosta-studio-audit.jsonl` with caller `mcp` and the client's name, including refusals
  (unknown tools, bad arguments, statements too long), approvals, denials, timeouts and withdrawn approvals. Turning the server on
  and allowing MCP in a workspace are recorded too.
- **Scope.** A client sees every database on the connections it may use. Allow only the workspaces you would hand to the client, and
  for production use a database user that can only read.

## Protocol

Streamable HTTP (MCP 2025-06-18; 2025-11-25, 2025-03-26 and 2024-11-05 are accepted too) at `POST /mcp`, one JSON-RPC message per
request, answered with `application/json`. `initialize` returns an `Mcp-Session-Id` that later requests send; an unknown session
answers `404` so the client initializes again (after an app restart). `DELETE /mcp` ends a session; `GET` answers `405`. Methods:
`initialize`, `ping`, `tools/list`, `tools/call`.
