use std::collections::HashMap;
use std::path::{Component, Path};

use tower_lsp::lsp_types::*;

use crate::index::{Entity, EntityKind, ProjectIndex};

use super::entity_matches_name;

pub(super) fn build_code_actions(
    index: &ProjectIndex,
    uri: &Url,
    range: Range,
    diagnostics: &[Diagnostic],
    text: &str,
) -> Option<CodeActionResponse> {
    let mut actions = Vec::new();
    for diagnostic in diagnostics {
        if diagnostic.code == Some(NumberOrString::String("missing-template".into())) {
            if let Some(reference) = index.entities().iter().find(|entity| {
                entity.uri == *uri
                    && entity.kind == EntityKind::TwigTemplate
                    && entity
                        .metadata
                        .get("reference")
                        .is_some_and(|value| value == "true")
                    && entity.range == diagnostic.range
            }) {
                if let Some(action) = create_template_action(index, reference, diagnostic.clone()) {
                    actions.push(CodeActionOrCommand::CodeAction(action));
                }
            }
        }
    }
    if let Some(action) = add_route_attribute_action(index, uri, range, text) {
        actions.push(CodeActionOrCommand::CodeAction(action));
    }
    if let Some(action) = add_service_constructor_injection_action(index, uri, range, text) {
        actions.push(CodeActionOrCommand::CodeAction(action));
    }
    if let Some(action) = add_service_tag_action(index, uri, range, text) {
        actions.push(CodeActionOrCommand::CodeAction(action));
    }
    if actions.is_empty() {
        None
    } else {
        Some(actions)
    }
}

pub(super) fn create_template_action(
    index: &ProjectIndex,
    reference: &Entity,
    diagnostic: Diagnostic,
) -> Option<CodeAction> {
    let relative = Path::new(&reference.name);
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let path = index.root.join("templates").join(relative);
    if path.exists() || !path.parent()?.is_dir() {
        return None;
    }
    let uri = Url::from_file_path(&path).ok()?;
    let template = "{% extends 'base.html.twig' %}\n\n{% block body %}\n{% endblock %}\n";
    let operations = vec![
        DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
            uri: uri.clone(),
            options: Some(CreateFileOptions {
                overwrite: Some(false),
                ignore_if_exists: Some(false),
            }),
            annotation_id: None,
        })),
        DocumentChangeOperation::Edit(TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier { uri, version: None },
            edits: vec![OneOf::Left(TextEdit {
                range: Range::default(),
                new_text: template.into(),
            })],
        }),
    ];
    Some(CodeAction {
        title: format!("Create Twig template `{}`", reference.name),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic]),
        edit: Some(WorkspaceEdit {
            document_changes: Some(DocumentChanges::Operations(operations)),
            ..Default::default()
        }),
        command: None,
        is_preferred: Some(true),
        disabled: None,
        data: None,
    })
}

pub(super) fn add_route_attribute_action(
    index: &ProjectIndex,
    uri: &Url,
    range: Range,
    text: &str,
) -> Option<CodeAction> {
    if !uri.path().contains("/Controller/") {
        return None;
    }
    let method = index.entities().iter().find(|entity| {
        entity.uri == *uri
            && entity.kind == EntityKind::Method
            && entity.range.start.line >= range.start.line
            && entity.range.start.line <= range.end.line
    })?;
    let line_number = method.range.start.line as usize;
    let class = index
        .entities()
        .iter()
        .filter(|entity| {
            entity.uri == *uri
                && matches!(entity.kind, EntityKind::Class | EntityKind::Interface)
                && entity.range.start.line <= method.range.start.line
        })
        .max_by_key(|entity| entity.range.start.line)?;
    let lines = text.lines().collect::<Vec<_>>();
    let source_line = *lines.get(line_number)?;
    if !source_line.trim_start().starts_with("public function ") || method.name == "__construct" {
        return None;
    }
    if lines.iter().take(line_number).rev().take(2).any(|line| {
        line.contains("#[Route")
            || line.contains("#[\\Symfony\\Component\\Routing\\Attribute\\Route")
            || line.contains("@Route")
    }) {
        return None;
    }
    let class_name = class.qualified_name.as_deref().unwrap_or(&class.name);
    if index
        .route_config_for_controller(class_name)
        .iter()
        .any(|route| {
            route
                .metadata
                .get("controller")
                .and_then(|target| target.rsplit_once("::").map(|(_, name)| name))
                == Some(method.name.as_str())
        })
    {
        return None;
    }
    let class_slug = class
        .name
        .strip_suffix("Controller")
        .unwrap_or(&class.name)
        .to_ascii_lowercase()
        .replace('_', "-");
    let method_slug = method.name.to_ascii_lowercase().replace('_', "-");
    let route_name = format!("{}.{}", class_slug.replace('-', "_"), method.name);
    if index
        .entities()
        .iter()
        .any(|entity| entity.kind == EntityKind::Route && entity_matches_name(entity, &route_name))
    {
        return None;
    }
    let indent = source_line.len() - source_line.trim_start().len();
    let attribute = format!("{}#[\\Symfony\\Component\\Routing\\Attribute\\Route('/{class_slug}/{method_slug}', name: '{route_name}')]\n", " ".repeat(indent));
    Some(CodeAction {
        title: format!("Add route attribute `{route_name}`"),
        kind: Some(CodeActionKind::REFACTOR_REWRITE),
        diagnostics: None,
        edit: Some(WorkspaceEdit {
            changes: Some(HashMap::from([(
                (*uri).clone(),
                vec![TextEdit {
                    range: Range {
                        start: Position {
                            line: line_number as u32,
                            character: 0,
                        },
                        end: Position {
                            line: line_number as u32,
                            character: 0,
                        },
                    },
                    new_text: attribute,
                }],
            )])),
            ..Default::default()
        }),
        command: None,
        is_preferred: None,
        disabled: None,
        data: None,
    })
}

pub(super) fn add_service_constructor_injection_action(
    index: &ProjectIndex,
    uri: &Url,
    range: Range,
    text: &str,
) -> Option<CodeAction> {
    let lines = text.lines().collect::<Vec<_>>();
    let line_number = range.start.line as usize;
    let line = *lines.get(line_number)?;
    let after_this = line.find("$this->")? + "$this->".len();
    let suffix = &line[after_this..];
    let property_length = suffix
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        .count();
    if property_length == 0 {
        return None;
    }
    let property = &suffix[..property_length];
    let class = index
        .entities()
        .iter()
        .filter(|entity| {
            entity.uri == *uri
                && entity.kind == EntityKind::Class
                && entity.range.start.line <= line_number as u32
        })
        .max_by_key(|entity| entity.range.start.line)?;
    let wanted_short_name = format!("{}{}", property[..1].to_ascii_uppercase(), &property[1..]);
    let dependencies = index
        .entities()
        .iter()
        .filter(|entity| {
            matches!(entity.kind, EntityKind::Class | EntityKind::Interface)
                && entity.name == wanted_short_name
        })
        .collect::<Vec<_>>();
    if dependencies.len() != 1 {
        return None;
    }
    let dependency = dependencies[0];
    let dependency_name = dependency
        .qualified_name
        .as_deref()
        .unwrap_or(&dependency.name);
    let is_service = index.entities().iter().any(|service| {
        matches!(service.kind, EntityKind::Service | EntityKind::ServiceAlias)
            && (service.name == dependency_name
                || service
                    .metadata
                    .get("class")
                    .is_some_and(|class_name| class_name == dependency_name))
    }) || index.entities().iter().any(|service| {
        service.kind == EntityKind::Service
            && service.name.ends_with('\\')
            && service.metadata.contains_key("resource")
            && dependency_name.starts_with(&service.name)
    });
    if !is_service {
        return None;
    }
    if class
        .metadata
        .get("constructor_dependencies")
        .is_some_and(|dependencies| dependencies.lines().any(|name| name == dependency_name))
    {
        return None;
    }
    if index.entities().iter().any(|entity| {
        entity.uri == *uri && entity.kind == EntityKind::Property && entity.name == property
    }) {
        return None;
    }
    let constructor = index.entities().iter().find(|entity| {
        entity.uri == *uri && entity.kind == EntityKind::Method && entity.name == "__construct"
    });
    let edit = if let Some(constructor) = constructor {
        let constructor_line = constructor.range.start.line as usize;
        let source = *lines.get(constructor_line)?;
        let open = source.find("__construct(")? + "__construct(".len();
        let close = source[open..].find(')')? + open;
        if source[open..close].contains(&format!("${property}")) {
            return None;
        }
        let args = source[open..close].trim();
        let separator = if args.is_empty() { "" } else { ", " };
        let insertion = format!("{separator}private readonly \\{dependency_name} ${property}");
        TextEdit {
            range: Range {
                start: Position {
                    line: constructor_line as u32,
                    character: source[..close].encode_utf16().count() as u32,
                },
                end: Position {
                    line: constructor_line as u32,
                    character: source[..close].encode_utf16().count() as u32,
                },
            },
            new_text: insertion,
        }
    } else {
        let class_line = class.range.start.line as usize;
        let (open_line, source) = lines
            .iter()
            .enumerate()
            .skip(class_line)
            .find(|(_, line)| line.contains('{'))?;
        let indent = source.len() - source.trim_start().len() + 4;
        TextEdit {
            range: Range {
                start: Position { line: (open_line + 1) as u32, character: 0 },
                end: Position { line: (open_line + 1) as u32, character: 0 },
            },
            new_text: format!("{}public function __construct(private readonly \\{dependency_name} ${property}) {{}}\n", " ".repeat(indent)),
        }
    };
    Some(CodeAction {
        title: format!("Inject `{}` through the constructor", dependency.name),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: None,
        edit: Some(WorkspaceEdit {
            changes: Some(HashMap::from([((*uri).clone(), vec![edit])])),
            ..Default::default()
        }),
        command: None,
        is_preferred: None,
        disabled: None,
        data: None,
    })
}

pub(super) fn add_service_tag_action(
    index: &ProjectIndex,
    uri: &Url,
    range: Range,
    text: &str,
) -> Option<CodeAction> {
    if !uri.path().contains("services") || !uri.path().ends_with(".yaml") {
        return None;
    }
    let service = index.entities().iter().find(|entity| {
        entity.uri == *uri
            && entity.kind == EntityKind::Service
            && entity.range.start.line >= range.start.line
            && entity.range.start.line <= range.end.line
    })?;
    let class = service.metadata.get("class").unwrap_or(&service.name);
    let (tag, description) = if class.ends_with("Controller") {
        (
            "controller.service_arguments",
            "controller service argument wiring",
        )
    } else if class.ends_with("Command") {
        ("console.command", "console command discovery")
    } else if class.ends_with("EventSubscriber") {
        ("kernel.event_subscriber", "event subscriber discovery")
    } else {
        return None;
    };
    if index.entities().iter().any(|entity| {
        entity.kind == EntityKind::ServiceTag
            && entity.name == tag
            && entity
                .metadata
                .get("service_id")
                .is_some_and(|service_id| service_id == &service.name)
    }) {
        return None;
    }
    let lines = text.lines().collect::<Vec<_>>();
    let service_line = service.range.start.line as usize;
    let block_end = (service_line + 1..lines.len())
        .find(|line_number| {
            let line = lines[*line_number];
            !line.trim().is_empty()
                && !line.trim_start().starts_with('#')
                && line.len() - line.trim_start().len() == 4
        })
        .unwrap_or(lines.len());
    if lines[service_line + 1..block_end]
        .iter()
        .any(|line| line.trim_start().starts_with("tags:"))
    {
        return None;
    }
    let newline = block_end as u32;
    let insertion = "        tags:\n            - '".to_string() + tag + "'\n";
    Some(CodeAction {
        title: format!("Add `{tag}` tag for {description}"),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: None,
        edit: Some(WorkspaceEdit {
            changes: Some(HashMap::from([(
                (*uri).clone(),
                vec![TextEdit {
                    range: Range {
                        start: Position {
                            line: newline,
                            character: 0,
                        },
                        end: Position {
                            line: newline,
                            character: 0,
                        },
                    },
                    new_text: insertion,
                }],
            )])),
            ..Default::default()
        }),
        command: None,
        is_preferred: None,
        disabled: None,
        data: None,
    })
}
