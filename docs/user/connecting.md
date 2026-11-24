# Connecting

Sela supports **PostgreSQL**, **MySQL**, and **SQLite**.

## Saved connections

Create, edit, and delete connections from the sidebar. Fields (host, port, user, database, SQLite path, TLS paths, and so on) are stored in `connections.json` under the [config directory](./installation.md#config-location) (`schema_version` 2).

**Passwords are never written to that file.** When an OS secret service is available, Sela stores them with the [`keyring`](https://crates.io/crates/keyring) crate under service `app.sela.database`:

| OS | Backend |
| --- | --- |
| macOS | Login Keychain |
| Linux | Secret Service (gnome-keyring, KWallet, …) over D-Bus |
| Windows | Credential Manager |

If no secret service is available (common on headless Linux or minimal desktop sessions), Sela keeps the password in process memory for the session only. The connection dialog shows a clear warning that the password will not be saved after you quit, and the status bar shows: *The OS keychain is unavailable, so the password is kept for this session only.* Connect, reconnect, and switch still work until you quit. Sela asks for the password again on the next launch. It still does not write the password into JSON.

## Switching and disconnecting

Click another saved connection in the sidebar to switch. Sela cancels queries on the previous connection, closes that connection’s tabs, then connects the one you chose. **Edit** opens the connection you selected in the tree, not only the one that is currently connected.

**Disconnect** cancels running queries for the active connection, closes its tabs, and drops the session. Tabs are not left open for a disconnected connection.

## TLS (PostgreSQL and MySQL)

SQLite has no TLS options (hidden in the form).

For Postgres and MySQL, set **SSL mode** and optional certificate paths:

| Mode | Meaning |
| --- | --- |
| `disable` | Plain TCP only (how saved files with no `tls` object still load) |
| `prefer` | Try TLS first; fall back to plain if the server refuses (default for new Postgres/MySQL connections) |
| `require` | Require TLS; do not verify the server certificate |
| `verify-ca` | Require TLS and verify the server certificate against a CA (hostname not checked) |
| `verify-full` | Like verify-ca, and check the server hostname |

When Prefer is selected, the dialog shows: *prefer may fall back to unencrypted; use require or verify-full for remote servers*.

Optional paths (plain text in JSON):

- **CA certificate** — required for `verify-ca` and `verify-full`
- **Client certificate** and **client key** — both set or both empty; the key must be **unencrypted PEM** for now (passphrase-protected keys are not supported yet)

Handshake and certificate failures surface as a TLS error, distinct from a wrong password.

## Query history

**Record query history** (default on) stores statements you run from the SQL editor in `history.sqlite` next to `connections.json`. Turn it off for a connection when you do not want those runs recorded. See [Query history](./query-history.md).

## Sample library

**Sample library** in the sidebar creates `sample.sqlite` next to the connections file and opens it (`authors`, `books`, `bookmarks`, and a `books_in_print` view).

## Local Postgres and MySQL

```bash
docker compose up -d
```

| Engine | Host | Port | User | Password | Database |
| --- | --- | --- | --- | --- | --- |
| PostgreSQL | `127.0.0.1` | `5432` | `sela` | `sela` | `sample` |
| MySQL | `127.0.0.1` | `3306` | `sela` | `sela` | `sample` |

The default **prefer** mode still works with these containers (they do not serve TLS, so the driver falls back to plain TCP). Use **disable** if you want to skip the TLS attempt.
