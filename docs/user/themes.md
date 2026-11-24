# Themes

Choose **Light**, **Dark**, or **System** from the theme control on the right side of the status bar. There is no keyboard shortcut; Cmd/Ctrl+, stays reserved for a future settings window.

The choice is stored in `settings.json` next to `connections.json` under the [config directory](./installation.md#config-location):

```json
{ "theme": "dark", "font_size": 13 }
```

Valid theme values are `light`, `dark`, and `system`. A missing or unreadable file means Dark and font size 13. The file holds no secrets (no passwords, tokens, or connection details).

**System** follows the OS appearance when the window can report it. Changing Light or Dark locks that mode and ignores later OS changes until you pick System again.

## Font size

One step for the whole window (UI and mono). The status-bar menu immediately before the theme menu lists **11** through **18**, with **Reset** back to 13. Shortcuts (global, including in text fields):

| Action | Shortcut |
| --- | --- |
| Larger | Cmd/Ctrl+= or Cmd/Ctrl++ |
| Smaller | Cmd/Ctrl+- |
| Reset to 13 | Cmd/Ctrl+0 |

Mono is one pixel smaller than the UI size, with an 11px floor. Toolbar, status bar, and tab strip heights stay fixed. Spec: [theme tokens — Font size](../design/ux/06-theme-tokens.md#font-size).
