# Installation

## Prebuilt binaries (v0.1.0+)

Download the asset for your platform from the [GitHub Releases](https://github.com/nesso99/sela/releases) page. Each release includes a `SHA256SUMS` file — verify the download before running it.

| Platform | Asset | Notes |
| --- | --- | --- |
| Linux x86_64 | `sela-*-linux-x86_64.tar.gz` or `Sela-*-x86_64.AppImage` | AppImage is self-contained enough to run without installing; still needs system GPU/Vulkan (below). |
| macOS arm64 (Apple Silicon) | `sela-*-macos-arm64.dmg` or `.zip` | **Apple Silicon (arm64) only for v0.3.1 — Intel Macs are not supported yet.** **Unsigned**. |
| Windows x86_64 | `sela-*-windows-x86_64.zip` | Contains `sela.exe`. **Unsigned** for v0.3.1. |

### Linux

1. **tarball:** extract and run `./sela`.
2. **AppImage:** `chmod +x Sela-*-x86_64.AppImage && ./Sela-*-x86_64.AppImage`.

**Runtime dependencies.** GPUI renders with Vulkan on Wayland or X11 (a display server is required). You need a working Vulkan ICD (GPU driver or Mesa). On Fedora:

```bash
sudo dnf install mesa-vulkan-drivers vulkan-loader
```

On Debian/Ubuntu:

```bash
sudo apt install mesa-vulkan-drivers libvulkan1
```

Without a GPU, Mesa lavapipe can stand in for smoke checks:

```bash
# Debian/Ubuntu path; Fedora uses a similar lavapipe ICD under /usr/share/vulkan/icd.d/
export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json
```

Also expect the usual desktop stack at runtime: Wayland or X11, fontconfig/Freetype fonts (e.g. DejaVu), and D-Bus. Passwords use the Secret Service API (gnome-keyring or KWallet) over D-Bus via keyring’s `sync-secret-service` backend (`libdbus-1-dev` at build time; already listed below). Without a Secret Service at runtime, Sela keeps the password in memory for the session, warns in the connection dialog and status bar that it will not be saved after quit, and never writes it to JSON.

Fedora note: an RPM is not published for v0.3.1. Prefer the AppImage or tarball; Flatpak may come later.

### macOS

**v0.3.1 macOS builds are Apple Silicon (arm64) only. Intel Macs are not supported yet** (no x86_64 or universal binary in this release).

Open the `.dmg` and drag **Sela** into Applications, or unzip the `.zip` and run `Sela.app`.

**Gatekeeper (unsigned builds).** v0.3.1 is not notarized or Developer ID–signed. The first launch may show *“Sela” cannot be opened because it is from an unidentified developer*. To open it anyway: Right-click (or Control-click) **Sela** → **Open** → confirm **Open**, or remove the quarantine flag after you have verified `SHA256SUMS`:

```bash
xattr -dr com.apple.quarantine /Applications/Sela.app
```

GPUI renders with Metal. Passwords go to the login Keychain.

### Windows

Unzip and run `sela.exe`.

**SmartScreen (unsigned builds).** v0.3.1 is not Authenticode-signed. SmartScreen may show *Windows protected your PC*. Choose **More info** → **Run anyway** only after verifying `SHA256SUMS`.

GPUI Kit targets Windows (Direct3D via wgpu); `keyring` uses Credential Manager. Treat Windows as less exercised than Linux/macOS until someone dogfoods there.

### Check the binary

```bash
sela --version   # prints e.g. sela 0.3.1
sela --help
```

## Build from source

### Requirements

- Rust **1.85** or newer (`rustup update stable`)
- A C toolchain and `pkg-config` (GPUI and SQLite link native libraries)

#### macOS

Install Xcode once and the macOS platform components. If `cargo build` cannot find SQLite: `brew install sqlite pkg-config`.

#### Linux

On Debian/Ubuntu (build-time packages; CI uses the same set):

```bash
sudo apt install build-essential pkg-config libsqlite3-dev libssl-dev \
  libvulkan-dev libwayland-dev libxkbcommon-dev libxcb1-dev libx11-dev \
  libxrandr-dev libxi-dev libfontconfig1-dev libfreetype6-dev libdbus-1-dev \
  libasound2-dev fonts-dejavu-core
```

On Fedora:

```bash
sudo dnf install gcc pkg-config sqlite-devel openssl-devel vulkan-loader-devel \
  wayland-devel libxkbcommon-devel libxcb-devel libX11-devel libXrandr-devel \
  libXi-devel fontconfig-devel freetype-devel dbus-devel alsa-lib-devel \
  dejavu-sans-fonts
```

You still need a Vulkan ICD at runtime (see above).

#### Windows

A recent Visual Studio Build Tools / MSVC toolchain is enough for the Rust MSVC target.

### Build and run

```bash
cargo build --workspace
cargo run -p sela-app
# release binary:
cargo build --release -p sela-app
./target/release/sela
```

The binary package is `sela-app`; the binary name is `sela`.

## Config location

| OS | Path |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/sela/`, or `~/.config/sela/` |
| macOS | `~/Library/Application Support/Sela/` |
| Windows | `%APPDATA%\Sela\` |

Override with `SELA_CONFIG_DIR`. On Unix, config files are written with mode `0600`.

Typical files in that directory: `connections.json`, `tabs.json`, `history.sqlite`, and `settings.json` (theme preference and font size — no secrets). See [Themes](./themes.md) and [Query history](./query-history.md).

See also: [Connecting](./connecting.md), [Copying](./copying.md), [Keyboard shortcuts](./keyboard-shortcuts.md).
