use std::collections::HashMap;
use std::path::Path;

use tower_lsp::lsp_types::{Position, Range, Url};

use super::{doctrine, entity, forms_twig, route_parameters, Confidence, Entity, EntityKind};

pub(super) fn parse_file(root: &Path, path: &Path, text: &str) -> Vec<Entity> {
    let Ok(uri) = Url::from_file_path(path) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    let mut namespace = String::new();
    let mut imports = HashMap::new();
    let mut current_class: Option<usize> = None;
    let mut current_route: Option<usize> = None;
    let mut route_defaults_indent: Option<usize> = None;
    let mut current_service: Option<usize> = None;
    let translation_xml = matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("xml" | "xlf")
    ) && path.to_string_lossy().contains("translations");
    let service_xml = path.extension().and_then(|e| e.to_str()) == Some("xml")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains("services"));
    let service_php = extension_is_php_services(path);
    let service_yaml = matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yaml" | "yml")
    ) && path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("services."));
    let route_yaml = matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yaml" | "yml")
    ) && path.to_string_lossy().contains("routes");
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    for (line_number, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".env"))
        {
            if let Some(reference) = environment_reference(trimmed) {
                push_reference(
                    &mut result,
                    EntityKind::EnvironmentVariable,
                    reference,
                    &uri,
                    line_number,
                    line,
                );
            }
        }
        if extension == "php" {
            if let Some(value) = trimmed
                .strip_prefix("namespace ")
                .and_then(|v| v.strip_suffix(';'))
            {
                namespace = value.trim().to_string();
            }
            if let Some(import) = trimmed
                .strip_prefix("use ")
                .and_then(|value| value.strip_suffix(';'))
            {
                let (class, alias) = import
                    .split_once(" as ")
                    .map(|(class, alias)| (class.trim(), alias.trim().to_string()))
                    .unwrap_or_else(|| {
                        let class = import.trim();
                        (
                            class,
                            class.rsplit('\\').next().unwrap_or(class).to_string(),
                        )
                    });
                imports.insert(alias, class.trim_start_matches('\\').to_string());
            }
            if let Some((kind, name)) =
                declaration(trimmed, "class").or_else(|| declaration(trimmed, "interface"))
            {
                let qualified = if namespace.is_empty() {
                    name.clone()
                } else {
                    format!("{namespace}\\{name}")
                };
                let entity_kind = if kind == "interface" {
                    EntityKind::Interface
                } else {
                    EntityKind::Class
                };
                result.push(entity(
                    entity_kind,
                    name,
                    Some(qualified),
                    &uri,
                    line_number,
                    line,
                    Confidence::Exact,
                ));
                current_class = Some(result.len() - 1);
            } else if let Some(name) = declaration(trimmed, "function").map(|(_, n)| n) {
                if name == "__construct" {
                    if let Some(class_index) = current_class {
                        let dependencies = constructor_dependencies(trimmed, &namespace, &imports);
                        if !dependencies.is_empty() {
                            if let Some(class) = result.get_mut(class_index) {
                                class.metadata.insert(
                                    "constructor_dependencies".into(),
                                    dependencies.join("\n"),
                                );
                            }
                        }
                    }
                }
                result.push(entity(
                    EntityKind::Method,
                    name,
                    None,
                    &uri,
                    line_number,
                    line,
                    Confidence::Likely,
                ));
            } else if let Some(name) = php_property(trimmed) {
                result.push(entity(
                    EntityKind::Property,
                    name,
                    None,
                    &uri,
                    line_number,
                    line,
                    Confidence::Likely,
                ));
            }
            if lower.contains("#[route(") || lower.contains("@route(") {
                if let Some(name) = quoted_or_path(trimmed) {
                    let mut route = entity(
                        EntityKind::Route,
                        name,
                        None,
                        &uri,
                        line_number,
                        line,
                        Confidence::Likely,
                    );
                    if let Some(route_name) = route_name(trimmed) {
                        if let Some(range) = named_string_range(line, "name:", line_number) {
                            route.range = range;
                            route.metadata.insert(
                                "route_name_range".into(),
                                format!(
                                    "{}:{}:{}",
                                    range.start.line,
                                    range.start.character,
                                    range.end.character - range.start.character
                                ),
                            );
                        }
                        route.metadata.insert("route_name".into(), route_name);
                    }
                    if let Some(parameters) = route_parameters(&route.name) {
                        route.metadata.insert("parameters".into(), parameters);
                    }
                    result.push(route);
                }
            }
            if let Some(reference) = call_string_argument(
                trimmed,
                &[
                    "path(",
                    "url(",
                    "redirecttoroute(",
                    "generate(",
                    "generateurl(",
                ],
            ) {
                push_reference(
                    &mut result,
                    EntityKind::Route,
                    reference,
                    &uri,
                    line_number,
                    line,
                );
            }
            if let Some(reference) =
                call_string_argument(trimmed, &["render(", "include(", "extends(", "embed("])
            {
                push_reference(
                    &mut result,
                    EntityKind::TwigTemplate,
                    reference,
                    &uri,
                    line_number,
                    line,
                );
            }
            if lower.contains("trans(") {
                if let Some(reference) = call_string_argument(trimmed, &["trans("]) {
                    push_reference(
                        &mut result,
                        EntityKind::TranslationKey,
                        reference,
                        &uri,
                        line_number,
                        line,
                    );
                }
            }
            if lower.contains("autowire(") {
                if let Some(reference) = autowire_service_reference(trimmed) {
                    push_reference(
                        &mut result,
                        EntityKind::Service,
                        reference,
                        &uri,
                        line_number,
                        line,
                    );
                }
            }
            if service_php {
                parse_php_service_line(&mut result, &uri, line_number, line, trimmed);
            }
            if lower.contains("ascommand(") {
                if let Some(name) = quoted_or_path(trimmed) {
                    result.push(entity(
                        EntityKind::ConsoleCommand,
                        name,
                        None,
                        &uri,
                        line_number,
                        line,
                        Confidence::Likely,
                    ));
                }
            }
        } else if extension == "twig" {
            if line_number == 0 {
                let name = path
                    .strip_prefix(root.join("templates"))
                    .or_else(|_| path.strip_prefix(root))
                    .unwrap_or(path)
                    .to_string_lossy()
                    .trim_start_matches('/')
                    .replace('\\', "/");
                result.push(entity(
                    EntityKind::TwigTemplate,
                    name,
                    None,
                    &uri,
                    0,
                    line,
                    Confidence::Exact,
                ));
            }
            let lower_line = trimmed.to_ascii_lowercase();
            if let Some(reference) =
                call_string_argument(trimmed, &["include(", "extends(", "embed("]).or_else(|| {
                    (lower_line.contains("{% include")
                        || lower_line.contains("{% extends")
                        || lower_line.contains("{% embed"))
                    .then(|| quoted_or_path(trimmed))
                    .flatten()
                })
            {
                push_reference(
                    &mut result,
                    EntityKind::TwigTemplate,
                    reference,
                    &uri,
                    line_number,
                    line,
                );
            }
            if let Some(reference) = call_string_argument(trimmed, &["path(", "url("]) {
                push_reference(
                    &mut result,
                    EntityKind::Route,
                    reference,
                    &uri,
                    line_number,
                    line,
                );
            }
            if lower_line.contains("|trans") || lower_line.contains("trans(") {
                if let Some(reference) = quoted_or_path(trimmed) {
                    push_reference(
                        &mut result,
                        EntityKind::TranslationKey,
                        reference,
                        &uri,
                        line_number,
                        line,
                    );
                }
            }
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(".env"))
        {
            if let Some((name, _)) = trimmed.split_once('=') {
                if !name.is_empty() {
                    result.push(entity(
                        EntityKind::EnvironmentVariable,
                        name.trim().to_string(),
                        None,
                        &uri,
                        line_number,
                        line,
                        Confidence::Exact,
                    ));
                }
            }
        } else if service_yaml {
            let indent = line.len() - line.trim_start().len();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((key, value)) = trimmed.split_once(':') else {
                continue;
            };
            let key = key.trim().trim_matches('"').trim_matches('\'');
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if indent == 4 {
                current_service = None;
                if key.starts_with('_') || key.starts_with('-') {
                    continue;
                }
                let mut service = entity(
                    EntityKind::Service,
                    key.to_string(),
                    None,
                    &uri,
                    line_number,
                    line,
                    Confidence::Exact,
                );
                service
                    .metadata
                    .insert("service_id".into(), key.to_string());
                if key.contains('\\') {
                    service.metadata.insert("class".into(), key.to_string());
                }
                result.push(service);
                current_service = Some(result.len() - 1);
            } else if indent > 4 {
                if let Some(reference) = service_reference(trimmed) {
                    push_reference(
                        &mut result,
                        EntityKind::Service,
                        reference,
                        &uri,
                        line_number,
                        line,
                    );
                }
                let service_index = current_service;
                let alias_reference = (key == "alias").then(|| value.to_string());
                if let Some(service_index) = service_index {
                    if let Some(service) = result.get_mut(service_index) {
                        match key {
                            "class" => {
                                service.metadata.insert("class".into(), value.to_string());
                            }
                            "resource" if service.name.ends_with('\\') => {
                                service
                                    .metadata
                                    .insert("resource".into(), value.to_string());
                            }
                            "alias" => {
                                service.kind = EntityKind::ServiceAlias;
                                service.metadata.insert("alias".into(), value.to_string());
                            }
                            _ => {}
                        }
                    }
                    let service_id = result
                        .get(service_index)
                        .map(|service| service.name.clone())
                        .unwrap_or_default();
                    if let Some(alias) = alias_reference {
                        push_reference(
                            &mut result,
                            EntityKind::Service,
                            alias,
                            &uri,
                            line_number,
                            line,
                        );
                    }
                    if key == "tags" {
                        for tag in yaml_quoted_values(value) {
                            let mut item = entity(
                                EntityKind::ServiceTag,
                                tag,
                                None,
                                &uri,
                                line_number,
                                line,
                                Confidence::Likely,
                            );
                            item.metadata
                                .insert("service_id".into(), service_id.clone());
                            result.push(item);
                        }
                    } else if key == "- name" {
                        let tag = value.trim_matches('"').trim_matches('\'');
                        let mut item = entity(
                            EntityKind::ServiceTag,
                            tag.to_string(),
                            None,
                            &uri,
                            line_number,
                            line,
                            Confidence::Likely,
                        );
                        item.metadata.insert("service_id".into(), service_id);
                        result.push(item);
                    }
                }
            }
        } else if translation_xml {
            if line.contains("<trans-unit") {
                if let Some(key) = xml_attribute(line, "id") {
                    let mut item = entity(
                        EntityKind::TranslationKey,
                        key,
                        None,
                        &uri,
                        line_number,
                        line,
                        Confidence::Likely,
                    );
                    if let Some(file_name) = path.file_name().and_then(|name| name.to_str()) {
                        if let Some(domain) = file_name.split('.').next() {
                            item.metadata.insert("domain".into(), domain.to_string());
                        }
                    }
                    result.push(item);
                }
            }
        } else if service_xml {
            parse_xml_service_line(&mut result, &uri, line_number, line);
        } else if route_yaml {
            let indent = line.len() - line.trim_start().len();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = trimmed.split_once(':') {
                let key = key.trim().trim_matches('"').trim_matches('\'');
                let value = value.trim();
                if indent == 0 {
                    route_defaults_indent = None;
                    let mut route = entity(
                        EntityKind::Route,
                        key.to_string(),
                        None,
                        &uri,
                        line_number,
                        line,
                        Confidence::Exact,
                    );
                    route.metadata.insert("route_name".into(), key.to_string());
                    if !value.is_empty() {
                        route.metadata.insert("value".into(), value.to_string());
                    }
                    result.push(route);
                    current_route = Some(result.len() - 1);
                } else if let Some(route_index) = current_route {
                    if key == "defaults" {
                        route_defaults_indent = Some(indent);
                    } else if route_defaults_indent
                        .is_some_and(|defaults_indent| indent > defaults_indent)
                    {
                        if let Some(route) = result.get_mut(route_index) {
                            route.metadata.insert(
                                format!("default:{key}"),
                                value.trim_matches('"').trim_matches('\'').to_string(),
                            );
                        }
                    } else {
                        route_defaults_indent = None;
                        if let Some(route) = result.get_mut(route_index) {
                            match key {
                                "controller" => {
                                    route.metadata.insert(
                                        "controller".into(),
                                        value.trim_matches('"').trim_matches('\'').to_string(),
                                    );
                                }
                                "path" => {
                                    route.metadata.insert(
                                        "path".into(),
                                        value.trim_matches('"').trim_matches('\'').to_string(),
                                    );
                                }
                                "resource" => {
                                    route.metadata.insert(
                                        "resource".into(),
                                        value.trim_matches('"').trim_matches('\'').to_string(),
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        } else if extension == "yaml" || extension == "yml" {
            if let Some((key, value)) = trimmed.split_once(':') {
                let name = key.trim().trim_matches('"').trim_matches('\'');
                if name.starts_with('_')
                    || name == "services"
                    || name == "parameters"
                    || name == "routes"
                {
                    continue;
                }
                let path_text = path.to_string_lossy();
                let kind = if path_text.contains("translations") {
                    EntityKind::TranslationKey
                } else if lower.contains("resource:")
                    || lower.contains("path:")
                    || path_text.contains("routes")
                {
                    EntityKind::Route
                } else if path_text.contains("services")
                    && (lower.contains("alias:") || lower.contains("alias"))
                {
                    EntityKind::ServiceAlias
                } else if path_text.contains("services")
                    && (lower.contains("tags:") || lower.contains("- name:"))
                {
                    EntityKind::ServiceTag
                } else if path_text.contains("services") {
                    EntityKind::Service
                } else {
                    EntityKind::Parameter
                };
                let mut item = entity(
                    kind,
                    name.to_string(),
                    None,
                    &uri,
                    line_number,
                    line,
                    Confidence::Likely,
                );
                item.metadata.insert("value".into(), value.trim().into());
                let value_text = value.trim().trim_matches('"').trim_matches('\'');
                if let Some(resource) = value_text.strip_prefix("resource:") {
                    item.metadata.insert(
                        "resource".into(),
                        resource
                            .trim()
                            .trim_matches('"')
                            .trim_matches('\'')
                            .to_string(),
                    );
                } else if (name == "resource" || name == "path") && value_text.contains("src/") {
                    item.metadata
                        .insert("resource".into(), value_text.to_string());
                }
                if item.kind == EntityKind::TranslationKey {
                    if let Some(file_name) = path.file_name().and_then(|name| name.to_str()) {
                        if let Some(domain) = file_name.split('.').next() {
                            item.metadata.insert("domain".into(), domain.to_string());
                        }
                    }
                }
                result.push(item);
            }
        } else if extension == "json"
            && path.file_name().and_then(|n| n.to_str()) == Some("composer.json")
            && lower.contains("autoload")
        {
            result.push(entity(
                EntityKind::Parameter,
                "composer.autoload".into(),
                None,
                &uri,
                line_number,
                line,
                Confidence::Exact,
            ));
        }
    }
    result.extend(doctrine::parse(root, path, text, &uri));
    result.extend(forms_twig::parse(root, path, text, &uri));
    result
}

fn extension_is_php_services(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("php")
        && path.to_string_lossy().contains("services")
}

fn yaml_quoted_values(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut quote = None;
    let mut current = String::new();
    for character in value.chars() {
        if quote == Some(character) {
            values.push(std::mem::take(&mut current));
            quote = None;
        } else if quote.is_none() && matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if quote.is_some() {
            current.push(character);
        }
    }
    values
}

fn xml_attribute(line: &str, name: &str) -> Option<String> {
    let marker = format!("{name}=");
    let start = line.find(&marker)? + marker.len();
    let tail = line[start..].trim_start();
    let quote = tail.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    let end = tail[1..].find(quote)? + 1;
    Some(tail[1..end].to_string())
}

fn parse_xml_service_line(entities: &mut Vec<Entity>, uri: &Url, line_number: usize, line: &str) {
    if line.contains("<service") {
        if let Some(id) = xml_attribute(line, "id") {
            let alias = xml_attribute(line, "alias");
            let class = xml_attribute(line, "class");
            let kind = if alias.is_some() {
                EntityKind::ServiceAlias
            } else {
                EntityKind::Service
            };
            let mut service = entity(kind, id, None, uri, line_number, line, Confidence::Exact);
            if let Some(class) = class {
                service.metadata.insert("class".into(), class);
            }
            if let Some(alias) = alias {
                service.metadata.insert("alias".into(), alias.clone());
                push_reference(entities, EntityKind::Service, alias, uri, line_number, line);
            }
            entities.push(service);
        }
    }
    if line.contains("<argument") && xml_attribute(line, "type").as_deref() == Some("service") {
        if let Some(id) = xml_attribute(line, "id") {
            push_reference(entities, EntityKind::Service, id, uri, line_number, line);
        }
    }
    if line.contains("<tag") {
        if let Some(name) = xml_attribute(line, "name") {
            entities.push(entity(
                EntityKind::ServiceTag,
                name,
                None,
                uri,
                line_number,
                line,
                Confidence::Likely,
            ));
        }
    }
}

fn parse_php_service_line(
    entities: &mut Vec<Entity>,
    uri: &Url,
    line_number: usize,
    line: &str,
    trimmed: &str,
) {
    let lower = trimmed.to_ascii_lowercase();
    if let Some(start) = lower.find("->alias(") {
        let values = yaml_quoted_values(&trimmed[start + "->alias(".len()..]);
        if let Some(alias_id) = values.first() {
            let mut alias = entity(
                EntityKind::ServiceAlias,
                alias_id.clone(),
                None,
                uri,
                line_number,
                line,
                Confidence::Exact,
            );
            if let Some(target) = values.get(1) {
                alias.metadata.insert("alias".into(), target.clone());
                push_reference(
                    entities,
                    EntityKind::Service,
                    target.clone(),
                    uri,
                    line_number,
                    line,
                );
            }
            entities.push(alias);
        }
    } else if let Some(start) = lower.find("->set(") {
        let tail = &trimmed[start + "->set(".len()..];
        let values = yaml_quoted_values(tail);
        let class = tail
            .split("::class")
            .next()
            .and_then(|part| {
                part.rsplit(|character: char| {
                    !character.is_ascii_alphanumeric() && character != '_' && character != '\\'
                })
                .next()
            })
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        let id = values.first().cloned().or_else(|| class.clone());
        if let Some(id) = id {
            let mut service = entity(
                EntityKind::Service,
                id,
                None,
                uri,
                line_number,
                line,
                Confidence::Exact,
            );
            if let Some(class) = class {
                service.metadata.insert("class".into(), class);
            }
            entities.push(service);
        }
    }
    if let Some(reference) = call_string_argument(trimmed, &["service("]) {
        push_reference(
            entities,
            EntityKind::Service,
            reference,
            uri,
            line_number,
            line,
        );
    }
    if let Some(start) = lower.find("->tag(") {
        if let Some(tag) = quoted_or_path(&trimmed[start + "->tag(".len()..]) {
            entities.push(entity(
                EntityKind::ServiceTag,
                tag,
                None,
                uri,
                line_number,
                line,
                Confidence::Likely,
            ));
        }
    }
}

fn named_string_range(source: &str, marker: &str, line: usize) -> Option<Range> {
    let marker_start = source.find(marker)? + marker.len();
    let tail = &source[marker_start..];
    let quote_offset = tail.find(['\'', '"'])?;
    let quote = tail.as_bytes()[quote_offset] as char;
    let value_start = marker_start + quote_offset + 1;
    let value_length = source[value_start..].find(quote)?;
    Some(Range {
        start: Position {
            line: line as u32,
            character: source[..value_start].encode_utf16().count() as u32,
        },
        end: Position {
            line: line as u32,
            character: source[..value_start + value_length].encode_utf16().count() as u32,
        },
    })
}

fn push_reference(
    entities: &mut Vec<Entity>,
    kind: EntityKind,
    name: String,
    uri: &Url,
    line_number: usize,
    line: &str,
) {
    let mut reference = entity(kind, name, None, uri, line_number, line, Confidence::Likely);
    reference.metadata.insert("reference".into(), "true".into());
    entities.push(reference);
}

fn call_string_argument(line: &str, markers: &[&str]) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    markers.iter().find_map(|marker| {
        lower
            .find(marker)
            .and_then(|start| quoted_or_path(&line[start + marker.len()..]))
    })
}

fn environment_reference(line: &str) -> Option<String> {
    if let Some(start) = line.find("%env(") {
        let tail = &line[start + 5..];
        let expression = tail.split([')', '%']).next()?;
        let name = expression.rsplit(':').next()?.to_string();
        return (!name.is_empty()).then_some(name);
    }
    call_string_argument(line, &["env("])
}

fn service_reference(line: &str) -> Option<String> {
    let value = quoted_or_path(line)?;
    value
        .strip_prefix('@')
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

fn autowire_service_reference(line: &str) -> Option<String> {
    if let Some(start) = line.to_ascii_lowercase().find("service:") {
        return quoted_or_path(&line[start + "service:".len()..]);
    }
    let start = line.to_ascii_lowercase().find("autowire(")? + "autowire(".len();
    quoted_or_path(&line[start..])
}

fn constructor_dependencies(
    line: &str,
    namespace: &str,
    imports: &HashMap<String, String>,
) -> Vec<String> {
    let Some(arguments) = line
        .split_once('(')
        .and_then(|(_, tail)| tail.split_once(')').map(|(args, _)| args))
    else {
        return Vec::new();
    };
    arguments
        .split(',')
        .filter_map(|argument| {
            let before_variable = argument.split('$').next()?.trim();
            let type_name = before_variable
                .split_whitespace()
                .last()?
                .trim_start_matches('?');
            if type_name.is_empty()
                || matches!(
                    type_name,
                    "string"
                        | "int"
                        | "float"
                        | "bool"
                        | "array"
                        | "callable"
                        | "iterable"
                        | "object"
                        | "mixed"
                )
            {
                return None;
            }
            let qualified = if let Some(qualified) = type_name.strip_prefix('\\') {
                qualified.to_string()
            } else if let Some(imported) = imports.get(type_name) {
                imported.clone()
            } else if type_name.contains('\\') {
                format!("{namespace}\\{type_name}")
            } else if namespace.is_empty() {
                type_name.to_string()
            } else {
                format!("{namespace}\\{type_name}")
            };
            Some(qualified)
        })
        .collect()
}

fn declaration(line: &str, keyword: &str) -> Option<(&'static str, String)> {
    let marker = format!("{keyword} ");
    let start = line.find(&marker)? + marker.len();
    let name = line[start..]
        .split(|c: char| c == '{' || c == ':' || c == '(' || c.is_whitespace())
        .next()?
        .trim_matches(['{', '\\'])
        .to_string();
    (!name.is_empty()).then_some((
        if keyword == "class" {
            "class"
        } else {
            "interface"
        },
        name,
    ))
}
fn php_property(line: &str) -> Option<String> {
    let start = line.find('$')? + 1;
    let name = line[start..]
        .split(|c: char| c == ';' || c == '=' || c.is_whitespace())
        .next()?;
    (!name.is_empty()).then_some(name.to_string())
}
fn route_name(line: &str) -> Option<String> {
    let marker = "name:";
    let start = line.find(marker)? + marker.len();
    quoted_or_path(&line[start..])
}

fn quoted_or_path(line: &str) -> Option<String> {
    let start = line.find(['\'', '"'])?;
    let quote = line.as_bytes()[start];
    let end = line[start + 1..].find(quote as char)? + start + 1;
    Some(line[start + 1..end].to_string())
}
