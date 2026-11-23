# Vendored crates

## `sqlx-core` 0.8.6 + hostname-skip backport

- **Upstream:** [`sqlx-core` 0.8.6 on crates.io](https://crates.io/crates/sqlx-core/0.8.6), vendored verbatim from the published crate (license files `LICENSE-MIT` and `LICENSE-APACHE` kept).
- **Wired in by:** the `[patch.crates-io]` entry in the root `Cargo.toml`.
- **Local changes:** exactly the diff in [`sqlx-core-0.8.6-sela.patch`](sqlx-core-0.8.6-sela.patch):
  - `src/net/tls/tls_rustls.rs`: one match arm. `NoHostnameTlsVerifier` also ignores `CertificateError::NotValidForNameContext`.
  - `Cargo.toml`: allow two rustc lints that upstream 0.8.6 trips on newer compilers.
- **CI guard:** the `vendor-drift` job downloads the upstream crate, applies the patch, and fails if `vendor/sqlx-core` differs.

### Why

sqlx 0.8.6's rustls `NoHostnameTlsVerifier` only ignores
`CertificateError::NotValidForName`. From rustls 0.23.24, hostname mismatches
are reported as `NotValidForNameContext`, so `PgSslMode::VerifyCa` /
`accept_invalid_hostnames` still fails name checks.

Upstream fix: [launchbadge/sqlx#3861](https://github.com/launchbadge/sqlx/pull/3861)
(released in sqlx 0.9.0). Backported here so `verify-ca` verifies the CA chain
but not the hostname while we stay on sqlx 0.8.

### Removal

Tracked in [#19](https://github.com/nesso99/sela/issues/19). When the workspace
moves to sqlx >= 0.9, delete `vendor/sqlx-core`, the patch file, the
`[patch.crates-io]` entry in the root `Cargo.toml`, and the `vendor-drift` CI job.
