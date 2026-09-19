use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use std::sync::{Arc, RwLock};

use tower_lsp::jsonrpc;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use tracing::{debug, info, warn};

use crate::index::{Confidence, Entity, EntityKind, ProjectIndex};
use crate::lsp_helpers::{
    byte_offset_for_utf16, completion_name, completion_prefix, context_kind, entity_detail,
    token_at,
};

mod code_actions;
mod diagnostics;

#[cfg(test)]
use code_actions::{
    add_route_attribute_action, add_service_constructor_injection_action, add_service_tag_action,
    create_template_action,
};
use diagnostics::index_diagnostics;

#[derive(Clone)]
pub struct SymfonyLanguageServer {
    client: Client,
    root_uri: Arc<RwLock<Option<Url>>>,
    documents: Arc<RwLock<HashMap<Url, (String, i32)>>>,
    index: Arc<RwLock<Option<ProjectIndex>>>,
    supports_file_watching: Arc<RwLock<bool>>,
}

impl SymfonyLanguageServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            root_uri: Arc::new(RwLock::new(None)),
            documents: Arc::new(RwLock::new(HashMap::new())),
            index: Arc::new(RwLock::new(None)),
            supports_file_watching: Arc::new(RwLock::new(false)),
        }
    }

    fn workspace_root(params: &InitializeParams) -> Option<Url> {
        params.root_uri.clone().or_else(|| {
            params
                .workspace_folders
                .as_ref()
                .and_then(|folders| folders.first())
                .map(|folder| folder.uri.clone())
        })
    }
}

impl SymfonyLanguageServer {
    fn update_index(&self, uri: Url, text: Option<String>) {
        let Ok(path) = uri.to_file_path() else { return };
        let Ok(mut index) = self.index.write() else {
            return;
        };
        let Some(index) = index.as_mut() else { return };
        index.update_file(&path, text.as_deref());
    }
}

impl SymfonyLanguageServer {
    async fn publish_open_document_diagnostics(&self) {
        let documents = self
            .documents
            .read()
            .ok()
            .map(|documents| {
                documents
                    .iter()
                    .map(|(uri, (_, version))| (uri.clone(), *version))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (uri, version) in documents {
            self.publish_index_diagnostics(uri, Some(version)).await;
        }
    }

    async fn publish_index_diagnostics(&self, uri: Url, version: Option<i32>) {
        let text = self.document_text(&uri).unwrap_or_default();
        let diagnostics = self
            .index
            .read()
            .ok()
            .and_then(|guard| {
                guard
                    .as_ref()
                    .map(|index| index_diagnostics(index, &uri, &text))
            })
            .unwrap_or_default();
        self.client
            .publish_diagnostics(uri, diagnostics, version)
            .await;
    }

    fn document_text(&self, uri: &Url) -> Option<String> {
        self.documents
            .read()
            .ok()?
            .get(uri)
            .map(|document| document.0.clone())
            .or_else(|| {
                uri.to_file_path()
                    .ok()
                    .and_then(|path| std::fs::read_to_string(path).ok())
            })
    }

    fn symbol_at(&self, uri: &Url, position: Position) -> Option<(String, String)> {
        let text = self.document_text(uri)?;
        let line = text.lines().nth(position.line as usize).unwrap_or("");
        let mut name = token_at(line, position.character as usize);
        if matches!(
            context_kind(line),
            Some(EntityKind::FormField | EntityKind::DoctrineField)
        ) {
            name = name.rsplit('.').next().unwrap_or(&name).to_string();
        }
        Some((name, line.to_string()))
    }

    fn resolve_entity(
        &self,
        uri: &Url,
        name: &str,
        line: &str,
        position: Position,
    ) -> Option<Entity> {
        let kind = if line.contains("<twig:")
            && line
                .find(name)
                .is_some_and(|start| line[start + name.len()..].trim_start().starts_with('='))
        {
            Some(EntityKind::TwigComponentProp)
        } else {
            context_kind(line)
        };
        let index = self.index.read().ok()?;
        let index = index.as_ref()?;
        let preferred_kind = |entity: &Entity| match &entity.kind {
            EntityKind::DoctrineEntity
            | EntityKind::DoctrineField
            | EntityKind::FormType
            | EntityKind::FormField
            | EntityKind::FormOption
            | EntityKind::TwigExtension
            | EntityKind::TwigFunction
            | EntityKind::TwigFilter
            | EntityKind::TwigTest
            | EntityKind::TwigTag
            | EntityKind::TwigComponent
            | EntityKind::TwigComponentProp => 0,
            EntityKind::Class | EntityKind::Interface | EntityKind::Property => 1,
            _ => 2,
        };
        let local_candidates = index
            .entities()
            .iter()
            .filter(|entity| {
                entity.uri == *uri
                    && entity.range.start.line == position.line
                    && position.character >= entity.range.start.character
                    && position.character <= entity.range.end.character
                    && entity_matches_name(entity, name)
                    && entity.confidence != Confidence::Unknown
            })
            .collect::<Vec<_>>();
        let matched = local_candidates
            .iter()
            .copied()
            .filter(|entity| {
                kind.as_ref()
                    .map(|wanted| *wanted == entity.kind)
                    .unwrap_or(true)
            })
            .min_by_key(|entity| preferred_kind(entity))
            .or_else(|| {
                local_candidates
                    .iter()
                    .copied()
                    .min_by_key(|entity| preferred_kind(entity))
            })
            .or_else(|| {
                index
                    .entities()
                    .iter()
                    .filter(|entity| {
                        entity_matches_name(entity, name)
                            && kind
                                .as_ref()
                                .map(|wanted| *wanted == entity.kind)
                                .unwrap_or(true)
                            && entity.confidence != Confidence::Unknown
                    })
                    .min_by_key(|entity| preferred_kind(entity))
            })?;
        if matched
            .metadata
            .get("reference")
            .is_some_and(|value| value == "true")
        {
            index
                .entities()
                .iter()
                .find(|entity| {
                    entity_matches_symbol(entity, matched, name)
                        && !entity
                            .metadata
                            .get("reference")
                            .is_some_and(|value| value == "true")
                })
                .cloned()
        } else {
            Some(matched.clone())
        }
    }

    fn rename_target_at(&self, uri: &Url, position: Position) -> Option<(String, Entity, Entity)> {
        let index = self.index.read().ok()?;
        rename_target_in_index(index.as_ref()?, uri, position)
    }

    fn route_definition_from_controller(
        &self,
        uri: &Url,
        symbol: &str,
        line: &str,
    ) -> Option<Location> {
        let index = self.index.read().ok()?;
        let index = index.as_ref()?;
        let class = index.entities().iter().find(|entity| {
            entity.uri == *uri && matches!(entity.kind, EntityKind::Class | EntityKind::Interface)
        })?;
        if !line.contains("function ")
            && class.name != symbol
            && class.qualified_name.as_deref() != Some(symbol)
        {
            return None;
        }
        let class_name = class.qualified_name.as_deref().unwrap_or(&class.name);
        let mut routes = index.route_config_for_controller(class_name);
        if line.contains("function ") {
            routes.retain(|route| {
                route.metadata.get("controller").is_some_and(|target| {
                    target
                        .rsplit_once("::")
                        .is_some_and(|(_, method)| method == symbol)
                })
            });
        }
        if routes.len() != 1 {
            return None;
        }
        let route = routes[0];
        Some(Location {
            uri: route.uri.clone(),
            range: route.range,
        })
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for SymfonyLanguageServer {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        let root_uri = Self::workspace_root(&params);
        let supports_file_watching = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.did_change_watched_files.as_ref())
            .is_some_and(|capabilities| capabilities.dynamic_registration == Some(true));
        if let Ok(mut supports) = self.supports_file_watching.write() {
            *supports = supports_file_watching;
        }
        if let Ok(mut root) = self.root_uri.write() {
            *root = root_uri.clone();
        }

        if let Some(options) = params.initialization_options.as_ref() {
            info!("received initialization options: {options}");
        }
        info!("initializing Symfony LSP for workspace {:?}", root_uri);
        if let Some(uri) = root_uri.as_ref() {
            if let Ok(root) = uri.to_file_path() {
                let index = ProjectIndex::discover(root);
                info!(
                    "detected {:?} project; indexed {} Symfony entities across {} files",
                    index.project.kind,
                    index.entities().len(),
                    index.file_count()
                );
                if index.project.is_symfony() {
                    let components = [
                        "doctrine/orm",
                        "symfony/form",
                        "symfony/translation",
                        "symfony/ux-twig-component",
                    ]
                    .into_iter()
                    .filter(|package| index.project.has_package(package))
                    .collect::<Vec<_>>();
                    info!("detected optional Symfony components: {components:?}");
                    match index.project.symfony_version {
                        Some(version) if version.is_supported() => {
                            info!("detected Symfony {}.{}", version.major, version.minor);
                        }
                        Some(version) => {
                            warn!(
                                "detected Symfony {}.{}, below the supported 6.4 baseline; version-sensitive behavior stays disabled",
                                version.major, version.minor
                            );
                        }
                        None => {
                            info!(
                                "Symfony version could not be determined; version-sensitive behavior stays disabled"
                            );
                        }
                    }
                }
                if let Ok(mut current) = self.index.write() {
                    *current = Some(index);
                }
            }
        }

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                })),
                document_link_provider: Some(DocumentLinkOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                }),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![
                            CodeActionKind::QUICKFIX,
                            CodeActionKind::REFACTOR_REWRITE,
                        ]),
                        ..Default::default()
                    },
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        "'".to_string(),
                        "\"".to_string(),
                        ":".to_string(),
                        "%".to_string(),
                        "@".to_string(),
                    ]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "Symfony LSP".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        info!("Symfony LSP initialized");
        let _ = self
            .client
            .log_message(
                MessageType::INFO,
                "Symfony LSP initialized; project index is ready for navigation and completion.",
            )
            .await;
        if self
            .supports_file_watching
            .read()
            .map(|supports| *supports)
            .unwrap_or(false)
        {
            let registration = Registration {
                id: "symfony-lsp-workspace-files".into(),
                method: "workspace/didChangeWatchedFiles".into(),
                register_options: Some(serde_json::json!({
                    "watchers": [
                        { "globPattern": "**/*.php" },
                        { "globPattern": "**/*.yaml" },
                        { "globPattern": "**/*.yml" },
                        { "globPattern": "**/*.xml" },
                        { "globPattern": "**/*.xlf" },
                        { "globPattern": "**/*.twig" },
                        { "globPattern": "**/*.json" },
                        { "globPattern": "**/.env*" },
                        { "globPattern": ".env*" }
                    ]
                })),
            };
            if let Err(error) = self.client.register_capability(vec![registration]).await {
                warn!("failed to register Symfony workspace file watchers: {error}");
            }
        }
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        info!("shutting down Symfony LSP");
        if let Ok(mut documents) = self.documents.write() {
            documents.clear();
        }
        if let Ok(mut index) = self.index.write() {
            *index = None;
        }
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        info!("opened {}", document.uri);
        let uri = document.uri.clone();
        if let Ok(mut documents) = self.documents.write() {
            documents.insert(uri.clone(), (document.text.clone(), document.version));
        }
        self.update_index(uri, Some(document.text));
        self.publish_open_document_diagnostics().await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let Some(change) = params.content_changes.into_iter().next() else {
            return;
        };

        debug!(
            "updated {} to version {}",
            uri, params.text_document.version
        );
        let text = change.text;
        if let Ok(mut documents) = self.documents.write() {
            documents.insert(uri.clone(), (text.clone(), params.text_document.version));
        }
        self.update_index(uri, Some(text));
        self.publish_open_document_diagnostics().await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        debug!("saved {}", params.text_document.uri);
        self.update_index(params.text_document.uri, params.text);
        self.publish_open_document_diagnostics().await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        debug!("closed {}", params.text_document.uri);
        if let Ok(mut documents) = self.documents.write() {
            documents.remove(&params.text_document.uri);
        }
        let uri = params.text_document.uri;
        self.update_index(uri.clone(), None);
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
        self.publish_open_document_diagnostics().await;
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        debug!("configuration changed: {}", params.settings);
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        debug!("received {} watched-file changes", params.changes.len());
        for change in params.changes {
            match change.typ {
                FileChangeType::DELETED => self.update_index(change.uri, None),
                FileChangeType::CREATED | FileChangeType::CHANGED => {
                    self.update_index(change.uri, None)
                }
                _ => {}
            }
        }
        let open_documents = self
            .documents
            .read()
            .ok()
            .map(|documents| {
                documents
                    .iter()
                    .map(|(uri, (text, version))| (uri.clone(), text.clone(), *version))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (uri, text, _) in open_documents {
            self.update_index(uri, Some(text));
        }
        self.publish_open_document_diagnostics().await;
    }

    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let position = params.text_document_position_params;
        let Some((name, line)) = self.symbol_at(&position.text_document.uri, position.position)
        else {
            return Ok(None);
        };
        let Some(entity) =
            self.resolve_entity(&position.text_document.uri, &name, &line, position.position)
        else {
            return Ok(None);
        };
        let detail = entity_detail(&entity);
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: detail,
            }),
            range: Some(entity.range),
        }))
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> jsonrpc::Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let text = self.document_text(&uri).unwrap_or_default();
        let Ok(index_guard) = self.index.read() else {
            return Ok(None);
        };
        let Some(index) = index_guard.as_ref() else {
            return Ok(None);
        };
        Ok(code_actions::build_code_actions(
            index,
            &uri,
            params.range,
            &params.context.diagnostics,
            &text,
        ))
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> jsonrpc::Result<Option<CompletionResponse>> {
        let position = params.text_document_position;
        let Some(text) = self.document_text(&position.text_document.uri) else {
            return Ok(None);
        };
        let line = text
            .lines()
            .nth(position.position.line as usize)
            .unwrap_or("");
        let mut prefix = completion_prefix(line, position.position.character as usize);
        let mut kind = context_kind(line);
        let Ok(index) = self.index.read() else {
            return Ok(None);
        };
        let Some(index) = index.as_ref() else {
            return Ok(None);
        };
        let cursor_byte = byte_offset_for_utf16(line, position.position.character as usize);
        let before_cursor = &line[..cursor_byte];
        if kind == Some(EntityKind::TwigComponent)
            && before_cursor.contains("<twig:")
            && before_cursor
                .rfind(' ')
                .is_some_and(|space| before_cursor.rfind('<').is_some_and(|open| space > open))
        {
            kind = Some(EntityKind::TwigComponentProp);
        }
        let lower_line = line.to_ascii_lowercase();
        if kind == Some(EntityKind::DoctrineField) {
            let items = doctrine_field_completions(
                index,
                &text,
                line,
                position.position.character as usize,
            )
            .unwrap_or_default();
            return if items.is_empty() {
                Ok(None)
            } else {
                Ok(Some(CompletionResponse::Array(items)))
            };
        }
        if kind == Some(EntityKind::FormField) && lower_line.contains("->add(") {
            let items = form_field_completions(
                index,
                &position.text_document.uri,
                &prefix,
                position.position.line,
            );
            if !items.is_empty() {
                return Ok(Some(CompletionResponse::Array(items)));
            }
        }
        if kind == Some(EntityKind::FormField) && before_cursor.contains("form.") {
            prefix = before_cursor
                .rsplit_once("form.")
                .map(|(_, tail)| tail)
                .unwrap_or("")
                .to_string();
        } else if kind == Some(EntityKind::FormOption) {
            prefix = before_cursor
                .rsplit(['\'', '"'])
                .next()
                .unwrap_or("")
                .to_string();
        }
        if kind == Some(EntityKind::Route)
            && before_cursor.contains(',')
            && (lower_line.contains("path(")
                || lower_line.contains("url(")
                || lower_line.contains("generate("))
        {
            let route_name = first_quoted_value(line);
            let items = route_name
                .map(|route_name| {
                    index
                        .route_parameters_for(&route_name)
                        .into_iter()
                        .filter(|parameter| parameter.starts_with(&prefix))
                        .map(|parameter| CompletionItem {
                            label: parameter.clone(),
                            kind: Some(CompletionItemKind::FIELD),
                            detail: Some(format!("Parameter for route `{route_name}`")),
                            ..Default::default()
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !items.is_empty() {
                return Ok(Some(CompletionResponse::Array(items)));
            }
        }
        if kind == Some(EntityKind::FormOption) {
            let mut options = index
                .entities_of(EntityKind::FormOption)
                .map(|entity| entity.name.clone())
                .collect::<HashSet<_>>();
            if lower_line.contains("entitytype") {
                options.extend(
                    [
                        "class",
                        "choice_label",
                        "multiple",
                        "expanded",
                        "placeholder",
                        "query_builder",
                    ]
                    .into_iter()
                    .map(str::to_string),
                );
            }
            options.extend(
                [
                    "required",
                    "label",
                    "help",
                    "attr",
                    "constraints",
                    "mapped",
                    "data",
                    "disabled",
                ]
                .into_iter()
                .map(str::to_string),
            );
            let items = options
                .into_iter()
                .filter(|option| option.starts_with(&prefix))
                .map(|option| CompletionItem {
                    label: option.clone(),
                    kind: Some(CompletionItemKind::PROPERTY),
                    detail: Some("Symfony form option".into()),
                    ..Default::default()
                })
                .collect::<Vec<_>>();
            if !items.is_empty() {
                return Ok(Some(CompletionResponse::Array(items)));
            }
        }
        let component_context = if kind == Some(EntityKind::TwigComponentProp) {
            before_cursor
                .split_once("<twig:")
                .and_then(|(_, tail)| tail.split([' ', '>', '/']).next())
                .map(str::to_string)
        } else {
            None
        };
        let candidates: Vec<&Entity> = if kind == Some(EntityKind::Service) {
            let service_id = service_definition_id(&text, position.position.line as usize);
            let constructor_services = service_id
                .as_deref()
                .map(|service_id| index.services_for_constructor(service_id))
                .unwrap_or_default();
            if constructor_services.is_empty() {
                index
                    .entities()
                    .iter()
                    .filter(|entity| {
                        matches!(entity.kind, EntityKind::Service | EntityKind::ServiceAlias)
                            && !entity
                                .metadata
                                .get("reference")
                                .is_some_and(|value| value == "true")
                    })
                    .collect()
            } else {
                constructor_services
            }
        } else {
            index
                .entities()
                .iter()
                .filter(|entity| {
                    Some(&entity.kind) == kind.as_ref()
                        && !entity
                            .metadata
                            .get("reference")
                            .is_some_and(|value| value == "true")
                        && component_context.as_deref().is_none_or(|component| {
                            entity
                                .metadata
                                .get("component")
                                .is_some_and(|name| name == component)
                        })
                })
                .collect()
        };
        let items = candidates
            .into_iter()
            .filter(|entity| completion_name(entity).starts_with(&prefix))
            .map(|entity| CompletionItem {
                label: completion_name(entity),
                kind: Some(CompletionItemKind::REFERENCE),
                detail: Some(entity_detail(entity)),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            return Ok(None);
        }
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let position = params.text_document_position_params;
        let Some((name, line)) = self.symbol_at(&position.text_document.uri, position.position)
        else {
            return Ok(None);
        };
        if let Some(location) =
            self.route_definition_from_controller(&position.text_document.uri, &name, &line)
        {
            return Ok(Some(GotoDefinitionResponse::Scalar(location)));
        }
        let Some(entity) =
            self.resolve_entity(&position.text_document.uri, &name, &line, position.position)
        else {
            return Ok(None);
        };
        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: entity.uri.clone(),
            range: entity.range,
        })))
    }

    async fn document_link(
        &self,
        params: DocumentLinkParams,
    ) -> jsonrpc::Result<Option<Vec<DocumentLink>>> {
        let Some(text) = self.document_text(&params.text_document.uri) else {
            return Ok(None);
        };
        let Ok(index) = self.index.read() else {
            return Ok(None);
        };
        let Some(index) = index.as_ref() else {
            return Ok(None);
        };
        let mut links = Vec::new();
        let file = params.text_document.uri.to_file_path().ok();
        if let Some(file) = file.as_ref() {
            let classes = index.entities().iter().filter(|entity| {
                entity.uri == params.text_document.uri
                    && matches!(entity.kind, EntityKind::Class | EntityKind::Interface)
            });
            for class in classes {
                if let Some(class_name) = class.qualified_name.as_deref() {
                    for route in index.route_config_for_controller(class_name) {
                        links.push(DocumentLink {
                            range: class.range,
                            target: Some(route.uri.clone()),
                            tooltip: Some(format!(
                                "Open route `{}` in routes configuration",
                                route.name
                            )),
                            data: None,
                        });
                    }
                }
            }
            for component in index.entities().iter().filter(|entity| {
                entity.uri == params.text_document.uri
                    && entity.kind == EntityKind::TwigComponent
                    && !entity
                        .metadata
                        .get("reference")
                        .is_some_and(|value| value == "true")
            }) {
                if let Some(template_name) = component.metadata.get("template") {
                    if let Some(template) =
                        index.entities_of(EntityKind::TwigTemplate).find(|entity| {
                            entity.name == *template_name
                                && !entity
                                    .metadata
                                    .get("reference")
                                    .is_some_and(|value| value == "true")
                        })
                    {
                        links.push(DocumentLink {
                            range: component.range,
                            target: Some(template.uri.clone()),
                            tooltip: Some(format!(
                                "Open Twig component template `{template_name}`"
                            )),
                            data: None,
                        });
                    }
                }
            }
            for config in index.route_config_for_file(file) {
                if let Some((line_number, line)) = text
                    .lines()
                    .enumerate()
                    .find(|(_, line)| line.contains("class "))
                {
                    links.push(DocumentLink {
                        range: line_range(line_number, line),
                        target: Some(config.uri.clone()),
                        tooltip: Some("Open Symfony route configuration".into()),
                        data: None,
                    });
                }
            }
        }
        for reference in index.entities().iter().filter(|entity| {
            entity.uri == params.text_document.uri
                && entity.kind == EntityKind::TwigComponent
                && entity
                    .metadata
                    .get("reference")
                    .is_some_and(|value| value == "true")
        }) {
            if let Some(component) = index.entities_of(EntityKind::TwigComponent).find(|entity| {
                entity.name == reference.name
                    && !entity
                        .metadata
                        .get("reference")
                        .is_some_and(|value| value == "true")
            }) {
                links.push(DocumentLink {
                    range: reference.range,
                    target: Some(component.uri.clone()),
                    tooltip: Some(format!("Open Twig component `{}`", component.name)),
                    data: None,
                });
            }
        }
        for reference in index.entities().iter().filter(|entity| {
            entity.uri == params.text_document.uri
                && entity.kind == EntityKind::TwigTemplate
                && entity
                    .metadata
                    .get("reference")
                    .is_some_and(|value| value == "true")
        }) {
            if let Some(template) = index.entities_of(EntityKind::TwigTemplate).find(|entity| {
                !entity
                    .metadata
                    .get("reference")
                    .is_some_and(|value| value == "true")
                    && template_reference_matches_definition(&reference.name, entity)
            }) {
                links.push(DocumentLink {
                    range: reference.range,
                    target: Some(template.uri.clone()),
                    tooltip: Some(format!("Open Twig template {}", template.name)),
                    data: None,
                });
            }
        }
        if links.is_empty() {
            Ok(None)
        } else {
            Ok(Some(links))
        }
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> jsonrpc::Result<Option<PrepareRenameResponse>> {
        let Some((name, local, _definition)) =
            self.rename_target_at(&params.text_document.uri, params.position)
        else {
            return Ok(None);
        };
        let placeholder = if local.kind == EntityKind::TwigTemplate {
            local.name.clone()
        } else {
            name
        };
        Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: local.range,
            placeholder,
        }))
    }

    async fn references(&self, params: ReferenceParams) -> jsonrpc::Result<Option<Vec<Location>>> {
        let position = params.text_document_position;
        let Some((name, line)) = self.symbol_at(&position.text_document.uri, position.position)
        else {
            return Ok(None);
        };
        let Some(definition) =
            self.resolve_entity(&position.text_document.uri, &name, &line, position.position)
        else {
            return Ok(None);
        };
        let Ok(index) = self.index.read() else {
            return Ok(None);
        };
        let Some(index) = index.as_ref() else {
            return Ok(None);
        };
        let mut locations = Vec::new();
        for entity in index.entities() {
            if entity.uri == definition.uri && entity.range == definition.range {
                continue;
            }
            if entity_matches_symbol(entity, &definition, &name) {
                locations.push(Location {
                    uri: entity.uri.clone(),
                    range: entity.range,
                });
            }
        }
        if params.context.include_declaration {
            locations.push(Location {
                uri: definition.uri.clone(),
                range: definition.range,
            });
        }
        Ok(Some(locations))
    }

    async fn rename(&self, params: RenameParams) -> jsonrpc::Result<Option<WorkspaceEdit>> {
        let Some((name, _local, definition)) = self.rename_target_at(
            &params.text_document_position.text_document.uri,
            params.text_document_position.position,
        ) else {
            return Ok(None);
        };
        let Ok(index) = self.index.read() else {
            return Ok(None);
        };
        let Some(index) = index.as_ref() else {
            return Ok(None);
        };
        if definition.kind == EntityKind::TwigTemplate {
            return Ok(rename_template(index, &definition, &name, &params.new_name));
        }
        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        for entity in index.entities() {
            if entity_matches_symbol(entity, &definition, &name) {
                changes
                    .entry(entity.uri.clone())
                    .or_default()
                    .push(TextEdit {
                        range: entity.range,
                        new_text: params.new_name.clone(),
                    });
            }
        }
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }))
    }

    async fn did_change_workspace_folders(&self, _: DidChangeWorkspaceFoldersParams) {
        warn!("workspace folders changed; multi-root re-discovery is not implemented yet");
    }
}

fn rename_target_in_index(
    index: &ProjectIndex,
    uri: &Url,
    position: Position,
) -> Option<(String, Entity, Entity)> {
    let locals = index
        .entities()
        .iter()
        .filter(|entity| {
            entity.uri == *uri
                && entity.range.start.line == position.line
                && position.character >= entity.range.start.character
                && position.character <= entity.range.end.character
                && matches!(
                    entity.kind,
                    EntityKind::Route
                        | EntityKind::TwigTemplate
                        | EntityKind::TranslationKey
                        | EntityKind::EnvironmentVariable
                        | EntityKind::Service
                        | EntityKind::ServiceAlias
                        | EntityKind::DoctrineEntity
                        | EntityKind::TwigFunction
                        | EntityKind::TwigFilter
                        | EntityKind::TwigTest
                        | EntityKind::TwigTag
                        | EntityKind::TwigComponent
                        | EntityKind::TwigComponentProp
                )
                && (entity.kind != EntityKind::TwigTemplate
                    || entity
                        .metadata
                        .get("reference")
                        .is_some_and(|value| value == "true"))
        })
        .collect::<Vec<_>>();
    if locals.len() != 1 {
        return None;
    }
    let local = locals[0];
    let name = if local.kind == EntityKind::Route {
        local
            .metadata
            .get("route_name")
            .cloned()
            .unwrap_or_else(|| local.name.clone())
    } else {
        local.name.clone()
    };
    let definitions = index
        .entities()
        .iter()
        .filter(|entity| {
            same_entity_kind(entity.kind.clone(), local.kind.clone())
                && !entity
                    .metadata
                    .get("reference")
                    .is_some_and(|value| value == "true")
                && if local.kind == EntityKind::TwigTemplate {
                    template_reference_matches_definition(&local.name, entity)
                } else {
                    entity_matches_symbol(entity, local, &name)
                }
        })
        .collect::<Vec<_>>();
    if definitions.len() != 1 {
        return None;
    }
    let rename_name = if local.kind == EntityKind::TwigTemplate {
        definitions[0].name.clone()
    } else {
        name
    };
    Some((rename_name, local.clone(), definitions[0].clone()))
}

fn same_entity_kind(left: EntityKind, right: EntityKind) -> bool {
    left == right
        || matches!(
            (left, right),
            (EntityKind::Service, EntityKind::ServiceAlias)
                | (EntityKind::ServiceAlias, EntityKind::Service)
        )
}

fn template_reference_matches_definition(reference_name: &str, definition: &Entity) -> bool {
    definition.name == reference_name
        || definition
            .name
            .strip_suffix(reference_name)
            .is_some_and(|prefix| prefix.ends_with('/'))
}

fn rename_template(
    index: &ProjectIndex,
    definition: &Entity,
    old_name: &str,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    let mut logical = if new_name.contains('/') || new_name.contains('\\') {
        PathBuf::from(new_name)
    } else {
        Path::new(old_name)
            .parent()
            .unwrap_or(Path::new(""))
            .join(new_name)
    };
    if logical.extension().is_none() {
        let old_file_name = Path::new(old_name)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(old_name);
        let suffix = old_file_name
            .find('.')
            .map(|dot| &old_file_name[dot..])
            .unwrap_or(".twig");
        let file_name = logical.file_name()?.to_string_lossy();
        logical.set_file_name(format!("{file_name}{suffix}"));
    }
    if logical
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let logical_name = logical.to_string_lossy().replace('\\', "/");
    let new_path = index.root.join("templates").join(&logical);
    if new_path.exists() {
        return None;
    }
    let new_uri = Url::from_file_path(new_path).ok()?;
    let mut by_uri: HashMap<Url, Vec<OneOf<TextEdit, AnnotatedTextEdit>>> = HashMap::new();
    for reference in index.entities().iter().filter(|entity| {
        entity.kind == EntityKind::TwigTemplate
            && entity
                .metadata
                .get("reference")
                .is_some_and(|value| value == "true")
            && template_reference_matches_definition(&entity.name, definition)
    }) {
        by_uri
            .entry(reference.uri.clone())
            .or_default()
            .push(OneOf::Left(TextEdit {
                range: reference.range,
                new_text: logical_name.clone(),
            }));
    }
    let mut operations = by_uri
        .into_iter()
        .map(|(uri, edits)| {
            DocumentChangeOperation::Edit(TextDocumentEdit {
                text_document: OptionalVersionedTextDocumentIdentifier { uri, version: None },
                edits,
            })
        })
        .collect::<Vec<_>>();
    operations.push(DocumentChangeOperation::Op(ResourceOp::Rename(
        RenameFile {
            old_uri: definition.uri.clone(),
            new_uri,
            options: Some(RenameFileOptions {
                overwrite: Some(false),
                ignore_if_exists: Some(false),
            }),
            annotation_id: None,
        },
    )));
    Some(WorkspaceEdit {
        document_changes: Some(DocumentChanges::Operations(operations)),
        ..Default::default()
    })
}

fn first_quoted_value(line: &str) -> Option<String> {
    let start = line.find(['\'', '"'])?;
    let quote = line.as_bytes()[start] as char;
    let end = line[start + 1..].find(quote)? + start + 1;
    Some(line[start + 1..end].to_string())
}

fn form_field_completions(
    index: &ProjectIndex,
    uri: &Url,
    prefix: &str,
    cursor_line: u32,
) -> Vec<CompletionItem> {
    let form_type = index
        .entities_of(EntityKind::FormType)
        .filter(|form_type| {
            form_type.uri == *uri
                && !form_type
                    .metadata
                    .get("reference")
                    .is_some_and(|value| value == "true")
        })
        .filter(|form_type| form_type.range.start.line <= cursor_line)
        .max_by_key(|form_type| form_type.range.start.line);
    let Some(form_type) = form_type else {
        return Vec::new();
    };
    let form_type_name = form_type
        .qualified_name
        .as_deref()
        .unwrap_or(&form_type.name);
    let mut candidates = index
        .entities_of(EntityKind::FormField)
        .filter(|field| {
            field
                .metadata
                .get("form_type")
                .is_some_and(|name| name == form_type_name)
        })
        .map(|field| (field.name.clone(), "Form field".to_string()))
        .collect::<HashMap<_, _>>();
    if let Some(data_class) = form_type.metadata.get("data_class") {
        for field in index
            .entities_of(EntityKind::DoctrineField)
            .filter(|field| {
                field.metadata.get("owner") == Some(data_class)
                    && !field
                        .metadata
                        .get("reference")
                        .is_some_and(|value| value == "true")
            })
        {
            candidates.entry(field.name.clone()).or_insert_with(|| {
                field
                    .metadata
                    .get("type")
                    .map(|kind| format!("Doctrine entity field ({kind})"))
                    .unwrap_or_else(|| "Doctrine entity field".into())
            });
        }
    }
    let mut items = candidates
        .into_iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(name, detail)| CompletionItem {
            label: name.clone(),
            insert_text: Some(name),
            kind: Some(CompletionItemKind::FIELD),
            detail: Some(detail),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.label.cmp(&right.label));
    items
}

fn doctrine_field_completions(
    index: &ProjectIndex,
    text: &str,
    line: &str,
    character: usize,
) -> Option<Vec<CompletionItem>> {
    let cursor_byte = byte_offset_for_utf16(line, character);
    let before = line.get(..cursor_byte)?;
    let dot = before.rfind('.')?;
    let alias_end = dot;
    let alias_start = before[..alias_end]
        .rfind(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .map(|index| index + 1)
        .unwrap_or(0);
    let alias = &before[alias_start..alias_end];
    if alias.is_empty() || !before[..dot].contains(['\'', '"']) {
        return None;
    }
    let typed = before[dot + 1..].trim_start_matches(|character: char| {
        !character.is_ascii_alphanumeric() && character != '_'
    });
    let aliases = doctrine_query_aliases(index, text);
    let owner = aliases.get(alias)?;
    let mut items = index
        .entities_of(EntityKind::DoctrineField)
        .filter(|field| {
            field.metadata.get("owner") == Some(owner)
                && !field
                    .metadata
                    .get("reference")
                    .is_some_and(|value| value == "true")
                && field.name.starts_with(typed)
        })
        .map(|field| {
            let detail = field
                .metadata
                .get("type")
                .map(|field_type| format!("Doctrine field ({field_type})"))
                .unwrap_or_else(|| "Doctrine field".into());
            CompletionItem {
                label: field.name.clone(),
                insert_text: Some(format!("{alias}.{}", field.name)),
                kind: Some(CompletionItemKind::FIELD),
                detail: Some(detail),
                ..Default::default()
            }
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.label.cmp(&right.label));
    Some(items)
}

fn doctrine_query_aliases(index: &ProjectIndex, text: &str) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    let repository_entity = text.lines().find_map(|source_line| {
        let declaration = source_line.trim();
        let declaration = declaration
            .strip_prefix("final ")
            .or_else(|| declaration.strip_prefix("abstract "))
            .or_else(|| declaration.strip_prefix("readonly "))
            .unwrap_or(declaration)
            .strip_prefix("class ")?;
        let short = declaration
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .next()?;
        let entity_name = short.strip_suffix("Repository")?;
        index
            .entities_of(EntityKind::DoctrineEntity)
            .find(|entity| entity.name == entity_name)
            .map(|entity| {
                entity
                    .qualified_name
                    .clone()
                    .unwrap_or_else(|| entity.name.clone())
            })
    });
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(start) = lower.find("->from(") {
            let tail = &line[start + "->from(".len()..];
            let class = tail
                .split_once("::class")
                .map(|(class, _)| class.trim().trim_start_matches('\\'))
                .filter(|class| !class.is_empty());
            if let Some(class) = class {
                let short = class.rsplit('\\').next().unwrap_or(class);
                let definition = index
                    .entities_of(EntityKind::DoctrineEntity)
                    .find(|entity| {
                        entity.qualified_name.as_deref() == Some(class)
                            || entity.name == class
                            || entity.name == short
                    });
                if let (Some(definition), Some(alias)) = (definition, first_quoted_value(tail)) {
                    aliases.insert(
                        alias,
                        definition
                            .qualified_name
                            .clone()
                            .unwrap_or_else(|| definition.name.clone()),
                    );
                }
            }
        } else if lower.contains("->createquerybuilder(") {
            if let (Some(owner), Some(alias)) =
                (repository_entity.as_ref(), first_quoted_value(line))
            {
                aliases.insert(alias, owner.clone());
            }
        }
    }
    aliases
}

fn entity_matches_symbol(entity: &Entity, definition: &Entity, name: &str) -> bool {
    let same_kind = same_entity_kind(entity.kind.clone(), definition.kind.clone())
        || matches!(
            (entity.kind.clone(), definition.kind.clone()),
            (EntityKind::Class, EntityKind::DoctrineEntity)
                | (EntityKind::DoctrineEntity, EntityKind::Class)
        );
    if !same_kind {
        return false;
    }
    if definition.kind == EntityKind::TwigTemplate {
        if entity
            .metadata
            .get("reference")
            .is_some_and(|value| value == "true")
        {
            return template_reference_matches_definition(&entity.name, definition);
        }
        if definition
            .metadata
            .get("reference")
            .is_some_and(|value| value == "true")
        {
            return template_reference_matches_definition(&definition.name, entity);
        }
    }
    if matches!(
        definition.kind,
        EntityKind::Class | EntityKind::Interface | EntityKind::DoctrineEntity
    ) && definition.qualified_name.is_some()
    {
        return entity.qualified_name == definition.qualified_name;
    }
    let definition_scope = entity_scope(definition);
    let candidate_scope = entity_scope(entity);
    if definition_scope.is_some()
        && candidate_scope.is_some()
        && definition_scope != candidate_scope
    {
        return false;
    }
    entity_matches_name(entity, name)
}

fn entity_scope(entity: &Entity) -> Option<&str> {
    entity
        .metadata
        .get("component")
        .or_else(|| entity.metadata.get("form_type"))
        .or_else(|| entity.metadata.get("owner"))
        .map(String::as_str)
}

fn entity_matches_name(entity: &Entity, name: &str) -> bool {
    entity.name == name
        || entity.qualified_name.as_deref() == Some(name)
        || entity
            .metadata
            .get("route_name")
            .is_some_and(|route_name| route_name == name)
        || entity
            .metadata
            .get("service_id")
            .is_some_and(|service_id| service_id == name)
}

fn service_definition_id(text: &str, cursor_line: usize) -> Option<String> {
    text.lines()
        .take(cursor_line + 1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .find_map(|line| {
            let indent = line.len() - line.trim_start().len();
            if indent != 4 || line.trim_start().starts_with('#') {
                return None;
            }
            let key = line
                .trim()
                .split_once(':')?
                .0
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            if key.is_empty() || key.starts_with('_') || key.ends_with('\\') || key == "services" {
                None
            } else {
                Some(key.to_string())
            }
        })
}

fn line_range(line: usize, source: &str) -> Range {
    Range {
        start: Position {
            line: line as u32,
            character: 0,
        },
        end: Position {
            line: line as u32,
            character: source.encode_utf16().count() as u32,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::ProjectKind;

    #[test]
    fn completes_doctrine_query_builder_fields_by_alias() {
        let uri = Url::parse("file:///tmp/src/Entity/User.php").unwrap();
        let mut index = ProjectIndex::default();
        index.entities.push(Entity {
            kind: EntityKind::DoctrineEntity,
            name: "User".into(),
            qualified_name: Some("App\\Entity\\User".into()),
            uri: uri.clone(),
            range: Range::default(),
            references: Vec::new(),
            metadata: HashMap::from([("owner".into(), "App\\Entity\\User".into())]),
            confidence: Confidence::Exact,
        });
        index.entities.push(Entity {
            kind: EntityKind::DoctrineField,
            name: "name".into(),
            qualified_name: Some("App\\Entity\\User::name".into()),
            uri,
            range: Range::default(),
            references: Vec::new(),
            metadata: HashMap::from([
                ("owner".into(), "App\\Entity\\User".into()),
                ("type".into(), "string".into()),
            ]),
            confidence: Confidence::Exact,
        });
        let text = "$qb->from(User::class, 'u');\n$qb->andWhere('u.na');";
        let line = text.lines().nth(1).unwrap();
        let cursor = line.find("na');").unwrap() + 2;
        let items = doctrine_field_completions(&index, text, line, cursor).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "name");
        assert_eq!(items[0].insert_text.as_deref(), Some("u.name"));
    }

    #[test]
    fn completes_form_fields_from_the_declared_doctrine_data_class() {
        let root = std::env::temp_dir().join(format!(
            "symfony-lsp-form-completion-{}",
            std::process::id()
        ));
        let form_path = root.join("src/Form/UserType.php");
        let entity_path = root.join("src/Entity/User.php");
        std::fs::create_dir_all(form_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(entity_path.parent().unwrap()).unwrap();
        std::fs::write(
            &form_path,
            "<?php\nnamespace App\\Form;\nuse App\\Entity\\User;\nclass UserType extends AbstractType {\n public function configureOptions($resolver) {\n  $resolver->setDefaults(['data_class' => User::class]);\n }\n public function buildForm($builder) {\n  $builder->add('na', TextType::class);\n }\n}\n",
        )
        .unwrap();
        std::fs::write(
            &entity_path,
            "<?php\nnamespace App\\Entity;\n#[ORM\\Entity]\nclass User {\n #[ORM\\Column(type: 'string')]\n private string $name;\n}\n",
        )
        .unwrap();
        let form_uri = Url::from_file_path(&form_path).unwrap();
        let index = ProjectIndex::discover(&root);
        let items = form_field_completions(&index, &form_uri, "na", 8);
        assert!(items.iter().any(|item| {
            item.label == "name" && item.detail.as_deref() == Some("Doctrine entity field (string)")
        }));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_service_definition_for_nested_arguments() {
        let text = "services:\n    App\\Controller\\HomeController:\n        arguments: ['@']\n";
        assert_eq!(
            service_definition_id(text, 2).as_deref(),
            Some("App\\Controller\\HomeController")
        );
    }

    #[test]
    fn recognizes_string_references_as_renameable_symbols() {
        let reference_uri = Url::parse("file:///tmp/src/Controller/HomeController.php").unwrap();
        let definition_uri = Url::parse("file:///tmp/config/routes.yaml").unwrap();
        let mut definition_metadata = HashMap::new();
        definition_metadata.insert("route_name".into(), "hello_world".into());
        let mut reference_metadata = HashMap::new();
        reference_metadata.insert("reference".into(), "true".into());
        let mut index = ProjectIndex::default();
        index.entities.push(Entity {
            kind: EntityKind::Route,
            name: "hello_world".into(),
            qualified_name: None,
            uri: definition_uri.clone(),
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 11,
                },
            },
            references: Vec::new(),
            metadata: definition_metadata,
            confidence: Confidence::Exact,
        });
        index.entities.push(Entity {
            kind: EntityKind::Route,
            name: "hello_world".into(),
            qualified_name: None,
            uri: reference_uri.clone(),
            range: Range {
                start: Position {
                    line: 8,
                    character: 42,
                },
                end: Position {
                    line: 8,
                    character: 53,
                },
            },
            references: Vec::new(),
            metadata: reference_metadata,
            confidence: Confidence::Likely,
        });
        let (name, local, definition) = rename_target_in_index(
            &index,
            &reference_uri,
            Position {
                line: 8,
                character: 47,
            },
        )
        .unwrap();
        assert_eq!(name, "hello_world");
        assert_eq!(local.uri, reference_uri);
        assert_eq!(definition.uri, definition_uri);
    }

    #[test]
    fn offers_route_attribute_action_for_unrouted_controller_method() {
        let uri = Url::parse("file:///tmp/project/src/Controller/DemoController.php").unwrap();
        let mut index = ProjectIndex::default();
        index.entities.push(Entity {
            kind: EntityKind::Class,
            name: "DemoController".into(),
            qualified_name: Some("App\\Controller\\DemoController".into()),
            uri: uri.clone(),
            range: Range {
                start: Position {
                    line: 2,
                    character: 14,
                },
                end: Position {
                    line: 2,
                    character: 28,
                },
            },
            references: Vec::new(),
            metadata: HashMap::new(),
            confidence: Confidence::Exact,
        });
        index.entities.push(Entity {
            kind: EntityKind::Method,
            name: "preview".into(),
            qualified_name: None,
            uri: uri.clone(),
            range: Range {
                start: Position {
                    line: 3,
                    character: 21,
                },
                end: Position {
                    line: 3,
                    character: 28,
                },
            },
            references: Vec::new(),
            metadata: HashMap::new(),
            confidence: Confidence::Likely,
        });
        let text = "<?php\nnamespace App\\Controller;\nfinal class DemoController {\n    public function preview(): Response {}\n}";
        let action = add_route_attribute_action(
            &index,
            &uri,
            Range {
                start: Position {
                    line: 3,
                    character: 21,
                },
                end: Position {
                    line: 3,
                    character: 28,
                },
            },
            text,
        )
        .unwrap();
        assert!(action.title.contains("demo.preview"));
    }

    #[test]
    fn offers_constructor_injection_for_known_service_property_use() {
        let source_uri = Url::parse("file:///tmp/src/MissingDependency.php").unwrap();
        let generator_uri = Url::parse("file:///tmp/src/HelloWorld/Generator.php").unwrap();
        let mut service_metadata = HashMap::new();
        service_metadata.insert("class".into(), "App\\HelloWorld\\Generator".into());
        let mut index = ProjectIndex::default();
        index.entities.push(Entity {
            kind: EntityKind::Class,
            name: "MissingDependency".into(),
            qualified_name: Some("App\\Demo\\MissingDependency".into()),
            uri: source_uri.clone(),
            range: Range {
                start: Position {
                    line: 2,
                    character: 6,
                },
                end: Position {
                    line: 2,
                    character: 23,
                },
            },
            references: Vec::new(),
            metadata: HashMap::new(),
            confidence: Confidence::Exact,
        });
        index.entities.push(Entity {
            kind: EntityKind::Class,
            name: "Generator".into(),
            qualified_name: Some("App\\HelloWorld\\Generator".into()),
            uri: generator_uri.clone(),
            range: Range::default(),
            references: Vec::new(),
            metadata: HashMap::new(),
            confidence: Confidence::Exact,
        });
        index.entities.push(Entity {
            kind: EntityKind::Service,
            name: "hello.world.generator".into(),
            qualified_name: None,
            uri: Url::parse("file:///tmp/config/services.yaml").unwrap(),
            range: Range::default(),
            references: Vec::new(),
            metadata: service_metadata,
            confidence: Confidence::Exact,
        });
        let text = "<?php\nnamespace App\\Demo;\nfinal class MissingDependency\n{\n    public function run(): string\n    {\n        return $this->generator->generate();\n    }\n}";
        let action = add_service_constructor_injection_action(
            &index,
            &source_uri,
            Range {
                start: Position {
                    line: 6,
                    character: 22,
                },
                end: Position {
                    line: 6,
                    character: 31,
                },
            },
            text,
        )
        .unwrap();
        assert!(action.title.contains("Generator"));
        let injection = action
            .edit
            .unwrap()
            .changes
            .unwrap()
            .values()
            .next()
            .unwrap()[0]
            .new_text
            .clone();
        assert!(injection.contains("App\\HelloWorld\\Generator"));
    }

    #[test]
    fn offers_common_controller_service_tag_action() {
        let uri = Url::parse("file:///tmp/project/config/services.yaml").unwrap();
        let mut metadata = HashMap::new();
        metadata.insert("class".into(), "App\\Controller\\DemoController".into());
        metadata.insert(
            "service_id".into(),
            "App\\Controller\\DemoController".into(),
        );
        let mut index = ProjectIndex::default();
        index.entities.push(Entity {
            kind: EntityKind::Service,
            name: "App\\Controller\\DemoController".into(),
            qualified_name: None,
            uri: uri.clone(),
            range: Range {
                start: Position {
                    line: 1,
                    character: 4,
                },
                end: Position {
                    line: 1,
                    character: 38,
                },
            },
            references: Vec::new(),
            metadata,
            confidence: Confidence::Exact,
        });
        let text = "services:\n    App\\Controller\\DemoController:\n        autowire: true\n    another.service:\n        class: App\\Other\n";
        let action = add_service_tag_action(
            &index,
            &uri,
            Range {
                start: Position {
                    line: 1,
                    character: 4,
                },
                end: Position {
                    line: 1,
                    character: 38,
                },
            },
            text,
        )
        .unwrap();
        assert!(action.title.contains("controller.service_arguments"));
    }

    #[test]
    fn template_rename_resolves_short_php_reference_and_preserves_compound_extension() {
        let root = std::env::temp_dir().join(format!(
            "symfony-template-short-rename-test-{}",
            std::process::id()
        ));
        let controller_path = root.join("src/Controller/HomeController.php");
        let template_path = root.join("templates/home/index.html.twig");
        std::fs::create_dir_all(controller_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(template_path.parent().unwrap()).unwrap();
        std::fs::write(
            &controller_path,
            "<?php\n$twig->render('index.html.twig');\n",
        )
        .unwrap();
        std::fs::write(&template_path, "<h1>Home</h1>").unwrap();
        let index = ProjectIndex::discover(&root);
        let controller_uri = Url::from_file_path(&controller_path).unwrap();
        let (name, local, definition) = rename_target_in_index(
            &index,
            &controller_uri,
            Position {
                line: 1,
                character: 28,
            },
        )
        .unwrap();
        assert_eq!(name, "home/index.html.twig");
        assert_eq!(local.name, "index.html.twig");

        let edit = rename_template(&index, &definition, &name, "details").unwrap();
        let Some(DocumentChanges::Operations(operations)) = edit.document_changes else {
            panic!("template rename should include edits and a file rename");
        };
        let DocumentChangeOperation::Edit(reference_edit) = &operations[0] else {
            panic!("template reference should be updated");
        };
        let OneOf::Left(reference_text_edit) = &reference_edit.edits[0] else {
            panic!("expected a plain text edit for the template reference");
        };
        assert_eq!(reference_text_edit.new_text, "home/details.html.twig");
        let DocumentChangeOperation::Op(ResourceOp::Rename(file_rename)) =
            operations.last().unwrap()
        else {
            panic!("template file should be renamed");
        };
        assert_eq!(
            file_rename.new_uri,
            Url::from_file_path(root.join("templates/home/details.html.twig")).unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn template_rename_updates_references_and_renames_file() {
        let old_uri =
            Url::from_file_path("/tmp/symfony-template-rename-test/templates/home/old.html.twig")
                .unwrap();
        let ref_uri = Url::from_file_path(
            "/tmp/symfony-template-rename-test/src/Controller/HomeController.php",
        )
        .unwrap();
        let mut index = ProjectIndex::with_root("/tmp/symfony-template-rename-test");
        index.entities.push(Entity {
            kind: EntityKind::TwigTemplate,
            name: "home/old.html.twig".into(),
            qualified_name: None,
            uri: old_uri,
            range: Range::default(),
            references: Vec::new(),
            metadata: HashMap::new(),
            confidence: Confidence::Exact,
        });
        let mut metadata = HashMap::new();
        metadata.insert("reference".into(), "true".into());
        index.entities.push(Entity {
            kind: EntityKind::TwigTemplate,
            name: "home/old.html.twig".into(),
            qualified_name: None,
            uri: ref_uri,
            range: Range {
                start: Position {
                    line: 4,
                    character: 30,
                },
                end: Position {
                    line: 4,
                    character: 48,
                },
            },
            references: Vec::new(),
            metadata,
            confidence: Confidence::Likely,
        });
        let definition = index.entities[0].clone();
        let edit =
            rename_template(&index, &definition, "home/old.html.twig", "new.html.twig").unwrap();
        assert!(matches!(
            edit.document_changes,
            Some(DocumentChanges::Operations(_))
        ));
    }

    #[test]
    fn reports_missing_route_parameters_but_honors_defaults() {
        let uri = Url::parse("file:///tmp/controller.php").unwrap();
        let mut route_metadata = HashMap::new();
        route_metadata.insert("route_name".into(), "user_show".into());
        route_metadata.insert("path".into(), "/users/{id}".into());
        let route = Entity {
            kind: EntityKind::Route,
            name: "user_show".into(),
            qualified_name: None,
            uri: Url::parse("file:///tmp/config/routes.yaml").unwrap(),
            range: Range::default(),
            references: Vec::new(),
            metadata: route_metadata,
            confidence: Confidence::Exact,
        };
        let mut reference_metadata = HashMap::new();
        reference_metadata.insert("reference".into(), "true".into());
        let reference = Entity {
            kind: EntityKind::Route,
            name: "user_show".into(),
            qualified_name: None,
            uri: uri.clone(),
            range: Range {
                start: Position {
                    line: 0,
                    character: 29,
                },
                end: Position {
                    line: 0,
                    character: 38,
                },
            },
            references: Vec::new(),
            metadata: reference_metadata,
            confidence: Confidence::Likely,
        };
        let mut index = ProjectIndex::default();
        index.entities.extend([route, reference]);
        index.entities[0]
            .metadata
            .insert("default:id".into(), "7".into());
        assert!(index_diagnostics(&index, &uri, "$urls->generate('user_show');").is_empty());
        index.entities[0].metadata.remove("default:id");
        let diagnostics = index_diagnostics(&index, &uri, "$urls->generate('user_show');");
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("requires parameter `id`"));
        assert!(
            index_diagnostics(&index, &uri, "$urls->generate('user_show', ['id' => 7]);")
                .is_empty()
        );
    }

    #[test]
    fn missing_template_action_creates_a_file_and_content() {
        let root = std::env::temp_dir().join(format!("symfony-lsp-phase5-{}", std::process::id()));
        let template_directory = root.join("templates/home");
        std::fs::create_dir_all(&template_directory).unwrap();
        let uri = Url::parse("file:///tmp/missing-template-reference.php").unwrap();
        let mut metadata = HashMap::new();
        metadata.insert("reference".into(), "true".into());
        let reference = Entity {
            kind: EntityKind::TwigTemplate,
            name: "home/generated.html.twig".into(),
            qualified_name: None,
            uri,
            range: Range::default(),
            references: Vec::new(),
            metadata,
            confidence: Confidence::Likely,
        };
        let index = ProjectIndex::with_root(&root);
        let action = create_template_action(
            &index,
            &reference,
            Diagnostic {
                message: "missing".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(action.title.contains("home/generated.html.twig"));
        assert!(matches!(
            action.edit.unwrap().document_changes,
            Some(DocumentChanges::Operations(_))
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reports_missing_template_references() {
        let uri = Url::parse("file:///tmp/templates/page.html.twig").unwrap();
        let mut metadata = HashMap::new();
        metadata.insert("reference".to_string(), "true".to_string());
        let mut index = ProjectIndex::default();
        index.entities.push(Entity {
            kind: EntityKind::TwigTemplate,
            name: "missing.html.twig".to_string(),
            qualified_name: None,
            uri: uri.clone(),
            range: Range {
                start: Position {
                    line: 2,
                    character: 10,
                },
                end: Position {
                    line: 2,
                    character: 26,
                },
            },
            references: Vec::new(),
            metadata,
            confidence: Confidence::Likely,
        });
        let diagnostics = index_diagnostics(&index, &uri, "");
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("Unknown Twig template"));
    }

    #[test]
    fn suppresses_diagnostics_for_confidently_non_symfony_projects() {
        let uri = Url::parse("file:///tmp/templates/page.html.twig").unwrap();
        let mut metadata = HashMap::new();
        metadata.insert("reference".to_string(), "true".to_string());
        let mut index = ProjectIndex::default();
        index.project.kind = ProjectKind::NotSymfony;
        index.entities.push(Entity {
            kind: EntityKind::TwigTemplate,
            name: "missing.html.twig".to_string(),
            qualified_name: None,
            uri: uri.clone(),
            range: Range::default(),
            references: Vec::new(),
            metadata,
            confidence: Confidence::Likely,
        });
        assert!(index_diagnostics(&index, &uri, "").is_empty());
    }

    #[test]
    fn reports_unknown_doctrine_relation_targets() {
        let uri = Url::parse("file:///tmp/config/doctrine/Article.orm.xml").unwrap();
        let mut metadata = HashMap::new();
        metadata.insert("reference".into(), "true".into());
        metadata.insert("owner".into(), "App\\Entity\\Article".into());
        let reference = Entity {
            kind: EntityKind::DoctrineEntity,
            name: "MissingAuthor".into(),
            qualified_name: Some("App\\Entity\\MissingAuthor".into()),
            uri: uri.clone(),
            range: Range::default(),
            references: Vec::new(),
            metadata,
            confidence: Confidence::Likely,
        };
        let mut index = ProjectIndex::default();
        index.entities.push(reference);
        let diagnostics = index_diagnostics(&index, &uri, "");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            Some(NumberOrString::String("missing-doctrine-entity".into()))
        );
    }
}
