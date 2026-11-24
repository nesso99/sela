# Installation

Sela is built from source today. There are no packaged installers yet.

## Requirements

- Rust **1.85** or newer (`rustup update stable`)
- A C toolchain and `pkg-config` (GPUI and SQLite link native libraries)

### macOS

GPUI renders with Metal. Install Xcode once and the macOS platform components. If `cargo build` cannot find SQLite: `brew install sqlite pkg-config`. Passwords go to the login Keychain.

### Linux

GPUI renders with Vulkan on Wayland or X11 (a display server is required). On Debian/Ubuntu:

```bash
sudo apt install build-essential pkg-config libsqlite3-dev libssl-dev \
  libvulkan-dev libwayland-dev libxkbcommon-dev libxcb1-dev libx11-dev \
  libxrandr-dev libxi-dev libfontconfig1-dev libfreetype6-dev libdbus-1-dev \
  libasound2-dev fonts-dejavu-core
```

A Vulkan driver must be visible to the loader. Without a GPU, Mesa lavapipe can stand in:

```bash
sudo apt install mesa-vulkan-drivers
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json
```

Passwords use the Secret Service API (gnome-keyring or KWallet) over D-Bus. Without it, Sela keeps the password in memory for the session and never writes it to JSON.

### Windows

GPUI Kit targets Windows (Direct3D via wgpu); `keyring` uses Credential Manager. Treat Windows as less exercised than Linux/macOS until someone dogfoods there.

## Build and run

```bash
cargo build --workspace
cargo run -p sela-app
```

The binary package is `sela-app`; the binary name is `sela`.

## Config location

| OS | Path |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/sela/`, or `~/.config/sela/` |
| macOS | `~/Library/Application Support/Sela/` |
| Windows | `%APPDATA%\Sela\` |

Override with `SELA_CONFIG_DIR`. On Unix, config files are written with mode `0600`.

See also: [Connecting](./connecting.md), [Query history](./query-history.md), [Keyboard shortcuts](./keyboard-shortcuts.md).
