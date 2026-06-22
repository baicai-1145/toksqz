# toksqz

This package installs the prebuilt `toksqz` binary from GitHub Releases and exposes it as an npm CLI.

## Install

```bash
npm install -g toksqz
```

## Usage

```bash
toksqz --version
SQUEEZE_UPSTREAM=https://api.openai.com toksqz
```

Supported platforms:

- `linux-x64`
- `linux-arm64`
- `darwin-x64`
- `darwin-arm64`

The npm package version must match an existing GitHub Release tag and asset set.
