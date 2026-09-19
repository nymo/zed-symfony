# Symfony Zed compatibility

Compatibility and integration status.

## Symfony versions

The initial support range is deliberately narrow:

| Symfony line | Role | Initial policy |
| --- | --- | --- |
| 6.4 | LTS baseline | Supported target |
| 7.4 | LTS baseline | Supported target |
| 8.1 | Current target | Supported target |

The server identifies the installed version from Composer metadata (`composer.lock`, falling back to the `composer.json` constraint), not from the PHP executable or the presence of a framework directory. A project with no reliable version information still receives non-version-sensitive indexing; version-specific behavior stays disabled. v1 has no version-gated rules, so the detected version is currently informational and is logged at startup.

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

The manifest attaches the server to `Env`, matching the documented community language identifier. Verify `.env` and `.env.*` attachment in Zed before release.

## Companion language servers

Symfony-specific intelligence is complementary to generic language tooling:

- Users should be able to keep their preferred PHP language server enabled.
- The Symfony server should focus on cross-file framework relationships, not PHP type checking, formatting, or general symbol completion.
- Twiggy should remain responsible for generic Twig language behavior where it is enabled.
- YAML, XML, and Env syntax providers remain responsible for parsing and highlighting.

### PHP rename provider priority

Zed routes Rename Symbol to a single server, not to all attached servers. For
`textDocument/prepareRename` and `textDocument/rename`, Zed queries servers in
the order configured by `languages.<LANG>.language_servers` and uses the first
one that advertises `renameProvider`; if that server declines the position, Zed
does **not** ask the next server. (See Zed's `LanguageServerToQuery::FirstCapable`
in `crates/project/src/lsp_store.rs`.) Hover, definition, references,
completion, diagnostics and code actions behave differently: Zed merges those
across every attached server, so keeping a companion PHP server first does not
hide Symfony's PHP features.

The two rename providers handle disjoint positions:

- A generic PHP server renames PHP symbols (classes, methods, variables).
- Symfony renames framework strings (route names, template paths, service IDs,
  translation keys, environment variables).

**Chosen policy: keep the companion PHP server first for PHP (the default).**
This preserves ordinary PHP symbol rename, which is the more common operation.
Symfony still contributes hover, definition, references, completion, diagnostics
and code actions to PHP. The only loss is renaming a framework string *from a PHP
file*; framework-string rename still works in Twig, YAML, XML and Env, where
Symfony is the only rename provider.

**Opt-in for PHP framework-string rename.** To rename route names, template paths
or service IDs from PHP, make Symfony the rename provider for PHP:

```json
{
  "languages": {
    "PHP": {
      "language_servers": ["symfony-lsp", "..."]
    }
  }
}
```

The `"..."` entry keeps other registered PHP servers attached. Because Symfony
only handles framework strings and returns no result for ordinary PHP symbols,
ordinary PHP symbol rename then stops working; verify your workflow with your
chosen companion server. This is a Zed routing limitation, not a Symfony parser
limitation. A transparent fix requires Zed to fan out rename requests or fall
back when the first capable server declines a position.

## Zed capability validation

The following capabilities are required by the roadmap but are not considered proven until exercised by a dev extension and fixture project:

| Capability | Why Symfony needs it | Current status |
| --- | --- | --- |
| Go to definition | Routes, templates, services, translations, env, Doctrine, Forms and Twig metadata | Provider implemented and covered by automated protocol tests; Zed integration check pending |
| Find references | Cross-file framework references | Provider implemented and covered by automated protocol tests; Zed integration check pending |
| Workspace symbols | Project-wide routes, services, and templates | Not implemented; not in v1 scope |
| Multi-file `WorkspaceEdit` | Rename template files and update references | Edit implemented for Twig templates; covered by automated protocol tests; Zed resource-operation check pending |
| File-creating code actions | Create missing templates or configuration | Template/action builders implemented and covered by automated protocol tests; Zed integration check pending |
| Code lens | Optional route/service metadata | Not implemented; not in v1 scope |
| Semantic tokens | Optional inline framework highlighting | Not implemented; not in v1 scope |
| Completion and hover | Framework metadata at source references | Providers implemented and covered by automated protocol tests; Zed interaction check pending |
| Watched-file notifications | Keep the static index current | Dynamic registration and index updates covered by automated protocol tests; Zed delivery check pending |

These statuses are intentionally conservative: native unit tests and protocol-level tests do not guarantee identical behavior across Zed versions or across multiple attached servers. See [`release-scope.md`](release-scope.md) for blockers.

## Supported static inputs

The first server version may read:

- `composer.json` and `composer.lock`.
- `composer.json` for project detection, optional-component detection, and `autoload`/`autoload-dev` `psr-4`/`psr-0` source roots.
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

## References

- [Zed language support](https://zed.dev/docs/languages)
- [Zed extension development](https://zed.dev/docs/extensions/developing-extensions)
- [PHP for Zed](https://github.com/zed-extensions/php)
- [Twig for Zed](https://github.com/YussufSassi/zed-twig)
- [XML for Zed](https://github.com/sweetppro/zed-xml)
- [Environment support for Zed](https://github.com/zarifpour/zed-env)
