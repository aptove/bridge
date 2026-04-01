# Installing the Bridge

The Bridge is distributed as a single standalone executable for multiple platforms (macOS, Linux, and Windows). You can install it using npm, our automated installation scripts, or download the binaries manually.

## npm (Recommended)

If you have Node.js installed, this is the simplest way:

```bash
npm install -g @aptove/bridge
```

This installs the correct platform-specific binary automatically.

## Shell Installer

### macOS & Linux

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/aptove/bridge/releases/latest/download/bridge-installer.sh | sh
```

### Windows

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/aptove/bridge/releases/latest/download/bridge-installer.ps1 | iex"
```

## Manual Installation

Download the appropriate executable for your platform from the [GitHub Releases](https://github.com/aptove/bridge/releases) page.

1. Go to the [latest Release](https://github.com/aptove/bridge/releases/latest).
2. Download the archive for your architecture:
   - macOS (Apple Silicon): `bridge-aarch64-apple-darwin.tar.xz`
   - macOS (Intel): `bridge-x86_64-apple-darwin.tar.xz`
   - Linux (x86_64): `bridge-x86_64-unknown-linux-gnu.tar.xz`
   - Linux (ARM64): `bridge-aarch64-unknown-linux-gnu.tar.xz`
   - Windows: `bridge-x86_64-pc-windows-msvc.zip`
3. Extract the archive and place the `bridge` binary in a directory in your `PATH`.

---

## Technical Details: Automated Release Generation

The Bridge project uses GitHub Actions and `cargo-dist` for automated CI/CD release builds.

New binaries and installers are built and published **automatically whenever a new Git tag matching a version number (like `v0.1.0`, `v1.0.0`) is pushed to the repository.** A release can also be triggered manually via `workflow_dispatch`.

### The Release Flow

When a version tag is pushed (or a manual dispatch is triggered):
1. The GitHub Actions **Release** workflow is triggered.
2. It builds the `bridge` executable for all supported platforms (macOS, Linux, Windows) using GitHub runners.
3. It generates the shell and PowerShell installation scripts (`bridge-installer.sh`, `bridge-installer.ps1`).
4. It packages the binaries and checksums into archives.
5. A new GitHub Release is created with all artifacts attached.
6. The library is published to [crates.io](https://crates.io/crates/aptove-bridge).
7. Platform-specific npm packages (`@aptove/bridge-*`) and the main `@aptove/bridge` package are published to [npmjs.com](https://www.npmjs.com/package/@aptove/bridge).

To trigger a new release as a developer, commit your changes, update the version in `Cargo.toml`, and run:
```bash
git tag vX.Y.Z
git push --tags
```
