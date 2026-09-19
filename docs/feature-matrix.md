# Symfony Zed LSP feature matrix

Current implementation and verification status for the Symfony Zed extension.

## Support policy

The initial compatibility target is:

- **Symfony 6.4** (LTS)
- **Symfony 7.4** (LTS)
- **Symfony 8.1** (current target)

The server detects the installed version from Composer metadata: `composer.lock` first, falling back to the `composer.json` constraint for `symfony/framework-bundle` or `symfony/symfony`. Detection does not boot the application. When the version cannot be determined confidently, the server keeps indexing conservatively and leaves version-sensitive behavior disabled.

v1 has no version-gated rules yet; the detected version is recorded and logged, and future version-sensitive rules will use it. The version labels below are support targets, not a promise that every feature is identical across all three lines.

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

| Feature family | Indexed sources | Current status |
| --- | --- | --- |
| Symfony project detection | Composer metadata and framework markers | Implemented: Composer `require`/`require-dev` markers plus structural fallback; confident non-Symfony projects suppress Symfony diagnostics |
| Composer PSR-4 roots | Composer autoload metadata and PHP source | Implemented: `autoload`/`autoload-dev` `psr-4`/`psr-0` roots are scanned alongside conventional directories |
| Route attributes and names | PHP attributes, YAML route files | Common forms implemented; XML and general PHP route configuration are not supported |
| Route references and parameters | PHP and Twig call sites | Common references, navigation, completion and required-parameter diagnostics implemented |
| Twig templates | `templates/`, PHP render calls, Twig include/extends/embed | Definition/references, completion, document links and file-aware rename implemented for static names |
| Environment variables | Root `.env*`, YAML `%env(...)%`, PHP/Twig `env(...)` | Common definitions/references, completion, hover and missing-key diagnostics implemented |
| Translation keys | YAML, XLIFF, PHP/Twig call sites | YAML/XLIFF and common call sites implemented; domain-aware resolution and other catalogs are limited |
| Service definitions | YAML, XML, PHP configurator calls | Common explicit definitions, aliases, tags and references implemented; no compiled-container view |
| Symfony attributes and commands | `Route`, `Autowire`, `AsCommand` and related declarations | Common static forms indexed; arbitrary attribute/config semantics are not evaluated |
| Doctrine / Forms / Twig UX | ORM attributes/XML/YAML, FormType classes, Twig registrations/components | Static baseline implemented; runtime/database and dynamic registration are out of scope |
| Runtime container truth | `bin/console` and compiled container | Not in v1 |
| Database-backed metadata | Database schema or runtime inspection | Not in v1 |

Symfony 6.4, 7.4 and 8.1 remain compatibility targets, not fully verified support guarantees. See [`release-scope.md`](release-scope.md) for public-release gates and acceptance criteria.

## Core static baseline

The current static index supports:

- YAML route entries and PHP `Route` attributes, with route-name/path navigation, references, hover, and path-parameter completion.
- Twig template references from PHP render calls and Twig include/extends/embed syntax, including definition, references, completion, document links, and a file-aware rename edit. Template names are resolved relative to `templates/`; a short reference such as `index.html.twig` also matches a definition in a subdirectory such as `home/index.html.twig`.
- `.env`, YAML `%env(...)%`, and PHP/Twig `env(...)` references; YAML and XLIFF translation keys with filename-derived domains.
- YAML service definitions, aliases, tags, XML service/argument/tag elements, and common PHP configurator `set()`/`alias()` calls.
- Constructor argument suggestions by matching imported PHP parameter types to service classes; `#[Autowire]` service-ID completion/navigation; missing route/template/env/service/translation warnings and required route-parameter diagnostics.
- Route, template, translation, environment-variable, and service-ID rename; code actions for creating templates, adding route attributes, injecting a matching constructor service property, and adding common service tags.

These are static, syntax-shaped parsers, not a Symfony container build. PHP/XML multiline forms, arbitrary PHP config execution, every translation format, generated routes, the full framework/vendor service catalog, and advanced container features such as factories/decorators/service locators remain unsupported. PHP generic completion remains delegated to the companion PHP LSP. Custom service-tag validation and broad deprecated-pattern conversions are deferred to avoid false positives without a Symfony-version/component-specific rule catalog.

## Doctrine, Forms, and Twig ecosystem baseline

- Doctrine entities and fields from PHP ORM attributes and XML/YAML mapping files, including table/column names, relation kinds and target classes. Relation targets support definition navigation and missing-entity diagnostics. QueryBuilder field completion supports aliases declared with `from(Entity::class, 'alias')` and conventional `*Repository::createQueryBuilder('alias')` usage.
- FormType classes, builder fields and option keys; literal `data_class => Entity::class` links to the PHP class and contributes mapped Doctrine fields to `->add()` completion. Common options are suggested, with EntityType-specific options when detected. Twig `form.field` expressions resolve against indexed form fields.
- Twig extension registrations using `TwigFunction`, `TwigFilter`, `TwigTest`, their Symfony `AsTwig*` attributes, and custom tags returned from `TokenParser::getTag()`. UX components declared with `AsTwigComponent` expose public props, component-template links, and navigation from `component()` and `<twig:...>` usages.

These integrations remain static and conservative: mappings are syntax-shaped, FormType/data-class inference requires literal class constants, QueryBuilder completion needs a statically visible alias/entity association, and Twig component paths use explicit templates or the default `templates/components/<name>.html.twig` convention. Runtime container state, database schema introspection, arbitrary PHP execution, inherited/dynamic FormType options, and every Twig extension registration pattern are not inferred.

## Version-sensitive rules

These are the rules the server follows today, and the policy for any future version-gated behavior. v1 has no version-gated rules yet, so the detected version is currently informational.

- Prefer Composer package metadata over assumptions based on the project directory structure.
- Feature-detect optional Symfony components (Doctrine ORM, Form, Translation, and UX Twig components) rather than assuming they are installed.
- Keep legacy annotation parsing available where practical, especially for Symfony 6.4 projects, but prioritize PHP attributes.
- Do not emit a deprecation or compatibility diagnostic unless the relevant component version is known with sufficient confidence.
- Keep generic PHP diagnostics delegated to the companion PHP language server.
