# Themes

Choose **Light**, **Dark**, or **System** from the theme control on the right side of the status bar. There is no keyboard shortcut; Cmd/Ctrl+, stays reserved for a future settings window.

The choice is stored in `settings.json` next to `connections.json` under the [config directory](./installation.md#config-location):

```json
{ "theme": "dark" }
```

Valid values are `light`, `dark`, and `system`. A missing or unreadable file means Dark. The file holds no secrets (no passwords, tokens, or connection details).

**System** follows the OS appearance when the window can report it. Changing Light or Dark locks that mode and ignores later OS changes until you pick System again.
