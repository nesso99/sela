# Vendored crates

## `sqlx-core` (0.8.6 + hostname-skip fix)

sqlx 0.8.6’s rustls `NoHostnameTlsVerifier` only ignores
`CertificateError::NotValidForName`. From rustls 0.23.24, hostname mismatches
are reported as `NotValidForNameContext`, so `PgSslMode::VerifyCa` /
`accept_invalid_hostnames` still fails name checks.

Upstream fix: [launchbadge/sqlx#3861](https://github.com/launchbadge/sqlx/pull/3861)
(in sqlx 0.9.0). Backported here so verify-ca verifies the CA chain but not the
hostname while we stay on sqlx 0.8.

When the workspace moves to sqlx ≥ 0.9, delete this vendor crate and the
`[patch.crates-io]` entry in the root `Cargo.toml`.
