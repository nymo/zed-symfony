# Symfony Zed development

Local development instructions.

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

The Zed launcher automatically checks the workspace's `target/debug` and
`target/release` directories for the locally built `symfony-lsp` binary. It
then checks `PATH` before attempting a GitHub release download. You can also
make the local binary available explicitly:

```sh
export PATH="$PWD/target/debug:$PATH"
zed --foreground .
```

This avoids requiring a GitHub release while the extension is being developed.

### Using the server from another project

The extension launcher checks the current worktree first. Therefore,
`target/debug/symfony-lsp` is found automatically only when Zed is opened on
this repository. A Symfony application such as `demo-application` has no such
Rust target directory, so the server must also be available on `PATH` (or as a
published release asset).

Build the server, expose it through a stable PATH directory, and restart Zed:

```sh
cargo build -p symfony-lsp
mkdir -p "$HOME/.local/bin"
ln -sf "$PWD/target/debug/symfony-lsp" "$HOME/.local/bin/symfony-lsp"
export PATH="$HOME/.local/bin:$PATH"
zed --foreground /path/to/demo-application
```

When starting Zed from the GUI, make sure the same directory is present in the
GUI process environment, or start Zed once from a shell with the updated PATH.
If the extension is installed from a release, the matching platform binary is
downloaded automatically instead and this local PATH setup is not needed.

## Install the dev extension

1. Open Zed's Extensions view.
2. Run `zed: install dev extension`.
3. Select the repository root containing `extension.toml`.
4. Rebuild the dev extension after changing the WASM launcher or manifest.

The native LSP can be rebuilt independently with `cargo build -p symfony-lsp`.

Run the full local check suite:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p symfony-lsp
python3 scripts/lsp_smoke.py target/debug/symfony-lsp
```

The smoke script verifies that the server responds to `initialize` and `shutdown`; it does not exercise editor-visible features. Pass a workspace path as a second argument to check detection and indexing against a real project:

```sh
python3 scripts/lsp_smoke.py target/debug/symfony-lsp /path/to/symfony-project
```


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

The launcher forwards settings for future user-configurable indexing and analysis options; adding settings does not require a launcher redesign.

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

### Publishing a release

`.github/workflows/release.yml` builds and uploads all six assets. The release
tag must equal the extension version exactly (no `v` prefix), because the
launcher downloads from
`releases/download/<version>/symfony-lsp-<platform>.<ext>`:

```sh
# Cargo.toml, extension.toml and the tag must all agree.
git tag 0.1.0
git push origin 0.1.0
```

The workflow:

1. Runs formatting, Clippy, the native tests and the WASI check, and verifies the
tag matches the `symfony-zed` package version.
2. Builds `symfony-lsp` in release mode for each platform, renames the binary to
   the platform-specific asset name, and packages it so the binary sits at the
   archive root.
3. Attaches the assets to the GitHub release for the tag.

macOS builds both architectures on the arm64 runner. Linux and Windows use
native x64 and arm64 runners (`ubuntu-24.04-arm`, `windows-11-arm`); update
those labels if GitHub changes runner availability. `workflow_dispatch` builds
the assets without publishing a release, which is useful for testing.
