# Bridge Development Guidelines

## OSS / Enterprise Architecture

The repo is a Cargo workspace split into two crates:

| Crate | Path | Role |
|-------|------|------|
| `aptove-bridge-core` | `core/` | Shared library — consumed by both the OSS binary and `bridge-ent`. |
| `aptove-bridge` | `cli/` | OSS binary — depends on `aptove-bridge-core`, registers OSS-only plugins. |

`bridge-ent` (separate private repo) is a third binary crate that also depends on `aptove-bridge-core` and adds enterprise-only features on top. **`aptove-bridge-core` is the shared foundation for both.**

---

## CRITICAL: Changes to `core/` May Break `bridge-ent`

Because `bridge-ent` depends on `aptove-bridge-core` as a library, any change to `core/` is effectively a change to a public API consumed by an external crate. **Before making any of the following changes to `core/`, stop and ask the developer for approval:**

- Removing or narrowing `pub` visibility on any type, function, trait, or field
- Changing the signature of a `pub trait` or `pub struct` in a breaking way (adding a required method, removing a field, changing a type)
- Adding a new required method to a trait that `bridge-ent` likely implements (e.g. `TransportPlugin`)
- Adding a new dependency to `core/Cargo.toml` that could conflict with `bridge-ent`'s dependency tree
- Removing or restricting an extension point (a plugin registry, a trait, a callback hook) that `bridge-ent` relies on
- Moving OSS-specific logic into `core/` where it would leak into `bridge-ent`
- Hardcoding behavior in `core/` that was previously delegated to plugins or registered at runtime

**When in doubt, flag it.** Say: "This change to `core/` may affect `bridge-ent` — please confirm before I proceed."

---

## General Rules for `core/`

1. **No enterprise-specific code.** `core/` must compile cleanly with zero knowledge of what `bridge-ent` adds. Any feature that only enterprise builds need belongs in `bridge-ent`.

2. **No hardcoded feature names or assumptions about what is registered.** Logic in `core/` must work generically regardless of which plugins, hooks, or extensions are registered at runtime.

3. **Extension points must remain open.** If `core/` exposes a registry, trait, or callback, do not remove it or make it harder to implement without first confirming with the developer.

4. **Prefer default trait methods over required ones.** When adding a method to a trait in `core/`, give it a default implementation so existing implementors in `bridge-ent` are not broken.

---

## Plugin/Extension Pattern (Transport System as Example)

The transport system illustrates the general pattern used throughout `aptove-bridge-core` for extensibility:

- `core/src/transport.rs` defines a `TransportPlugin` trait and a global registry (`OnceLock`)
- `cli/src/main.rs` calls `transport::init(oss_transports())` at startup
- `bridge-ent` calls `transport::init(ent_transports())` with its own set of plugins

This same pattern should be followed for any future extension point: define the trait and registry in `core/`, register implementations in the binary (`cli/` or `bridge-ent`).

### TransportPlugin trait (reference)

```rust
pub trait TransportPlugin: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn is_configured(&self, config: &CommonConfig) -> bool;
    fn build(&self, cfg, common, config_dir, advertise_addr, cwd) -> Result<TransportHandle>;
    fn wizard_fields(&self) -> Option<Vec<WizardField>> { None }  // default = self-configuring
    fn run_setup(&self, fields) -> Future<Result<TransportConfig>> // only needed if wizard_fields is Some
}
```

---

## TUI Guidelines

### Dark Background Requirement

Every popup and wizard screen **must** render a full-screen dark background (`Color::Black`) before drawing any popup widget. This is implemented in `app.rs`'s draw loop by rendering a `Block::default().style(Style::default().bg(Color::Black))` over `frame.area()` before calling `render_wizard` or `render_popup`.

Do **not** rely solely on the popup widget's own `bg(Color::Black)` style — that only covers the popup rectangle, leaving the log panel visible through the surrounding area.

### Escape Requirement

Every screen (wizard step, popup) **must** have an Escape option:

- Popups: `Esc` closes the popup and returns to the running screen.
- Wizard opened from a running bridge (`/config`, `/reconnect`): `Esc` cancels the wizard and returns to the running screen. This requires `WizardState::cancelable = true`.
- First-run wizard (no config): `Esc` on `AgentSelect` / `TransportPick` is a no-op (the user must configure before the bridge can start).
- Wizard sub-steps (`AgentCustomInput`): `Esc` goes back one step (to `AgentSelect`).
- `PushSetup`: `Esc` skips push setup and advances the wizard.
- Enterprise wizard steps: `Esc` goes back to `TransportPick`.

The hint text shown at the bottom of each wizard panel must include `Esc cancel` (cancelable mode) or the appropriate action label so users can discover the shortcut.

### First-Run and Missing-Config Wizard

Every time the bridge starts, `WizardState::compute()` checks `CommonConfig` for missing or incomplete configuration and runs the relevant setup screen **before** launching the bridge. The checks in order are:

1. `agent_command` missing → `AgentSelect` screen
2. 0 enabled transports → `TransportPick` screen (user must choose a transport)
3. 2+ enabled transports → `TransportPick` screen (user must choose which to run)
4. Enterprise transport enabled but missing required config → transport's own wizard step (driven by `wizard_fields()` in `bridge-ent`)
5. `push_relay` absent → push config popup shown after bridge starts

**`CommonConfig::default()` must NOT pre-enable any transport.** The transport map starts empty so that a brand-new installation always shows the transport picker. Existing configs loaded from `common.toml` are unaffected.

After any wizard completion (`finish_wizard()`), the QR pairing popup is opened automatically as soon as the bridge reports `PairingUrlReady`. This lets the user pair their mobile client immediately without typing `/qr`.

---

## Testing

When adding a new feature or changing existing behaviour, you **must**:

1. **Add new tests** covering the happy path and key error cases of the changed logic.
2. **Update any existing tests** that break due to the change — do not just delete them.
3. **Do not skip tests** with `#[ignore]` without a comment explaining why.

Tests live either inline as `#[cfg(test)]` blocks in the source file, or in `core/tests/` for integration scenarios that span multiple components. Follow the style of the surrounding file:

- Pure / sync logic → `#[test]`
- Async logic → `#[tokio::test]`
- Integration tests that spawn real processes → `#[tokio::test]` in `core/tests/`

`tempfile` is already in `[dev-dependencies]` for tests that need a real filesystem path. `mockito` is available for HTTP mocking.

Run the full test suite before committing:
```bash
cargo test
```

---

## Logging

Use the correct `tracing` log level for every message:

| Level | When to use |
|-------|-------------|
| `error!` | Unrecoverable failures or silent data loss (kill failed, message dropped, critical setup failed) |
| `warn!` | Degraded behaviour the user should notice (unexpected process death, send failure, port conflict) |
| `info!` | Normal lifecycle events (bridge started, transport up, agent spawned, pairing completed) |
| `debug!` | Internal detail useful when diagnosing issues but noisy in production (replay counts, individual messages, per-send status) |

**Rules:**

1. Never use `info!` on an error path — use `warn!` or `error!`.
2. Use `error!` when data may be silently lost or a critical setup step fails.
3. Diagnostic/instrumentation logs belong at `debug!`. Do **not** tag them with `[push-dbg]` or similar markers — just use `debug!` directly.
4. TUI panel messages follow the same hierarchy: use `log_push_warn()` for WARN-level events. For ERROR-level events push a `LogRecord { level: "ERROR".to_string(), ... }` directly — do not use `log_push()` (INFO) for failure messages.

---

## Documentation Updates

When adding, changing, or removing any user-facing feature (TUI wizard steps, slash commands, transport modes, push notifications, configuration fields, CLI flags, etc.), you **must** update the following before committing:

1. **`README.md`** in this repo — keep the TUI section, slash commands table, configuration examples, and feature list accurate.
2. **`docs/bridge/`** in the docs repo — update or create the relevant page under `docs.aptove.com/bridge/`. If a page doesn't exist yet, add it and register it in `docs/docs.json` navigation.

This applies to all change types: new feature, modified behaviour, and removal. Do not leave docs describing functionality that no longer exists or missing docs for functionality that does.

---

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

6. **Update the submodule pointer in the monorepo** (`openspec-acp-swift-sdk`):
   ```bash
   cd /path/to/openspec-acp-swift-sdk
   git add bridge
   git commit -m "chore: update bridge submodule pointer to vX.Y.Z"
   git push
   ```
