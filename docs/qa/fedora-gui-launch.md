# Launching and driving Sela on Fedora GNOME Wayland (QA)

Primary manual-QA target per `AGENTS.md`. A QA bot can start Sela and drive it
with `orca computer` without the owner opening it by hand.

## 1. Launch

```sh
scripts/qa-launch-fedora.sh          # uses target/release/sela, builds it if missing
scripts/qa-launch-fedora.sh --build  # force `cargo build --release -p sela-app`
```

It enables AT-SPI (see "Root cause"), starts Sela detached (log:
`$XDG_RUNTIME_DIR/sela-qa.log`), waits until the window is visible over AT-SPI,
and prints:

```
PID=464032
WINDOW_TITLE=Sela
APP_SELECTOR=pid:464032
```

`ORCA_CLI` (or `ORCA_CLI_COMMAND`) must point at the Orca IDE CLI, resolved as
in the `orca-computer-use` skill. There is no default: bare `orca` / `/usr/bin/orca`
on Fedora is the GNOME screen reader, and the script refuses it (and anything
resolving to it).

`--no-a11y` skips the AT-SPI step and the window check: it only confirms the
process stays alive for ~1 s and prints `PID` / `APP_SELECTOR` (no `WINDOW_TITLE`).

Set `VK_ICD_FILENAMES` before running only if the GPU driver needs it (the
owner's machine does not).

## Teardown

**After testing, QA must always run the stop script**, even if a test failed:

```sh
scripts/qa-stop-fedora.sh
```

The launcher records the PID in `$XDG_RUNTIME_DIR/sela-qa.pid` (fallback
`/tmp/sela-qa-<uid>/sela-qa.pid`) and as a `qa-launch: PID=<pid>` line in the
log. The stop script signals only those PIDs (and only while they still run a
`sela` binary), sends TERM, waits up to `STOP_WAIT_SECS` (default 5), then KILL,
and removes the PID file. It never uses `pkill`, so other Sela instances (for
example the owner's own) are left alone. It is idempotent: exit 0 and "no Sela
running" if nothing is left. Launching again overwrites the PID file, so stop
before relaunching.

## 2. Drive with `orca computer`

```sh
orca computer list-apps --json                       # expect gnome-shell AND sela
orca computer list-windows --app pid:<PID> --json    # title "Sela"
orca computer get-app-state --app pid:<PID> --no-screenshot --json
orca computer get-app-state --app pid:<PID> --query New --no-screenshot --json
orca computer click --app pid:<PID> --element-index <n> --no-screenshot --json
```

Rules that matter on this setup:

- Always use `--no-screenshot`: Wayland screenshots are unsupported on GNOME.
- **Indexes change between runs and after UI changes.** Look the control up
  with `--query` right before each click; never reuse indexes from an earlier run.
- `click --element-index` works through the AT-SPI `Action` interface, i.e. for
  controls that have an `on_click` handler (New, Run, Refresh, Cancel, tabs,
  dialog buttons...). Verified: `New` opens the connection dialog (tree gains a
  `dialog` node, focus moves to it); `Cancel` closes it.
- Controls without a click handler action (e.g. the status-bar `Dark` popup
  trigger) fail with "coordinate action target lies outside the target window".
- Window geometry is unknown on Wayland, so `--x/--y` clicks never work.
- `press-key`, `hotkey`, `type-text` (without element) are refused: "native
  Wayland windows ignore synthetic input".
- `set-value` / `type-text --element-index` return `value_not_settable` for
  Sela's text entries (GPUI inputs expose no editable-text interface).
  Typing therefore cannot be automated on Wayland; use a real X11 session (section 4).

## 3. Root cause of "Sela is not on the AT-SPI bus"

Sela already registers: GPUI (`gpui-fast-linux` / `gpui-pre-linux`) creates an
`accesskit_unix::Adapter` per window on Wayland and X11, and Sela's widgets set
roles and labels. The blocker was environmental:

- GNOME ships `org.a11y.Status` `IsEnabled = false` (and
  `gsettings org.gnome.desktop.interface toolkit-accessibility` = `false`)
  unless a screen reader or this setting is on.
- `accesskit_unix` only registers with the AT-SPI registry once `IsEnabled`
  is true, so the process ran but `list-apps` showed only `gnome-shell`.

Fix (runtime only, not persisted, reset on logout / bus restart; done by the
launch script):

```sh
busctl --user set-property org.a11y.Bus /org/a11y/bus org.a11y.Status IsEnabled b true
```

It also works for an already-running Sela (it registers within ~3 s). Setting
`gsettings set org.gnome.desktop.interface toolkit-accessibility true` makes it
permanent but changes the owner's desktop settings; do not do that unprompted.
No env vars (`GTK_A11Y`, `QT_ACCESSIBILITY`) are involved: GPUI is not GTK/Qt.

### Remaining upstream/GPUI limits

- No window position on Wayland, so coordinate clicks and screenshots are
  impossible (compositor limitation + Orca provider).
- Text inputs are not exposed as editable text; no keyboard injection on Wayland.

## 4. Optional: X11 path for typing and keys

`scripts/screenshot-smoke.sh` runs a release binary under Xvfb + lavapipe and
only captures screenshots (used by CI); it does not provide keyboard input. When
a test needs real keyboard input, log into a real X11 session and drive Sela
there. X11 is an add-on, not a replacement for the Wayland pass.
