# Symfony Zed compatibility

Phase 0 compatibility baseline.

## Symfony versions

The initial support range is deliberately narrow:

| Symfony line | Role | Initial policy |
| --- | --- | --- |
| 6.4 | LTS baseline | Supported target |
| 7.4 | LTS baseline | Supported target |
| 8.1 | Current target | Supported target |

The server should identify the installed version from Composer metadata, not from the PHP executable or the presence of a framework directory. A project with no reliable version information should still receive non-version-sensitive indexing, but version-specific diagnostics should be disabled.

The initial release is static-only. It does not require PHP, a bootable application, configured environment variables, a database, or a working `bin/console` command to start.

## Zed language providers

The Symfony server is an analysis layer over existing Zed languages.

| Zed language ID | Provider | What was verified | Integration requirement |
| --- | --- | --- | --- |
| `PHP` | [zed-extensions/php](https://github.com/zed-extensions/php) | The extension defines `PHP` and supports PhpTools, Intelephense, Phpactor, and PHPantom integrations | Do not duplicate generic PHP diagnostics or completion |
| `Twig` | [zed-twig](https://github.com/YussufSassi/zed-twig) | The extension defines `Twig` and registers Twiggy | Test Symfony server coexistence with Twiggy |
| `YAML` | Zed native language support | Zed's language documentation lists YAML as native | Index Symfony config without providing a grammar |
| `XML` | [zed-xml](https://github.com/sweetppro/zed-xml) | The extension defines `XML` and claims `.xml` files/XML declarations | Index Symfony config without claiming XML syntax |
| `Env` | [zed-env](https://github.com/zarifpour/zed-env) | The extension defines the `env` language and supports environment-file suffixes | Attach to `Env`; do not use `Shell Script` as the Symfony integration language |

The exact capitalization used by the installed `Env` language should be confirmed in Zed during Phase 1. The intended manifest identifier is `Env`, matching Zed's documented community language name.

## Companion language servers

Symfony-specific intelligence is complementary to generic language tooling:

- Users should be able to keep their preferred PHP language server enabled.
- The Symfony server should focus on cross-file framework relationships, not PHP type checking, formatting, or general symbol completion.
- Twiggy should remain responsible for generic Twig language behavior where it is enabled.
- YAML, XML, and Env syntax providers remain responsible for parsing and highlighting.

If Zed's multi-server behavior causes duplicate responses, the Symfony server should narrow its advertised features or make language attachment configurable rather than taking over an existing provider.

## Zed capability validation

The following capabilities are required by the roadmap but are not considered proven until exercised by a dev extension and fixture project:

| Capability | Why Symfony needs it | Phase 0 status |
| --- | --- | --- |
| Go to definition | Routes, templates, services, translations, and env keys | Protocol/design target; integration test pending |
| Find references | Cross-file framework references | Protocol/design target; integration test pending |
| Workspace symbols | Project-wide routes, services, and templates | Integration test pending |
| Multi-file `WorkspaceEdit` | Rename route/template/translation/service references | Integration test pending |
| File-creating code actions | Create missing templates or configuration | Integration test pending |
| Code lens | Optional route/service metadata | Integration test pending |
| Semantic tokens | Optional inline framework highlighting | Integration test pending |
| Completion and hover | Core first-release experience | Integration test pending |
| Watched-file notifications | Keep the static index current | Integration test pending |

These statuses are intentionally conservative: standard LSP support does not guarantee identical behavior across Zed versions or across multiple attached servers.

## Supported static inputs

The first server version may read:

- `composer.json` and `composer.lock`.
- `vendor/composer` metadata when present.
- PHP source files and attributes.
- `config/` YAML, XML, and PHP files.
- `templates/` Twig files.
- `translations/` translation files.
- `.env`, `.env.local`, `.env.test`, and related environment files recognized by the active `Env` language.
- `bin/console` as a project-detection signal only; it must not be executed in static mode.

## Explicit non-goals for the initial compatibility range

- Supporting Symfony versions older than 6.4 as a release promise.
- Reproducing PhpStorm's complete Symfony plugin surface.
- Runtime-compiled container truth.
- Database-backed Doctrine completion.
- Project code execution from the editor.
- Replacing PHP, Twig, YAML, XML, or Env syntax providers.

## References checked in Phase 0

- [Zed language support](https://zed.dev/docs/languages)
- [Zed extension development](https://zed.dev/docs/extensions/developing-extensions)
- [PHP for Zed](https://github.com/zed-extensions/php)
- [Twig for Zed](https://github.com/YussufSassi/zed-twig)
- [XML for Zed](https://github.com/sweetppro/zed-xml)
- [Environment support for Zed](https://github.com/zarifpour/zed-env)
