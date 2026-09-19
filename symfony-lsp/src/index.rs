use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use tower_lsp::lsp_types::{Position, Range, Url};

mod doctrine;
mod forms_twig;
mod parser;
mod project;

pub use project::{ComposerProject, ProjectKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Confidence {
    Exact,
    Likely,
    Unknown,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum EntityKind {
    Class,
    Interface,
    Method,
    Property,
    Route,
    Service,
    ServiceAlias,
    ServiceTag,
    Parameter,
    EnvironmentVariable,
    TwigTemplate,
    TranslationKey,
    ConsoleCommand,
    DoctrineEntity,
    DoctrineField,
    FormType,
    FormField,
    FormOption,
    TwigExtension,
    TwigFunction,
    TwigFilter,
    TwigTest,
    TwigTag,
    TwigComponent,
    TwigComponentProp,
}

#[derive(Clone, Debug)]
pub struct Entity {
    pub kind: EntityKind,
    pub name: String,
    pub qualified_name: Option<String>,
    pub uri: Url,
    pub range: Range,
    pub references: Vec<Range>,
    pub metadata: HashMap<String, String>,
    pub confidence: Confidence,
}

#[derive(Clone, Debug, Default)]
pub struct ProjectIndex {
    pub root: PathBuf,
    pub entities: Vec<Entity>,
    /// Composer-derived detection and source-root metadata for the workspace.
    pub project: ComposerProject,
    files: HashMap<PathBuf, Vec<Entity>>,
    fingerprints: HashMap<PathBuf, u64>,
}

impl ProjectIndex {
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            ..Default::default()
        }
    }

    pub fn discover(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let mut index = Self::with_root(root.clone());
        index.load_cache();
        index.project = ComposerProject::load(&root);
        let mut scanned = HashSet::new();
        for directory in index.project.scan_directories() {
            let path = root.join(&directory);
            let key = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if scanned.insert(key) && path.is_dir() {
                index.scan_directory(&path);
            }
        }
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_env_source =
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            (name == ".env" || name.starts_with(".env.")) && !name.ends_with(".php")
                        });
                if is_env_source && path.is_file() {
                    index.scan_file(&path);
                }
            }
        }
        // composer files are useful project markers even when they contain no PHP symbols.
        for name in ["composer.json", "composer.lock"] {
            let path = root.join(name);
            if path.is_file() {
                index.fingerprints.insert(path.clone(), fingerprint(&path));
            }
        }
        index.rebuild_references();
        index.persist_cache();
        index
    }

    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    pub fn update_file(&mut self, path: &Path, text: Option<&str>) {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        self.remove_file(&path);
        if let Some(text) = text {
            let entities = parser::parse_file(&self.root, &path, text);
            self.fingerprints
                .insert(path.clone(), fingerprint_bytes(text.as_bytes()));
            self.entities.extend(entities.iter().cloned());
            self.files.insert(path, entities);
        } else if path.is_file() {
            self.scan_file(&path);
        }
        self.rebuild_references();
        self.persist_cache();
    }

    pub fn remove_file(&mut self, path: &Path) {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        self.files.remove(&path);
        self.fingerprints.remove(&path);
        self.entities
            .retain(|entity| entity.uri.to_file_path().map(|p| p != path).unwrap_or(true));
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn entities_of(&self, kind: EntityKind) -> impl Iterator<Item = &Entity> {
        self.entities
            .iter()
            .filter(move |entity| entity.kind == kind)
    }

    pub fn services_for_constructor(&self, service_id: &str) -> Vec<&Entity> {
        let dependencies = self
            .entities
            .iter()
            .find(|entity| {
                matches!(entity.kind, EntityKind::Class | EntityKind::Interface)
                    && entity.qualified_name.as_deref() == Some(service_id)
            })
            .and_then(|entity| entity.metadata.get("constructor_dependencies"));
        let Some(dependencies) = dependencies else {
            return Vec::new();
        };
        let mut services = Vec::new();
        for dependency in dependencies.lines() {
            let explicit = self
                .entities
                .iter()
                .filter(|service| {
                    matches!(service.kind, EntityKind::Service | EntityKind::ServiceAlias)
                        && (service
                            .metadata
                            .get("class")
                            .is_some_and(|class| class == dependency)
                            || service.name == dependency
                            || service
                                .metadata
                                .get("alias")
                                .is_some_and(|alias| alias == dependency))
                })
                .collect::<Vec<_>>();
            if explicit.is_empty() {
                let has_autowired_resource = self.entities.iter().any(|service| {
                    service.kind == EntityKind::Service
                        && service.name.ends_with('\\')
                        && service.metadata.contains_key("resource")
                        && dependency.starts_with(&service.name)
                });
                if has_autowired_resource {
                    services.extend(self.entities.iter().filter(|entity| {
                        matches!(entity.kind, EntityKind::Class | EntityKind::Interface)
                            && entity.qualified_name.as_deref() == Some(dependency)
                    }));
                }
            } else {
                services.extend(explicit);
            }
        }
        services
    }

    pub fn route_parameters_for(&self, route_name: &str) -> Vec<String> {
        self.entities
            .iter()
            .filter(|route| {
                route.kind == EntityKind::Route
                    && (route.name == route_name
                        || route
                            .metadata
                            .get("route_name")
                            .is_some_and(|name| name == route_name))
            })
            .flat_map(|route| {
                let path = route
                    .metadata
                    .get("path")
                    .map(String::as_str)
                    .unwrap_or(&route.name);
                route_parameters(path)
                    .unwrap_or_default()
                    .split(',')
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub fn route_config_for_controller(&self, class_name: &str) -> Vec<&Entity> {
        self.entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Route)
            .filter(|entity| {
                entity.metadata.get("controller").is_some_and(|controller| {
                    controller.split("::").next().is_some_and(|name| {
                        name.trim_start_matches('\\') == class_name.trim_start_matches('\\')
                    })
                })
            })
            .collect()
    }

    pub fn route_config_for_file(&self, file: &Path) -> Vec<&Entity> {
        let file = fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
        self.entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Route)
            .filter(|entity| {
                let Some(resource) = entity.metadata.get("resource") else {
                    return false;
                };
                let Ok(config) = entity.uri.to_file_path() else {
                    return false;
                };
                let target =
                    fs::canonicalize(config.parent().unwrap_or(Path::new(".")).join(resource))
                        .unwrap_or_else(|_| {
                            config.parent().unwrap_or(Path::new(".")).join(resource)
                        });
                file.starts_with(target)
            })
            .collect()
    }

    fn rebuild_references(&mut self) {
        let mut definitions: HashMap<(EntityKind, String), Vec<usize>> = HashMap::new();
        for (index, entity) in self.entities.iter_mut().enumerate() {
            entity.references.clear();
            if entity
                .metadata
                .get("reference")
                .is_some_and(|value| value == "true")
            {
                continue;
            }
            let mut names = vec![entity.name.clone()];
            names.extend(entity.qualified_name.iter().cloned());
            names.extend(entity.metadata.get("route_name").cloned());
            names.extend(entity.metadata.get("service_id").cloned());
            names.sort();
            names.dedup();
            for name in names {
                definitions
                    .entry((entity.kind.clone(), name.clone()))
                    .or_default()
                    .push(index);
                if let Some(scope) = entity_scope(entity) {
                    definitions
                        .entry((entity.kind.clone(), scoped_entity_name(scope, &name)))
                        .or_default()
                        .push(index);
                }
                if entity.kind == EntityKind::ServiceAlias {
                    definitions
                        .entry((EntityKind::Service, name))
                        .or_default()
                        .push(index);
                }
            }
        }
        let references = self
            .entities
            .iter()
            .filter(|entity| {
                entity
                    .metadata
                    .get("reference")
                    .is_some_and(|value| value == "true")
            })
            .map(|entity| {
                (
                    entity.kind.clone(),
                    entity.name.clone(),
                    entity.qualified_name.clone(),
                    entity_scope(entity).map(str::to_string),
                    entity.range,
                )
            })
            .collect::<Vec<_>>();
        for (kind, name, qualified_name, scope, range) in references {
            let scoped = scope
                .as_deref()
                .map(|scope| scoped_entity_name(scope, &name));
            let key = scoped
                .as_ref()
                .filter(|scoped| definitions.contains_key(&(kind.clone(), (*scoped).clone())))
                .cloned()
                .or(qualified_name)
                .unwrap_or(name);
            if let Some(indices) = definitions.get(&(kind, key)) {
                for index in indices {
                    self.entities[*index].references.push(range);
                }
            }
        }
    }

    fn scan_directory(&mut self, directory: &Path) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') || ComposerProject::is_skipped_directory(name) {
                continue;
            }
            if path.is_dir() {
                self.scan_directory(&path);
            } else {
                self.scan_file(&path);
            }
        }
    }

    fn scan_file(&mut self, path: &Path) {
        if !is_supported(path) {
            return;
        }
        let current = fingerprint(path);
        if self.fingerprints.get(path) == Some(&current) && self.files.contains_key(path) {
            return;
        }
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        self.remove_file(path);
        let entities = parser::parse_file(&self.root, path, &text);
        self.entities.extend(entities.iter().cloned());
        self.files.insert(path.to_path_buf(), entities);
        self.fingerprints.insert(path.to_path_buf(), current);
    }

    fn cache_path(&self) -> PathBuf {
        self.root.join(".zed/symfony-index.cache")
    }

    fn load_cache(&mut self) {
        let Ok(text) = fs::read_to_string(self.cache_path()) else {
            return;
        };
        // The cache intentionally stores fingerprints only. Source files remain the source of truth,
        // which keeps cache loading safe when the server version changes its parser.
        for line in text.lines() {
            let mut fields = line.splitn(2, '\t');
            let Some(path) = fields.next() else {
                continue;
            };
            let Some(value) = fields.next().and_then(|v| v.parse().ok()) else {
                continue;
            };
            self.fingerprints.insert(PathBuf::from(path), value);
        }
    }

    fn persist_cache(&self) {
        let directory = self.root.join(".zed");
        if fs::create_dir_all(&directory).is_err() {
            return;
        }
        let body = self
            .fingerprints
            .iter()
            .map(|(path, value)| format!("{}\t{}\n", path.display(), value))
            .collect::<String>();
        let temporary = directory.join("symfony-index.cache.tmp");
        if fs::write(&temporary, body).is_ok() {
            let _ = fs::rename(temporary, self.cache_path());
        }
    }
}

fn entity_scope(entity: &Entity) -> Option<&str> {
    entity
        .metadata
        .get("component")
        .or_else(|| entity.metadata.get("form_type"))
        .or_else(|| entity.metadata.get("owner"))
        .map(String::as_str)
}

fn scoped_entity_name(scope: &str, name: &str) -> String {
    format!("@{scope}::{name}")
}

fn is_supported(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("php" | "yaml" | "yml" | "xml" | "xlf" | "twig" | "json")
    ) || path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == ".env" || n.starts_with(".env."))
}

fn fingerprint(path: &Path) -> u64 {
    let modified = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    modified ^ fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}
fn fingerprint_bytes(bytes: &[u8]) -> u64 {
    bytes.iter().fold(1469598103934665603, |hash, byte| {
        (hash ^ *byte as u64).wrapping_mul(1099511628211)
    })
}

fn route_parameters(path: &str) -> Option<String> {
    let parameters = path
        .split('{')
        .skip(1)
        .filter_map(|part| part.split('}').next())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    (!parameters.is_empty()).then(|| parameters.join(","))
}

fn entity(
    kind: EntityKind,
    name: String,
    qualified_name: Option<String>,
    uri: &Url,
    line: usize,
    source: &str,
    confidence: Confidence,
) -> Entity {
    let selection = source
        .find(&name)
        .map(|start| {
            (
                source[..start].encode_utf16().count() as u32,
                name.encode_utf16().count() as u32,
            )
        })
        .unwrap_or((0, source.encode_utf16().count() as u32));
    Entity {
        kind,
        name,
        qualified_name,
        uri: uri.clone(),
        range: Range {
            start: Position {
                line: line as u32,
                character: selection.0,
            },
            end: Position {
                line: line as u32,
                character: selection.0 + selection.1,
            },
        },
        references: Vec::new(),
        metadata: HashMap::new(),
        confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::parser::parse_file;
    use super::*;
    #[test]
    fn entity_ranges_use_lsp_utf16_columns() {
        let uri = Url::parse("file:///tmp/source.php").unwrap();
        let line = "// 🧪 #[Route('/users')]";
        let entity = entity(
            EntityKind::Route,
            "/users".into(),
            None,
            &uri,
            0,
            line,
            Confidence::Exact,
        );
        let byte_start = line.find("/users").unwrap();
        assert_eq!(
            entity.range.start.character,
            line[..byte_start].encode_utf16().count() as u32
        );
        assert_eq!(
            entity.range.end.character - entity.range.start.character,
            "/users".encode_utf16().count() as u32
        );
    }

    #[test]
    fn extracts_php_symbols_and_routes() {
        let path = Path::new("/tmp/Controller.php");
        let entities = parse_file(Path::new("/tmp"), path, "<?php\nnamespace App\\Controller;\n#[Route('/users')]\nclass UserController {\n public function list() {}\n}");
        assert!(entities
            .iter()
            .any(|e| e.qualified_name.as_deref() == Some("App\\Controller\\UserController")));
        assert!(entities
            .iter()
            .any(|e| e.kind == EntityKind::Route && e.name == "/users"));
    }

    #[test]
    fn matches_constructor_dependency_to_explicit_service_id() {
        let php = parse_file(
            Path::new("/tmp"),
            Path::new("/tmp/src/Controller/HomeController.php"),
            "<?php\nnamespace App\\Controller;\nuse App\\HelloWorld\\Generator;\nclass HomeController {\n public function __construct(private readonly Generator $generator) {}\n}",
        );
        let yaml = parse_file(
            Path::new("/tmp"),
            Path::new("/tmp/config/services.yaml"),
            "services:\n    App\\:\n        resource: '../src/'\n    App\\Controller\\HomeController:\n        arguments: []\n    hello.world.generator:\n        class: 'App\\HelloWorld\\Generator'\n",
        );
        let mut index = ProjectIndex::default();
        index.entities.extend(php);
        index.entities.extend(yaml);
        let suggestions = index.services_for_constructor("App\\Controller\\HomeController");
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].name, "hello.world.generator");
    }

    #[test]
    fn discovers_all_dot_env_variants() {
        let root =
            std::env::temp_dir().join(format!("symfony-dotenv-index-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".env.dev"), "DEMO_SECRET=fixture\n").unwrap();
        let index = ProjectIndex::discover(&root);
        assert!(index.entities().iter().any(|entity| {
            entity.kind == EntityKind::EnvironmentVariable && entity.name == "DEMO_SECRET"
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn indexes_env_and_xliff_translation_entities() {
        let env_definition = parse_file(
            Path::new("/tmp"),
            Path::new("/tmp/.env"),
            "APP_SECRET=test\n",
        );
        let env_reference = parse_file(
            Path::new("/tmp"),
            Path::new("/tmp/config/packages/framework.yaml"),
            "framework:\n    secret: '%env(resolve:APP_SECRET)%'\n",
        );
        assert!(env_definition
            .iter()
            .any(|entity| entity.kind == EntityKind::EnvironmentVariable
                && entity.name == "APP_SECRET"));
        assert!(env_reference
            .iter()
            .any(|entity| entity.kind == EntityKind::EnvironmentVariable
                && entity.name == "APP_SECRET"
                && entity
                    .metadata
                    .get("reference")
                    .is_some_and(|value| value == "true")));

        let translations = parse_file(
            Path::new("/tmp"),
            Path::new("/tmp/translations/messages.en.xlf"),
            "<trans-unit id=\"welcome.title\"><source>Welcome</source></trans-unit>",
        );
        assert!(translations
            .iter()
            .any(|entity| entity.kind == EntityKind::TranslationKey
                && entity.name == "welcome.title"
                && entity
                    .metadata
                    .get("domain")
                    .is_some_and(|domain| domain == "messages")));
    }

    #[test]
    fn indexes_route_references_with_precise_ranges() {
        let route_definitions = parse_file(
            Path::new("/tmp"),
            Path::new("/tmp/config/routes.yaml"),
            "home:\n    path: /\n    controller: App\\Controller\\HomeController::index\n",
        );
        let php = parse_file(
            Path::new("/tmp"),
            Path::new("/tmp/src/Controller/HomeController.php"),
            "<?php\nfinal class HomeController {\n public function show() { return $this->generateUrl('home'); }\n}",
        );
        let mut index = ProjectIndex::default();
        index.entities.extend(route_definitions);
        index.entities.extend(php);
        index.rebuild_references();
        let route = index
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Route
                    && entity.name == "home"
                    && !entity.metadata.contains_key("reference")
            })
            .unwrap();
        assert_eq!(route.references.len(), 1);
        assert_eq!(
            route.references[0].end.character - route.references[0].start.character,
            4
        );
    }

    #[test]
    fn indexes_xml_and_php_service_configurations() {
        let xml = parse_file(
            Path::new("/tmp"),
            Path::new("/tmp/config/services.xml"),
            "<services>\n  <service id=\"mailer\" class=\"App\\Mailer\\Mailer\">\n    <tag name=\"app.mailer\"/>\n  </service>\n  <argument type=\"service\" id=\"mailer\"/>\n</services>",
        );
        assert!(xml.iter().any(|entity| entity.kind == EntityKind::Service
            && entity.name == "mailer"
            && entity
                .metadata
                .get("class")
                .is_some_and(|class| class == "App\\Mailer\\Mailer")));
        assert!(xml
            .iter()
            .any(|entity| entity.kind == EntityKind::ServiceTag && entity.name == "app.mailer"));
        assert!(xml.iter().any(|entity| entity
            .metadata
            .get("reference")
            .is_some_and(|value| value == "true")));

        let php = parse_file(
            Path::new("/tmp"),
            Path::new("/tmp/config/services.php"),
            "<?php\n$services->set('app.mailer', App\\Mailer\\Mailer::class);\n$services->alias('mailer.interface', 'app.mailer');\n$services->set('app.consumer', App\\Consumer::class)->arg('$mailer', service('app.mailer'))->tag('app.consumer');",
        );
        assert!(php
            .iter()
            .any(|entity| entity.kind == EntityKind::Service && entity.name == "app.mailer"));
        assert!(php
            .iter()
            .any(|entity| entity.kind == EntityKind::ServiceAlias
                && entity.name == "mailer.interface"));
        assert!(php
            .iter()
            .any(|entity| entity.kind == EntityKind::Service && entity.name == "app.consumer"));
        assert!(php
            .iter()
            .any(|entity| entity.kind == EntityKind::ServiceTag && entity.name == "app.consumer"));
        assert!(php.iter().any(|entity| entity.kind == EntityKind::Service
            && entity.name == "app.mailer"
            && entity
                .metadata
                .get("reference")
                .is_some_and(|value| value == "true")));
    }

    #[test]
    fn parses_yaml_route_controller_targets() {
        let path = Path::new("/tmp/config/routes.yaml");
        let entities = parse_file(
            Path::new("/tmp"),
            path,
            "home:\n    path: /users/{id}\n    controller: App\\Controller\\HomeController::index\n    defaults:\n        id: 7\n",
        );
        let route = entities
            .iter()
            .find(|entity| entity.name == "home")
            .unwrap();
        assert_eq!(
            route.metadata.get("path").map(String::as_str),
            Some("/users/{id}")
        );
        assert_eq!(
            route.metadata.get("controller").map(String::as_str),
            Some("App\\Controller\\HomeController::index")
        );
        assert_eq!(
            route.metadata.get("default:id").map(String::as_str),
            Some("7")
        );

        let controller = parse_file(
            Path::new("/tmp"),
            Path::new("/tmp/src/Controller/HomeController.php"),
            "<?php\nnamespace App\\Controller;\nclass HomeController {}",
        );
        let mut index = ProjectIndex::default();
        index.entities.extend(entities);
        index.entities.extend(controller);
        assert_eq!(
            index
                .route_config_for_controller("App\\Controller\\HomeController")
                .len(),
            1
        );
        assert_eq!(index.route_parameters_for("home"), vec!["id"]);
    }

    #[test]
    fn joins_php_definitions_to_twig_component_references() {
        let root = Path::new("/tmp/twig_component_references");
        let php_path = root.join("src/Twig/Components.php");
        let php_uri = Url::from_file_path(&php_path).unwrap();
        let php = parse_file(
            root,
            &php_path,
            "<?php\nnamespace App\\Twig;\n#[AsTwigComponent('alert')]\nclass Alert {\n public string $title;\n}\n#[AsTwigComponent('card')]\nclass Card {\n public string $title;\n}\n",
        );
        let twig_path = root.join("templates/page.html.twig");
        let twig = parse_file(
            root,
            &twig_path,
            "<twig:alert title=\"Hello\" />\n<twig:card title=\"World\" />\n",
        );
        assert!(php.iter().any(|entity| {
            entity.kind == EntityKind::TwigComponentProp
                && entity
                    .metadata
                    .get("component")
                    .is_some_and(|name| name == "alert")
        }));
        let mut index = ProjectIndex::default();
        index.entities.extend(php);
        index.entities.extend(twig);
        index.rebuild_references();
        let alert_title = index
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::TwigComponentProp
                    && entity.uri == php_uri
                    && entity
                        .metadata
                        .get("component")
                        .is_some_and(|name| name == "alert")
            })
            .unwrap();
        let card_title = index
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::TwigComponentProp
                    && entity.uri == php_uri
                    && entity
                        .metadata
                        .get("component")
                        .is_some_and(|name| name == "card")
            })
            .unwrap();
        assert_eq!(alert_title.references.len(), 1);
        assert_eq!(card_title.references.len(), 1);
        assert_ne!(alert_title.references[0], card_title.references[0]);
    }

    #[test]
    fn indexes_doctrine_php_mappings_in_the_project_parser() {
        let root = Path::new("/tmp/doctrine_entity_mappings");
        let path = root.join("src/Entity/User.php");
        let entities = parse_file(
            root,
            &path,
            "<?php\nnamespace App\\Entity;\nuse Doctrine\\ORM\\Mapping as ORM;\n#[ORM\\Entity]\nclass User {\n #[ORM\\Column(type: 'string')]\n private string $email;\n}\n",
        );
        assert!(entities.iter().any(|entity| {
            entity.kind == EntityKind::DoctrineEntity
                && entity.qualified_name.as_deref() == Some("App\\Entity\\User")
        }));
        assert!(entities.iter().any(|entity| {
            entity.kind == EntityKind::DoctrineField
                && entity.name == "email"
                && entity
                    .metadata
                    .get("owner")
                    .is_some_and(|owner| owner == "App\\Entity\\User")
                && entity
                    .metadata
                    .get("type")
                    .is_some_and(|kind| kind == "string")
        }));
    }

    #[test]
    fn uses_symfony_template_reference_names() {
        let path = Path::new("/tmp/templates/home/index.html.twig");
        let entities = parse_file(Path::new("/tmp"), path, "<h1>Home</h1>");
        assert!(entities.iter().any(|entity| {
            entity.kind == EntityKind::TwigTemplate && entity.name == "home/index.html.twig"
        }));
    }

    #[test]
    fn discovers_classes_from_composer_psr4_roots() {
        let root =
            std::env::temp_dir().join(format!("symfony-psr4-discovery-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("lib/Domain")).unwrap();
        fs::write(
            root.join("composer.json"),
            r#"{"require": {"symfony/framework-bundle": "6.4.*"}, "autoload": {"psr-4": {"App\\": "lib/"}}}"#,
        )
        .unwrap();
        fs::write(
            root.join("lib/Domain/Invoice.php"),
            "<?php\nnamespace App\\Domain;\nclass Invoice {}",
        )
        .unwrap();

        let index = ProjectIndex::discover(&root);
        assert_eq!(index.project.kind, ProjectKind::Symfony);
        assert!(index.entities().iter().any(|entity| {
            entity.kind == EntityKind::Class
                && entity.qualified_name.as_deref() == Some("App\\Domain\\Invoice")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn skips_vendor_and_cache_directories_while_scanning() {
        let root =
            std::env::temp_dir().join(format!("symfony-skip-directories-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("vendor/acme")).unwrap();
        fs::create_dir_all(root.join("var/cache")).unwrap();
        fs::write(
            root.join("src/App.php"),
            "<?php\nnamespace App;\nclass App {}",
        )
        .unwrap();
        fs::write(
            root.join("vendor/acme/Vendored.php"),
            "<?php\nnamespace Acme;\nclass Vendored {}",
        )
        .unwrap();
        fs::write(
            root.join("var/cache/Cached.php"),
            "<?php\nnamespace App\\Cache;\nclass Cached {}",
        )
        .unwrap();

        let index = ProjectIndex::discover(&root);
        assert!(index
            .entities()
            .iter()
            .any(|entity| entity.qualified_name.as_deref() == Some("App\\App")));
        assert!(!index
            .entities()
            .iter()
            .any(|entity| entity.qualified_name.as_deref() == Some("Acme\\Vendored")));
        assert!(!index
            .entities()
            .iter()
            .any(|entity| entity.qualified_name.as_deref() == Some("App\\Cache\\Cached")));
        fs::remove_dir_all(root).unwrap();
    }
}
