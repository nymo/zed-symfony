# Symfony Zed troubleshooting

## The server does not start

1. Open Zed's log with `zed: open log`.
2. Run Zed in the foreground:

   ```sh
   zed --foreground .
   ```

3. Confirm that the native server is available:

   ```sh
   cargo build -p symfony-lsp
   export PATH="$PWD/target/debug:$PATH"
   command -v symfony-lsp
   ```

The extension's lookup order is:

1. A cached binary from the current extension version.
2. `target/debug/symfony-lsp` or `target/release/symfony-lsp` in the workspace.
3. A platform-specific binary in the extension's version directory.
4. A matching platform-specific binary on `PATH`.
5. The unsuffixed `symfony-lsp` binary on `PATH`.
6. The pinned GitHub release asset.

## The native binary starts but Zed shows no Symfony features

The current server builds a static index when initialized and updates indexed files as buffers change. It does not require Symfony to boot. Check the Zed log for messages containing:

```text
Symfony LSP starting
initializing Symfony LSP
Symfony LSP initialized
```

## Download failures

Automatic downloads require a published release whose tag matches the Zed
extension version and whose asset name matches the current platform. During
local development, build the server in the workspace first; the launcher will
find it automatically. The `PATH` fallback remains available:

```sh
cargo build -p symfony-lsp
export PATH="$PWD/target/debug:$PATH"
```

## Duplicate PHP or Twig results

The Symfony server is designed to complement existing PHP and Twig tooling. Zed merges hover, definition, references, completion, diagnostics and code actions across servers, but routes Rename Symbol to a single server and does not fall back. The default policy keeps your PHP server first, so ordinary PHP symbol rename works and framework-string rename in PHP is unavailable; see [`compatibility.md`](compatibility.md#php-rename-provider-priority) for the Symfony-first opt-in. Twiggy coexistence and duplicate framework results still need a Zed fixture test.
