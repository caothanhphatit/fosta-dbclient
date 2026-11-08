# SSH tunnels and TLS for engines

The network layer lives in `src-tauri/src/net/`. It gives every engine the same SSH tunnel and the same TLS
settings, so an engine only maps them onto its driver's options.

- `net::spec`: the settings (`NetSpec`, `SshSpec`, `TlsSpec`) and the secrets (`NetSecrets`).
- `net::prepare`: opens the tunnel, if there is one, and says where the engine should connect.
- `net::tls`: builds a rustls `ClientConfig`, or the options a native-tls driver supports.
- `net::ssh`: the tunnel itself, the host key checks and the `SSH_*` error codes.

## Settings

```jsonc
{
  "ssh": {
    "enabled": true,
    "host": "bastion.example.com", "port": 22, "user": "deploy",
    "auth": { "kind": "privateKey", "keyFile": "~/.ssh/id_ed25519" },  // or {"kind":"password"} / {"kind":"agent"}
    "jumpHosts": [{ "host": "jump.example.com", "port": 22, "user": "ops", "auth": { "kind": "agent" } }],
    "hostKey": "acceptNew",          // acceptNew (default) | strict | off
    "keepaliveSecs": 30              // 0 turns keepalives off
  },
  "tls": {
    "mode": "verifyFull",            // disable | prefer | require | verifyCa | verifyFull
    "caFile": "/path/ca.pem",
    "clientCertFile": "/path/client.pem", "clientKeyFile": "/path/client-key.pem",
    "serverName": "db.internal.example.com",
    "acceptInvalidCerts": false,
    "pinnedSha256": "AB:CD:…"
  }
}
```

The config never holds a secret. The caller reads secrets from the secret store and passes them in
`NetSecrets`:

| Field | Secret slot | Used for |
|---|---|---|
| `ssh_password` | `sshPassword` | `password` auth, on the target and on the jump host |
| `ssh_key_passphrase` | `sshKeyPassphrase` | an encrypted `privateKey` |
| `tls_key_passphrase` | `tlsKeyPassphrase` | reserved; encrypted TLS client keys are rejected for now |

## Connecting

The connection manager does this for every engine:

```rust
let ctx = net::NetContext::new(data_dir.join("known_hosts"));      // built once
let prepared = net::prepare(&net_spec, &Endpoint { host, port }, &net_secrets, &ctx).await?;
// connect the engine to prepared.endpoint (127.0.0.1:<port> when tunnelled)
// keep prepared.tunnel next to the connection; dropping it closes the tunnel
```

- `target` is the database as the SSH server sees it. `127.0.0.1:3306` means the database runs on the SSH
  server itself.
- `prepared.tunnel` is a `TunnelGuard`. It holds the local listener, the forwarded sockets and the SSH
  session. Dropping it, or calling `close().await`, shuts all of them down.
- `guard.learned_host_keys()` lists the host keys accepted on first use, with their SHA-256 fingerprints.
  Show them to the user.
- When the engine's connect fails with a vague "connection closed", check `guard.last_error()`. It often
  holds the real cause, for example the SSH server refusing to forward to the database.
- If the SSH session drops, the next forwarded socket opens a new one. It retries 4 times with backoff
  from 0.5 s to 8 s. Authentication and host key errors are not retried. `guard.status()` reports
  `connected`, `reconnects`, `forwarded` and `lastError`.

## TLS in an engine

```rust
if let Some(tls) = spec.tls.as_ref().filter(|tls| tls.is_enabled()) {
    let config = net::tls::rustls_client_config(tls)?;
    let name = net::tls::server_name(tls, &prepared.tls_server_name)?;  // SNI + verification name
    // hand `config` (and `name` where the driver takes one) to the driver
}
```

`prepared.tls_server_name` is the configured database host, or `tls.serverName` when that is set. It is
never the tunnel's `127.0.0.1`, so certificate checks still work through a tunnel. The verifier already uses
`serverName` when it is set. Drivers that choose the SNI from the connect host (e.g. tokio-postgres-rustls)
still verify against the right name.

The certificate is trusted by the first rule that applies:

1. `pinnedSha256`: some certificate in the chain the server sends has this fingerprint. Chain, expiry and
   name are not checked. Pin the leaf, or a CA the server includes in its chain.
2. `acceptInvalidCerts`: any certificate.
3. Otherwise the chain must lead to `caFile` (only that file) or to the system and Mozilla roots.
   `require`, `verifyFull` and `prefer` also check the host name. `verifyCa` does not.

Handshake signatures are checked in every case. Unlike libpq, `require` verifies the certificate. Skipping
verification needs `acceptInvalidCerts`.

`prefer` means: try TLS, and fall back to plain TCP when the server has no TLS. Only engines whose protocol
can negotiate that (MySQL, PostgreSQL) should offer it.

### native-tls (tiberius on Windows)

tiberius on Windows uses SChannel through native-tls and does not accept a rustls config.
`net::tls::native_tls_options(tls)` maps the settings onto what it supports:

| `NativeTlsOptions` | tiberius |
|---|---|
| `encrypt` | `config.encryption(EncryptionLevel::Required)`, or `NotSupported` when false |
| `trust_server_certificate` | `config.trust_cert()` |
| `ca_file` | `config.trust_cert_ca(path)` |

Pinning and client certificates return a `TLS_CONFIG` error and are never silently ignored. `verifyCa` gets
the stricter full check. On Linux, tiberius uses rustls but has no hook for a custom config, so the same
mapping applies there.

## Host keys

The app's `known_hosts` file is read and written. `~/.ssh/known_hosts` is only read, so hosts already
trusted in OpenSSH connect without a prompt. Once the app file has an entry for a host, that entry decides.

| Policy | Unknown host | Host known with other key types only | Changed key |
|---|---|---|---|
| `acceptNew` | recorded and accepted, reported in `learned_host_keys()` | `SSH_HOST_KEY_UNKNOWN` | `SSH_HOST_KEY_MISMATCH` |
| `strict` | `SSH_HOST_KEY_UNKNOWN` | `SSH_HOST_KEY_UNKNOWN` | `SSH_HOST_KEY_MISMATCH` |
| `off` | accepted | accepted | accepted |

For a trust prompt, `detail.hostKey` carries `{ host, port, algorithm, fingerprintSha256, publicKey }`. After
the user confirms, call `net::ssh::trust_host_key(&ctx.known_hosts, host, port, publicKey)` and connect
again. `trust_host_key` replaces the host's existing entries in the app file. Use it after a mismatch only
when the user has confirmed the new fingerprint some other way.

## Errors

Every failure is an `AppError::structured` whose `code` says what went wrong. Most also carry
`host`/`port`/`role`, where `role` is `"SSH server"` or `"jump host"`:

| Code | Meaning |
|---|---|
| `SSH_AUTH_FAILED` | Credentials rejected. `serverMethods` lists the methods the server accepts. |
| `SSH_SECRET_REQUIRED` | A secret is missing. `slot` is `sshPassword` or `sshKeyPassphrase`. Prompt for it and retry. |
| `SSH_KEY_FILE` | The key file is missing, unreadable or malformed, or the passphrase is wrong. |
| `SSH_AGENT` | No agent is reachable, or it holds no keys. |
| `SSH_HOST_KEY_UNKNOWN` / `SSH_HOST_KEY_MISMATCH` | See above. |
| `SSH_UNREACHABLE` | TCP connect failed, the connection dropped during the handshake, or the jump host could not reach the target. |
| `SSH_TIMEOUT` | Nothing answered within `NetContext::connect_timeout` (default 15 s). |
| `SSH_FORWARD_FAILED` | The SSH server refused the forward: `AllowTcpForwarding no`, or it cannot reach the database. |
| `SSH_PROTOCOL` | Not an SSH server, or no algorithm in common. |
| `SSH_CONFIG` | Incomplete settings, more than one jump host, or a known_hosts file could not be used. |
| `TLS_CONFIG` | Unusable TLS settings or files. |

## SSH agent

- Unix: the agent named by `SSH_AUTH_SOCK`.
- Windows: the `SSH_AUTH_SOCK` pipe when it names one (`\\.\pipe\…`), else the OpenSSH agent pipe
  (`\\.\pipe\openssh-ssh-agent`, the "OpenSSH Authentication Agent" service), then Pageant.
- Every key in the agent is tried in turn. The live tests use an in-process agent. Pageant itself is not
  tested.

## Limits

- One jump host.
- Encrypted TLS client keys are not supported yet. Decrypt them with `openssl pkcs8`.
- Host certificate authorities (`@cert-authority`) are not supported. A host certificate is checked by its
  key.
- Keyboard-interactive and second-factor logins are reported as `SSH_AUTH_FAILED` with an explanation.

## Tests

- Unit tests: `cargo test net::`. TLS modes run full in-memory handshakes against certificates generated
  with rcgen. Host key policies run against temporary known_hosts files.
- Live tests (ignored by default) run through `dev/compose/ssh.yml` to the dev MariaDB. They cover password,
  OpenSSH and PEM keys, agent, jump host, host key prompts and mismatches, reconnects, and refused forwards:

  ```sh
  docker compose -f dev/docker-compose.yml up -d --wait mariadb
  docker compose -f dev/compose/ssh.yml up -d --wait
  cd src-tauri && cargo test net::live_tests -- --ignored --test-threads=1
  ```
