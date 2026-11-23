# ADR 0005: GPL-3.0-or-later and cargo-deny allow-list

- **Status:** Accepted
- **Date:** 2026-11-23
- **PR:** [#14](https://github.com/nesso99/sela/pull/14)

## Context

Sela is public open source. Dependency licenses must stay compatible with a copyleft project license, and CI must fail closed on incompatible or unknown licenses.

## Decision

- Project license: **GPL-3.0-or-later** (root [`LICENSE`](../../LICENSE); workspace `license = "GPL-3.0-or-later"`; crates `publish = false`).
- Enforce with [`deny.toml`](../../deny.toml) and CI `cargo deny check licenses bans sources`.
- Allow-list is minimal and matches the current graph (MIT, Apache-2.0, BSD variants, ISC, Zlib, Unicode-3.0, CC0-1.0, MPL-2.0, BSL-1.0, 0BSD, Unlicense, CDLA-Permissive-2.0 for `webpki-roots`, and related dual-license forms).
- Do **not** allow OpenSSL (old OpenSSL license is GPL-incompatible), GPL-2.0-only / LGPL-2.1-only without or-later, proprietary, or unknown.

## Consequences

- Every PR runs cargo-deny after `check`.
- New dependencies must fit the allow-list or be rejected / clarified with hashed license files.
