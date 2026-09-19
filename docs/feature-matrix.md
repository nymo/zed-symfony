# Symfony Zed LSP feature matrix

Phase 0 baseline for the Symfony Zed extension.

## Support policy

The initial compatibility target is:

- **Symfony 6.4** (LTS)
- **Symfony 7.4** (LTS)
- **Symfony 8.1** (current target)

The server should detect the project version from Composer metadata, preferably `composer.lock`, and select framework-specific behavior from the installed component versions. The version labels below are support targets, not a promise that every feature is identical across all three lines.

Version detection must not require booting the application. If the installed version cannot be determined confidently, the server should prefer conservative indexing and suppress version-sensitive diagnostics.

## Language and extension ownership

The Symfony extension does not provide grammars or syntax highlighting. It attaches a Symfony-aware language server to existing Zed languages.

| Concern | Existing provider | Zed language ID | Symfony extension responsibility |
| --- | --- | --- | --- |
| PHP syntax and generic PHP tooling | [zed-extensions/php](https://github.com/zed-extensions/php) | `PHP` | Symfony metadata only; complement Intelephense, Phpactor, or another PHP server |
| Twig syntax and generic Twig tooling | [zed-twig](https://github.com/YussufSassi/zed-twig) | `Twig` | Symfony-aware template, route, translation, and component metadata; test coexistence with Twiggy |
| YAML syntax | Zed native support | `YAML` | Symfony configuration and service metadata |
| XML syntax | [zed-xml](https://github.com/sweetppro/zed-xml) | `XML` | Symfony service and configuration metadata |
| Environment-file syntax | [zed-env](https://github.com/zarifpour/zed-env) | `Env` | Symfony environment-variable indexing and references |

`Env` is intentional. The initial design must not attach the Symfony server to `Shell Script` as a workaround for `.env` files. The community `zed-env` extension defines an `Env` language and handles environment-file syntax.

## Feature targets

| Feature family | Main indexed sources | 6.4 | 7.4 | 8.1 | Initial status |
| --- | --- | --- | --- | --- | --- |
| Symfony project detection | `composer.json`, `composer.lock`, `vendor/composer` | Target | Target | Target | Phase 2 |
| Composer PSR-4 and PHP symbol index | PHP source and Composer metadata | Target | Target | Target | Phase 2 |
| Route attributes and route names | PHP attributes, YAML, XML, PHP route configuration | Target | Target | Target | Phase 3 |
| Route references and parameters | PHP and Twig call sites | Target | Target | Target | Phase 3 |
| Twig template references | `templates/`, PHP render calls, Twig tags | Target | Target | Target | Phase 3 |
| Environment variables | `.env*`, `%env(...)%`, `env(...)` | Target | Target | Target | Phase 3 |
| Translation keys | YAML, XLIFF, PHP translation files and call sites | Target | Target | Target | Phase 3 |
| Explicit service definitions | YAML, XML, PHP configuration | Target | Target | Target | Phase 4 |
| Service aliases, bindings, and tags | Symfony service configuration and attributes | Target | Target | Target | Phase 4 |
| Symfony attributes | `Route`, `Autowire`, `AsCommand`, event/message attributes | Target | Target | Target | Phase 4 |
| Console command discovery | `AsCommand`, `Command` subclasses, command metadata | Target | Target | Target | Phase 4 |
| High-confidence diagnostics | Missing routes/templates/env keys and explicit services | Target | Target | Target | Phase 5 |
| Doctrine metadata | Attributes, XML/YAML mappings, entity source | Target | Target | Target | Phase 6 |
| Symfony Forms metadata | Form types, options, `data_class`, entity fields | Target | Target | Target | Phase 6 |
| Twig extensions and Symfony UX | Extension classes, components, templates | Target | Target | Target | Phase 6 |
| Runtime container truth | `bin/console` and compiled container | Not in v1 | Not in v1 | Not in v1 | Future opt-in |
| Database-backed metadata | Database schema or runtime inspection | Not in v1 | Not in v1 | Not in v1 | Future opt-in |

`Target` means the feature should be implemented against that Symfony line and covered by fixtures. It does not mean the feature has been validated yet.

## Version-sensitive rules

- Prefer Composer package metadata over assumptions based on the project directory structure.
- Feature-detect optional Symfony components such as Translation, Twig, Messenger, Doctrine, and Form rather than assuming they are installed.
- Keep legacy annotation parsing available where practical, especially for Symfony 6.4 projects, but prioritize PHP attributes.
- Do not emit a deprecation or compatibility diagnostic unless the relevant component version is known with sufficient confidence.
- Keep generic PHP diagnostics delegated to the companion PHP language server.

## Phase 0 validation still required

The following capabilities need an end-to-end test in a Zed fixture before Phase 1 is considered complete:

- Multiple language servers attached to PHP at the same time.
- Symfony framework intelligence attached to Twig while Twiggy is installed.
- Multi-file `WorkspaceEdit`.
- File-creating code actions.
- Code lens.
- Semantic tokens.
- Workspace symbols.

These are standard LSP-shaped requirements, but the extension should verify the behavior against the Zed version used for development rather than relying only on protocol support.
