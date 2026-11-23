# ADR 0006: No config or data migrations before the first release

- **Status:** Accepted
- **Date:** 2026-11-23

## Context

There are no stable users yet. Config shapes (`connections.json`, future `tabs.json` / history) will change through M1 (for example TLS and `schema_version` 2 in M1-8). Shipping migration code for pre-release formats adds cost without benefit.

## Decision

Until the first release:

- Prefer **`schema_version` bumps only**.
- **No migration code** for old config or data shapes — drop incompatible shapes and require a fresh file when needed.
- `tabs.json` stays separate from `connections.json` and does not require a connections schema bump for tab persistence.

## Consequences

- M1-8 can introduce TLS fields and `schema_version: 2` without a migrator.
- Contributors must not assume on-disk formats are stable before v1.0.
- After the first release, introduce migrations when breaking changes ship.
