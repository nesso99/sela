# Themes

Choose **Light**, **Dark**, or **System** from the theme control on the right side of the status bar. There is no keyboard shortcut; Cmd/Ctrl+, stays reserved for a future settings window.

The choice is stored in `settings.json` next to `connections.json` under the [config directory](./installation.md#config-location):

```json
{ "theme": "dark" }
```

Valid values are `light`, `dark`, and `system`. A missing or unreadable file means Dark. The file holds no secrets (no passwords, tokens, or connection details).

**System** follows the OS appearance when the window can report it. Changing Light or Dark locks that mode and ignores later OS changes until you pick System again.

## Font size

Not in this build. The UI font stays 13px and the grid and SQL editor stay 12px. A later control is specified in [the theme tokens](../design/ux/06-theme-tokens.md#font-size): one step from 11 to 18, stored as `font_size` in this same file, with shortcuts Cmd/Ctrl+Plus, Cmd/Ctrl+Minus, and Cmd/Ctrl+0. `settings.json` today does not need a `font_size` key.
