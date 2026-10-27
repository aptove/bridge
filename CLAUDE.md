# Bridge TUI Development Guidelines

## Workspace Structure and OSS / Enterprise Split

The repo is a Cargo workspace with two crates:

| Crate | Path | Purpose |
|-------|------|---------|
| `bridge-core` | `core/` | Library — all shared logic (TUI, runner, pairing, transports, push). **No enterprise-specific dependencies.** |
| `aptove-bridge` | `cli/` | OSS binary — wires up `bridge-core` with Local + Tailscale transports. |

`bridge-ent` (separate private repo) is a third binary crate that depends on `bridge-core` and additionally registers enterprise transports (e.g. Cloudflare tunnel, custom SSO proxies).

### TransportPlugin System

All transport implementations use the `TransportPlugin` trait defined in `core/src/transport.rs`:

```rust
pub trait TransportPlugin: Send + Sync + 'static {
    fn name(&self) -> &'static str;          // key in common.toml [transports]
    fn display_name(&self) -> &'static str;  // label in the TUI picker
    fn is_configured(&self, config: &CommonConfig) -> bool;
    fn build(&self, cfg, common, config_dir, advertise_addr, cwd) -> Result<TransportHandle>;
    fn wizard_fields(&self) -> Option<Vec<WizardField>> { None }   // None = self-configuring
    fn run_setup(&self, fields) -> Future<Result<TransportConfig>> // only if wizard_fields is Some
}
```

Transports are registered once at startup via `transport::init(plugins)` and accessed globally via `transport::all()` / `transport::get(name)`.

### OSS binary registration (`cli/src/main.rs`)

```rust
transport::init(plugins::oss_transports());
// registers: LocalTransportPlugin, TailscaleTransportPlugin
```

### Enterprise binary registration (bridge-ent)

```rust
transport::init(ent_transports());
// registers: LocalTransportPlugin, TailscaleTransportPlugin, + enterprise plugins (e.g. CloudflareTransportPlugin)
```

### Rules — keep bridge-core reusable

1. **No enterprise-specific code in `core/`.** Any transport-specific logic, external API calls, and custom wizard steps belong in `bridge-ent`'s `plugins.rs`, not in `core/`.
2. **No hardcoded transport names in `core/`.** The wizard, runner, and config code must work with whatever transports are registered — never branch on any specific transport name (e.g. `"cloudflare"`, `"vpn"`) inside `core/`.
3. **`wizard_fields()` / `run_setup()` drive async wizard steps.** Transports that require user input (e.g. a tunnel ID, an API key) implement these methods; the TUI calls them generically without knowing the transport type.
4. **`WizardState::compute()` must not reference specific transport names.** It may check counts (0 enabled, 2+ enabled) and call `transport::get(name).wizard_fields()` to decide whether a setup step is needed — but must never hard-code any transport name.
5. **Adding a new transport:** implement `TransportPlugin` in the appropriate binary's `plugins.rs`, add it to `init()`, and optionally implement `wizard_fields()` + `run_setup()` if it needs first-run configuration.


## Dark Background Requirement

Every popup and wizard screen **must** render a full-screen dark background (`Color::Black`) before drawing any popup widget. This is implemented in `app.rs`'s draw loop by rendering a `Block::default().style(Style::default().bg(Color::Black))` over `frame.area()` before calling `render_wizard` or `render_popup`.

Do **not** rely solely on the popup widget's own `bg(Color::Black)` style — that only covers the popup rectangle, leaving the log panel visible through the surrounding area.

## Escape Requirement

Every screen (wizard step, popup) **must** have an Escape option:

- Popups: `Esc` closes the popup and returns to the running screen.
- Wizard opened from a running bridge (`/config`, `/reconnect`): `Esc` cancels the wizard and returns to the running screen. This requires `WizardState::cancelable = true`.
- First-run wizard (no config): `Esc` on `AgentSelect` / `TransportPick` is a no-op (the user must configure before the bridge can start).
- Wizard sub-steps (`AgentCustomInput`): `Esc` goes back one step (to `AgentSelect`).
- `PushSetup`: `Esc` skips push setup and advances the wizard.
- Enterprise wizard steps (e.g. a tunnel setup screen): `Esc` goes back to `TransportPick`.

The hint text shown at the bottom of each wizard panel must include `Esc cancel` (cancelable mode) or the appropriate action label so users can discover the shortcut.

## First-Run and Missing-Config Wizard

Every time the bridge starts, `WizardState::compute()` checks `CommonConfig` for missing or incomplete configuration and runs the relevant setup screen **before** launching the bridge. The checks in order are:

1. `agent_command` missing → `AgentSelect` screen
2. 0 enabled transports → `TransportPick` screen (user must choose a transport)
3. 2+ enabled transports → `TransportPick` screen (user must choose which to run)
4. Enterprise transport enabled but missing required config → transport's own wizard step (driven by `wizard_fields()` in `bridge-ent`)
5. `push_relay` absent → push config popup shown after bridge starts

**`CommonConfig::default()` must NOT pre-enable any transport.** The transport map starts empty so that a brand-new installation always shows the transport picker. Existing configs loaded from `common.toml` are unaffected.

After any wizard completion (`finish_wizard()`), the QR pairing popup is opened automatically as soon as the bridge reports `PairingUrlReady`. This lets the user pair their mobile client immediately without typing `/qr`.

## Releasing a New Version Tag

When asked to create a new release tag (e.g. `v0.2.7`), follow these steps in order:

1. **Delete any prematurely pushed tag** (if already pushed by mistake):
   ```bash
   git push origin --delete vX.Y.Z
   git tag -d vX.Y.Z
   ```

2. **Bump versions** in both `cli/Cargo.toml` and `core/Cargo.toml`:
   - Update `version = "old"` → `version = "new"` in each file.

3. **Commit and push the version bump:**
   ```bash
   git add cli/Cargo.toml core/Cargo.toml
   git commit -m "chore: bump version to vX.Y.Z"
   git push
   ```

4. **Create and push the tag from main:**
   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

5. The GitHub Actions release workflow triggers automatically and publishes:
   - GitHub Release with shell/PowerShell installers + binary archives
   - npm packages to npmjs.com (`@aptove/bridge` and platform sub-packages)

> **Note:** The npm package version is managed automatically by the workflow — do not update `npm/bridge/package.json` manually.
