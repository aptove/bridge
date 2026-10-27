# Bridge TUI Development Guidelines

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
- `CloudflareSetup`: `Esc` goes back to `TransportPick`.

The hint text shown at the bottom of each wizard panel must include `Esc cancel` (cancelable mode) or the appropriate action label so users can discover the shortcut.

## First-Run and Missing-Config Wizard

Every time the bridge starts, `WizardState::compute()` checks `CommonConfig` for missing or incomplete configuration and runs the relevant setup screen **before** launching the bridge. The checks in order are:

1. `agent_command` missing → `AgentSelect` screen
2. 0 enabled transports → `TransportPick` screen (user must choose a transport)
3. 2+ enabled transports → `TransportPick` screen (user must choose which to run)
4. Cloudflare transport enabled but `tunnel_id` absent → `CloudflareSetup` form
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
