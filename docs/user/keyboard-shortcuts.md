# Keyboard shortcuts

Bindings are registered in `crates/sela-app/src/ui/mod.rs`. On macOS use **⌘**; on Linux/Windows use **Ctrl** for the same actions (both are bound).

| Action | Shortcut |
| --- | --- |
| Run query | Cmd/Ctrl+Enter |
| Cancel query | Cmd/Ctrl+. |
| New query tab | Cmd/Ctrl+T |
| Close tab | Cmd/Ctrl+W |
| Switch to tab 1–9 | Cmd/Ctrl+1 … Cmd/Ctrl+9 |
| Show table Data | Cmd/Ctrl+Shift+[ |
| Show table Structure | Cmd/Ctrl+Shift+] |
| Quit | Cmd+Q |

Run applies to the active query tab. Cancel stops the running query for that tab (server-side cancel on Postgres/MySQL; SQLite drops the stream). Closing a tab cancels its in-flight work.

The chords above are the ones registered in `ui/mod.rs`. Query history, the connection dialog, and the theme toggle are specified and not bound yet. The design list is [docs/design/ux/README-batch2.md](../design/ux/README-batch2.md). `secondary` in that folder means ⌘ on macOS and Ctrl on Linux and Windows.
