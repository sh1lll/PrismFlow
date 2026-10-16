<p align="center"><img src="branding/prismflow-logo-800.png" alt="PrismFlow" width="560"></p>

# PrismFlow

PrismFlow connects the AI coding apps you already use (Claude Code, Codex,
Claude Desktop, VS Code, JetBrains IDEs, Zed and many terminal agents) to
any model provider, through a private gateway on your own computer.

Your provider keys stay in your system's secure store. Each app gets its
own local key that only works on your computer and stops working when you
disconnect it.

This README is a placeholder while the project page is written.

## Build from source

```sh
npm ci
scripts/fetch-gateway.sh
npx tauri build
```

Releases: see [docs/RELEASE.md](docs/RELEASE.md).
