<p align="center"><img src="branding/prismflow-logo-800.png" alt="PrismFlow" width="560"></p>

# PrismFlow

One app in, any model out. PrismFlow connects Claude Code, Codex, Claude
Desktop, VS Code, Zed and 15+ agents to 100+ providers or your own local
models, privately on your computer, with no keys handed out: a local gateway
gives each app its own key, with usage tracking and one-click setup.

Your provider keys stay in your system's secure store. Each app gets its own
local key that only works on your computer and stops working when you
disconnect it.

This README is a placeholder while the project page is written.

## Build from source

```sh
npm ci
scripts/fetch-gateway.sh
npx tauri build
```

Releases: see [docs/RELEASE.md](docs/RELEASE.md).

## License

[Apache License 2.0](LICENSE).
