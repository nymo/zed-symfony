use std::collections::HashSet;

use tower_lsp::lsp_types::*;

use crate::index::{Entity, EntityKind, ProjectIndex, ProjectKind};

use super::{entity_matches_name, entity_matches_symbol, template_reference_matches_definition};

pub(super) fn index_diagnostics(index: &ProjectIndex, uri: &Url, text: &str) -> Vec<Diagnostic> {
    // Suppress Symfony-specific warnings in a project that Composer clearly identifies as
    // non-Symfony, so unrelated PHP that happens to match a framework pattern stays quiet.
    if index.project.kind == ProjectKind::NotSymfony {
        return Vec::new();
    }
    index
        .entities()
        .iter()
        .filter(|reference| {
            reference.uri == *uri
                && reference
                    .metadata
                    .get("reference")
                    .is_some_and(|value| value == "true")
        })
        .filter_map(|reference| {
            let exists = index.entities().iter().any(|definition| {
                !definition
                    .metadata
                    .get("reference")
                    .is_some_and(|value| value == "true")
                    && if reference.kind == EntityKind::TwigTemplate {
                        definition.kind == EntityKind::TwigTemplate
                            && template_reference_matches_definition(&reference.name, definition)
                    } else if reference.kind == EntityKind::DoctrineEntity {
                        definition.kind == EntityKind::DoctrineEntity
                            && entity_matches_symbol(definition, reference, &reference.name)
                    } else {
                        entity_matches_symbol(definition, reference, &reference.name)
                    }
            });
            if exists {
                if reference.kind == EntityKind::Route {
                    return missing_route_parameter_diagnostic(index, uri, reference, text);
                }
                return None;
            }
            let (code, message) = match reference.kind {
                EntityKind::Route => (
                    "missing-route",
                    format!("Unknown Symfony route `{}`", reference.name),
                ),
                EntityKind::TwigTemplate => {
                    let line = text
                        .lines()
                        .nth(reference.range.start.line as usize)
                        .unwrap_or("");
                    if line.to_ascii_lowercase().contains("extends") {
                        (
                            "missing-template",
                            format!("Unknown Twig parent template `{}`", reference.name),
                        )
                    } else {
                        (
                            "missing-template",
                            format!("Unknown Twig template `{}`", reference.name),
                        )
                    }
                }
                EntityKind::EnvironmentVariable => (
                    "missing-environment-variable",
                    format!("Unknown environment variable `{}`", reference.name),
                ),
                EntityKind::Service => (
                    "missing-service",
                    format!("Unknown Symfony service `{}`", reference.name),
                ),
                EntityKind::TranslationKey => (
                    "missing-translation",
                    format!("Unknown translation key `{}`", reference.name),
                ),
                EntityKind::DoctrineEntity => (
                    "missing-doctrine-entity",
                    format!("Unknown Doctrine entity `{}`", reference.name),
                ),
                _ => return None,
            };
            Some(Diagnostic {
                range: reference.range,
                severity: Some(DiagnosticSeverity::WARNING),
                code: Some(NumberOrString::String(code.to_string())),
                source: Some("symfony".into()),
                message,
                ..Default::default()
            })
        })
        .collect()
}

pub(super) fn missing_route_parameter_diagnostic(
    index: &ProjectIndex,
    _uri: &Url,
    reference: &Entity,
    text: &str,
) -> Option<Diagnostic> {
    let routes = index
        .entities()
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Route
                && !entity
                    .metadata
                    .get("reference")
                    .is_some_and(|value| value == "true")
                && entity_matches_name(entity, &reference.name)
        })
        .collect::<Vec<_>>();
    if routes.len() != 1 {
        return None;
    }
    let route = routes[0];
    let path = route
        .metadata
        .get("path")
        .map(String::as_str)
        .unwrap_or(&route.name);
    let parameters = extract_route_parameters(path);
    if parameters.is_empty() {
        return None;
    }
    let line_number = reference.range.start.line as usize;
    let line = text.lines().nth(line_number)?;
    let provided = provided_named_arguments(line, &reference.name);
    let missing = parameters.into_iter().find(|parameter| {
        !provided.contains(parameter)
            && !route.metadata.contains_key(&format!("default:{parameter}"))
    })?;
    Some(Diagnostic {
        range: reference.range,
        severity: Some(DiagnosticSeverity::WARNING),
        code: Some(NumberOrString::String("missing-route-parameter".into())),
        source: Some("symfony".into()),
        message: format!("Route `{}` requires parameter `{missing}`", reference.name),
        ..Default::default()
    })
}

pub(super) fn extract_route_parameters(path: &str) -> Vec<String> {
    path.split('{')
        .skip(1)
        .filter_map(|part| part.split('}').next())
        .map(|parameter| parameter.split('<').next().unwrap_or(parameter))
        .filter(|parameter| !parameter.is_empty())
        .map(str::to_string)
        .collect()
}

pub(super) fn provided_named_arguments(line: &str, route_name: &str) -> HashSet<String> {
    let Some(route_start) = line.find(route_name) else {
        return HashSet::new();
    };
    let mut tail_start = route_start + route_name.len();
    if line
        .as_bytes()
        .get(tail_start)
        .is_some_and(|byte| matches!(byte, b'\'' | b'"'))
    {
        tail_start += 1;
    }
    let tail = &line[tail_start..];
    let bytes = tail.as_bytes();
    let mut arguments = HashSet::new();
    let mut index = 0;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"') {
            let quote = bytes[index];
            let start = index + 1;
            let Some(relative_end) = bytes[start..].iter().position(|byte| *byte == quote) else {
                break;
            };
            let end = start + relative_end;
            let mut next = end + 1;
            while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                next += 1;
            }
            if tail[next..].starts_with(':') || tail[next..].starts_with("=>") {
                arguments.insert(tail[start..end].to_string());
            }
            index = end + 1;
        } else if bytes[index].is_ascii_alphabetic() || bytes[index] == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            let mut next = index;
            while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                next += 1;
            }
            if next < bytes.len()
                && bytes[next] == b':'
                && (next + 1 == bytes.len() || bytes[next + 1] != b':')
            {
                arguments.insert(tail[start..index].to_string());
            }
        } else {
            index += 1;
        }
    }
    arguments
}
