# Symfony Zed development

Phase 1 local-development instructions.

## Prerequisites

- Rust through `rustup`.
- The `wasm32-wasip2` Rust target.
- Zed with dev-extension support.

Install the target if needed:

```sh
rustup target add wasm32-wasip2
```

## Build the native LSP

From the repository root:

```sh
cargo build -p symfony-lsp
```

Because this is a Cargo workspace, the development binary is written to:

```text
target/debug/symfony-lsp
```

The Zed launcher first looks for a platform-specific binary and then for
`symfony-lsp` on `PATH`. For local development, add the debug directory to
`PATH` before starting Zed:

```sh
export PATH="$PWD/target/debug:$PATH"
zed --foreground .
```

The `PATH` fallback avoids requiring a GitHub release while the server is
being developed.

## Install the dev extension

1. Open Zed's Extensions view.
2. Run `zed: install dev extension`.
3. Select the repository root containing `extension.toml`.
4. Rebuild the dev extension after changing the WASM launcher or manifest.

The native LSP can be rebuilt independently with `cargo build -p symfony-lsp`.

Validate the stdio protocol handshake without opening Zed:

```sh
python3 scripts/lsp_smoke.py target/debug/symfony-lsp
```

The script verifies that the server responds to `initialize` and `shutdown`.

## Optional settings

The launcher forwards both `initialization_options` and `settings` for the
`symfony-lsp` server. A minimal settings shape is:

```json
{
  "lsp": {
    "symfony-lsp": {
      "initialization_options": {},
      "settings": {}
    }
  }
}
```

No Symfony settings are consumed yet. This forwarding is part of the Phase 1
contract so later project-index and feature settings do not require a launcher
redesign.

## Native server release assets

The launcher expects a release asset for the current host using these names:

```text
symfony-lsp-macos-arm64
symfony-lsp-macos-x64
symfony-lsp-linux-arm64
symfony-lsp-linux-x64
symfony-lsp-windows-arm64.exe
symfony-lsp-windows-x64.exe
```

Non-Windows assets are packaged as `.tar.gz`; Windows assets are packaged as
`.zip`. The archive must contain the binary at its root. The extension pins
the downloaded release to its own package version.
