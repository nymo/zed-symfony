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
2. A platform-specific binary in the extension's version directory.
3. A matching platform-specific binary on `PATH`.
4. The unsuffixed `symfony-lsp` binary on `PATH`.
5. The pinned GitHub release asset.

## The native binary starts but Zed shows no Symfony features

Phase 1 only validates server startup, initialization, shutdown, and document
synchronization. The server intentionally returns no hover, completion, or
definition results until the Phase 2 index exists.

Check the Zed log for messages containing:

```text
Symfony LSP starting
initializing Symfony LSP
Symfony LSP initialized
```

## Download failures

Automatic downloads require a published release whose tag matches the Zed
extension version and whose asset name matches the current platform. During
local development, use the `PATH` fallback instead of downloading:

```sh
cargo build -p symfony-lsp
export PATH="$PWD/target/debug:$PATH"
```

## Duplicate PHP or Twig results

The Symfony server is designed to complement existing PHP and Twig tooling.
The Phase 1 manifest attaches it to `PHP` and `Twig`, but the coexistence
behavior with Intelephense, Phpactor, and Twiggy still needs to be validated
in a Zed fixture before framework features are enabled.
