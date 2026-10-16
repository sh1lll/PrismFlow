# Releasing PrismFlow

Status: **the pipeline is built; the owner's signing identities are required to run it** (NEEDS_OWNER).

## What a release contains

- The app, built with `--features updater` (signed update checks enabled).
- The pinned gateway engine (`scripts/fetch-gateway.sh`: npm integrity hash + version + MIT license checks).
- A pinned Node.js runtime (`scripts/fetch-node.sh`: official nodejs.org build, SHA-256 verified against `SHASUMS256.txt`). Users no longer need Node installed.
- `PRISMFLOW_RELEASE=1` is compiled in, so a missing engine is reported as a broken install, not as a dev-setup step.

## One-time owner setup

| Secret | How to create | Used for |
|---|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` / `_PASSWORD` | `npx tauri signer generate -w ~/.tauri/prismflow.key` — keep the private key offline | Signing update bundles |
| `PRISMFLOW_UPDATER_PUBKEY` | The `.pub` printed by the command above | Embedded in the app; verifies every update |
| `PRISMFLOW_UPDATE_ENDPOINT` | An https URL you control serving `latest.json` (e.g. GitHub Releases `.../releases/latest/download/latest.json`) | Update checks |
| `APPLE_SIGNING_IDENTITY` | "Developer ID Application: …" certificate in your keychain (Apple Developer Program) | macOS code signing |
| `APPLE_ID`, `APPLE_PASSWORD` (app-specific), `APPLE_TEAM_ID` | appleid.apple.com → app-specific password | Notarization |
| Windows certificate | Authenticode certificate; set `bundle.windows.certificateThumbprint` or use Azure Trusted Signing | Windows signing |

Linux packages (`.deb`, `.rpm`, AppImage) are signed by the updater key; distro repo signing is out of scope until there is a repo.

## Build

```bash
scripts/release.sh
```

The script refuses to run if any required secret is missing; there are no unsigned release builds. CI runs the same script per platform in `.github/workflows/release.yml` when a `v*` tag is pushed, reading the secrets from the repository's Actions secrets.

## Verified locally (2026-10-14)

- `cargo check --features updater` compiles; dev builds report "no updater" honestly.
- An unsigned local `.app` (47.6 MB) was built with `tauri.bundle.conf.json`: the engine and `runtime/node` land under `Contents/Resources` where `use_bundled_runtime` looks, and the bundled `prismflow --version` runs.
- Using Homebrew's `node` as a stand-in fails inside the bundle (it links Homebrew's `libnode.dylib`). That is why releases use the official standalone build from `fetch-node.sh`, and why the app now adopts a bundled Node only after it runs `--version` successfully.
