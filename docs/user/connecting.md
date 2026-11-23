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

If no secret service is available (common on headless Linux), Sela keeps the password in process memory for the session, shows a status-bar note, and asks again on the next launch. It still does not write the password into JSON.

## TLS (PostgreSQL and MySQL)

SQLite has no TLS options (hidden in the form).

For Postgres and MySQL, set **SSL mode** and optional certificate paths:

| Mode | Meaning |
| --- | --- |
| `disable` | Plain TCP only (default for new connections and for docker-compose) |
| `prefer` | Try TLS first; fall back to plain if the server refuses |
| `require` | Require TLS; do not verify the server certificate |
| `verify-ca` | Require TLS and verify the server certificate against a CA (hostname not checked) |
| `verify-full` | Like verify-ca, and check the server hostname |

Optional paths (plain text in JSON):

- **CA certificate** — required for `verify-ca` and `verify-full`
- **Client certificate** and **client key** — both set or both empty; the key must be **unencrypted PEM** for now (passphrase-protected keys are not supported yet)

Handshake and certificate failures surface as a TLS error, distinct from a wrong password.

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

Leave SSL mode on **disable** for these containers; they do not serve TLS.
