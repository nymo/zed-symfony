# Contributing

Thanks for helping improve Symfony for Zed. The project is a Zed extension launcher plus a native Rust LSP. Most framework behavior belongs in the LSP; the WASM extension should stay focused on launching/configuring it.

## Prerequisites

- Stable Rust via `rustup`.
- `wasm32-wasip2` for checking the Zed launcher.
- Zed with dev-extension support for editor-level testing.

```sh
rustup target add wasm32-wasip2
```

## Build and test

From the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p symfony-lsp
cargo check -p symfony-zed --target wasm32-wasip2
cargo build -p symfony-lsp
python3 scripts/lsp_smoke.py target/debug/symfony-lsp
```

The smoke test only validates the stdio `initialize`/`shutdown` handshake. `cargo test -p symfony-lsp` also runs the end-to-end LSP request/response tests in `symfony-lsp/tests/lsp_e2e.rs`, which spawn the real server against a fixture project. For changes to editor-visible behavior, install this checkout as a dev extension and test it in Zed as well. The server binary must be available on `PATH` when the Symfony project is outside this repository; see [development setup](docs/development.md).

## Implementation guidelines

- Keep the LSP static-only: do not boot the Symfony application, execute project PHP, read the compiled container, or connect to a database.
- Keep PHP/Twig/YAML/XML/Env grammars and generic PHP analysis in their existing providers. Add Symfony-specific metadata and navigation rather than duplicating generic diagnostics or type checking.
- Prefer small format-specific parsers that return indexed entities. Keep file I/O/index lifecycle separate from syntax parsing and keep LSP handlers thin.
- Preserve exact LSP ranges (UTF-16 code units), and add a regression test for each new syntax form, resolution rule, or edit.
- Parse conservatively. If a relationship cannot be resolved statically and uniquely, return no result rather than guessing.
- Update `docs/feature-matrix.md` and `docs/release-scope.md` when support or known limitations change. Do not describe a feature as verified based on parser unit tests alone; record required Zed integration checks separately.

## Pull requests

Please include:

1. A concise description and the Symfony syntax/configuration forms covered.
2. Focused tests, including ranges and ambiguous/missing-symbol cases where relevant.
3. Results for formatting, Clippy, tests, and the WASI check when applicable.
4. Documentation updates for user-visible behavior, release limits, or configuration changes.

Avoid committing generated binaries, `target/`, local index caches, or project-specific secrets. Keep unrelated workspace changes out of the change where possible.

## License

By contributing, you agree that your contributions are licensed under the project's [GNU General Public License v3.0](LICENSE).
