use std::path::Path;

use tower_lsp::lsp_types::{Position, Range, Url};

use super::{entity, Confidence, Entity, EntityKind};

pub(super) fn parse(root: &Path, path: &Path, text: &str, uri: &Url) -> Vec<Entity> {
    let _ = root;
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("php") => parse_php(text, uri),
        Some("xml") => parse_xml(text, uri),
        Some("yml" | "yaml") => parse_yaml(text, uri),
        _ => Vec::new(),
    }
}

fn parse_php(text: &str, uri: &Url) -> Vec<Entity> {
    let mut entities = Vec::new();
    let mut namespace = String::new();
    let mut imports = Vec::<(String, String)>::new();
    let mut owner: Option<String> = None;
    let mut pending_attributes = String::new();

    for (line_number, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(value) = trimmed
            .strip_prefix("namespace ")
            .and_then(|value| value.strip_suffix(';'))
        {
            namespace = value.trim().trim_matches('\\').to_string();
        }
        if let Some(value) = trimmed
            .strip_prefix("use ")
            .and_then(|value| value.strip_suffix(';'))
        {
            let (fqcn, alias) = value
                .split_once(" as ")
                .map(|(fqcn, alias)| (fqcn.trim(), alias.trim().to_string()))
                .unwrap_or_else(|| {
                    let fqcn = value.trim();
                    (fqcn, fqcn.rsplit('\\').next().unwrap_or(fqcn).to_string())
                });
            if !fqcn.starts_with("function ") && !fqcn.starts_with("const ") {
                imports.push((alias, fqcn.trim_start_matches('\\').to_string()));
            }
        }

        if trimmed.starts_with("#[") || !pending_attributes.is_empty() {
            pending_attributes.push(' ');
            pending_attributes.push_str(trimmed);
        }

        if let Some((_, class_name, name_start)) = declaration_name(line, "class") {
            let fqcn = qualify(&namespace, class_name);
            if has_attribute(&pending_attributes, "Entity") {
                let mut definition = make_entity(
                    EntityKind::DoctrineEntity,
                    class_name,
                    Some(fqcn.clone()),
                    uri,
                    line_number,
                    line,
                    name_start,
                );
                definition.metadata.insert("owner".into(), fqcn.clone());
                if let Some(table) = named_string_argument(&pending_attributes, "name") {
                    if has_attribute(&pending_attributes, "Table") {
                        definition.metadata.insert("table".into(), table);
                    }
                }
                entities.push(definition);
            }
            owner = Some(fqcn);
            pending_attributes.clear();
            continue;
        }

        if let (Some(owner_name), Some((property_name, property_start))) =
            (owner.as_deref(), php_property_name(line))
        {
            let relation = relation_name(&pending_attributes);
            let is_column = has_attribute(&pending_attributes, "Column") || relation.is_some();
            if is_column {
                let mut field = make_entity(
                    EntityKind::DoctrineField,
                    property_name,
                    Some(format!("{owner_name}::{property_name}")),
                    uri,
                    line_number,
                    line,
                    property_start,
                );
                field
                    .metadata
                    .insert("owner".into(), owner_name.to_string());
                if let Some(kind) = relation {
                    field.metadata.insert("relation".into(), kind.to_string());
                    if let Some((target, start)) = target_argument(&pending_attributes) {
                        let resolved = resolve_class(&target, &namespace, &imports);
                        field
                            .metadata
                            .insert("relation_target".into(), resolved.clone());
                        let mut reference = make_entity(
                            EntityKind::DoctrineEntity,
                            target.rsplit('\\').next().unwrap_or(&target),
                            Some(resolved),
                            uri,
                            line_number,
                            line,
                            start,
                        );
                        reference
                            .metadata
                            .insert("owner".into(), owner_name.to_string());
                        reference.metadata.insert("reference".into(), "true".into());
                        entities.push(reference);
                    }
                }
                if let Some(kind) = named_string_argument(&pending_attributes, "type") {
                    field.metadata.insert("type".into(), kind);
                }
                if let Some(name) = named_string_argument(&pending_attributes, "name") {
                    if has_attribute(&pending_attributes, "Column")
                        || has_attribute(&pending_attributes, "JoinColumn")
                    {
                        field.metadata.insert("column_name".into(), name.clone());
                        field.metadata.insert("mapping_name".into(), name);
                    }
                }
                entities.push(field);
            }
        }

        if trimmed.contains(';') || (trimmed.contains('{') && !trimmed.starts_with("#[")) {
            pending_attributes.clear();
        }
    }
    entities
}

fn parse_xml(text: &str, uri: &Url) -> Vec<Entity> {
    if !text.contains("<doctrine-mapping") && !text.contains("<entity") {
        return Vec::new();
    }
    let mut entities = Vec::new();
    let mut owner: Option<String> = None;
    for (line_number, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("<entity") {
            if let Some((class, start)) = xml_attribute(line, "name") {
                owner = Some(class.clone());
                let short = class.rsplit('\\').next().unwrap_or(&class);
                let start = start + class.rfind('\\').map_or(0, |offset| offset + 1);
                let mut definition = make_entity(
                    EntityKind::DoctrineEntity,
                    short,
                    Some(class.clone()),
                    uri,
                    line_number,
                    line,
                    start,
                );
                definition.metadata.insert("owner".into(), class);
                if let Some((table, _)) = xml_attribute(line, "table") {
                    definition.metadata.insert("table".into(), table);
                }
                entities.push(definition);
                continue;
            }
        }
        let Some(owner_name) = owner.as_deref() else {
            continue;
        };
        let tag = xml_tag_name(trimmed);
        if matches!(
            tag,
            Some(
                "field"
                    | "id"
                    | "embedded"
                    | "one-to-one"
                    | "one-to-many"
                    | "many-to-one"
                    | "many-to-many"
            )
        ) {
            let Some((field_name, start)) =
                xml_attribute(line, "field").or_else(|| xml_attribute(line, "name"))
            else {
                continue;
            };
            let mut field = make_entity(
                EntityKind::DoctrineField,
                &field_name,
                Some(format!("{owner_name}::{field_name}")),
                uri,
                line_number,
                line,
                start,
            );
            field
                .metadata
                .insert("owner".into(), owner_name.to_string());
            if let Some((kind, _)) = xml_attribute(line, "type") {
                field.metadata.insert("type".into(), kind);
            }
            if let Some((column, _)) = xml_attribute(line, "column") {
                field.metadata.insert("column_name".into(), column.clone());
                field.metadata.insert("mapping_name".into(), column);
            }
            if let Some(relation) = tag.filter(|tag| tag.contains('-')) {
                field
                    .metadata
                    .insert("relation".into(), relation.to_string());
                if let Some((target, target_start)) = xml_attribute(line, "target-entity") {
                    field
                        .metadata
                        .insert("relation_target".into(), target.clone());
                    let short = target.rsplit('\\').next().unwrap_or(&target).to_string();
                    let target_start =
                        target_start + target.rfind('\\').map_or(0, |offset| offset + 1);
                    let mut reference = make_entity(
                        EntityKind::DoctrineEntity,
                        &short,
                        Some(target),
                        uri,
                        line_number,
                        line,
                        target_start,
                    );
                    reference
                        .metadata
                        .insert("owner".into(), owner_name.to_string());
                    reference.metadata.insert("reference".into(), "true".into());
                    entities.push(reference);
                }
            }
            entities.push(field);
        }
        if trimmed.starts_with("</entity") {
            owner = None;
        }
    }
    entities
}

fn parse_yaml(text: &str, uri: &Url) -> Vec<Entity> {
    if !text.contains("type: entity")
        && !text.contains("type: 'entity'")
        && !text.contains("type: \"entity\"")
    {
        return Vec::new();
    }
    let lines = text.lines().collect::<Vec<_>>();
    let mut entities = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();
        let indent = indentation(line);
        let Some((key, _)) = trimmed.split_once(':') else {
            index += 1;
            continue;
        };
        if indent != 0 || !key.contains('\\') {
            index += 1;
            continue;
        }
        let owner = unquote(key.trim()).to_string();
        let block_end = (index + 1..lines.len())
            .find(|candidate| {
                !lines[*candidate].trim().is_empty() && indentation(lines[*candidate]) == 0
            })
            .unwrap_or(lines.len());
        let block = &lines[index + 1..block_end];
        if !block
            .iter()
            .any(|line| line.trim().starts_with("type:") && line.contains("entity"))
        {
            index = block_end;
            continue;
        }
        let short = owner.rsplit('\\').next().unwrap_or(&owner);
        let owner_start =
            line.find(key).unwrap_or(0) + key.rfind('\\').map_or(0, |offset| offset + 1);
        let mut definition = make_entity(
            EntityKind::DoctrineEntity,
            short,
            Some(owner.clone()),
            uri,
            index,
            line,
            owner_start,
        );
        definition.metadata.insert("owner".into(), owner.clone());
        if let Some(table_line) = block.iter().find(|line| line.trim().starts_with("table:")) {
            if let Some((_, value)) = table_line.trim().split_once(':') {
                definition
                    .metadata
                    .insert("table".into(), unquote(value.trim()).to_string());
            }
        }
        entities.push(definition);

        let mut section: Option<&str> = None;
        let mut active_field: Option<(String, usize, usize)> = None;
        let mut field_data = Vec::<(usize, String)>::new();
        let flush = |field: Option<(String, usize, usize)>,
                     data: &[(usize, String)],
                     entities: &mut Vec<Entity>| {
            let Some((name, line_no, start)) = field else {
                return;
            };
            let source = lines[line_no];
            let mut field = make_entity(
                EntityKind::DoctrineField,
                &name,
                Some(format!("{owner}::{name}")),
                uri,
                line_no,
                source,
                start,
            );
            field.metadata.insert("owner".into(), owner.clone());
            for (data_line, entry) in data {
                let Some((key, value)) = entry.split_once(':') else {
                    continue;
                };
                let key = key.trim();
                let value = unquote(value.trim()).to_string();
                if key == "type" {
                    field.metadata.insert("type".into(), value);
                } else if key == "column" || key == "columnName" {
                    field.metadata.insert("column_name".into(), value.clone());
                    field.metadata.insert("mapping_name".into(), value);
                } else if key == "relation" {
                    field.metadata.insert("relation".into(), value);
                } else if key == "targetEntity" || key == "target-entity" {
                    field
                        .metadata
                        .insert("relation_target".into(), value.clone());
                    let target_range = value.rsplit('\\').next().unwrap_or(&value).to_string();
                    let start = lines[*data_line].rfind(&target_range).unwrap_or(0);
                    let mut reference = make_entity(
                        EntityKind::DoctrineEntity,
                        &target_range,
                        Some(value),
                        uri,
                        *data_line,
                        lines[*data_line],
                        start,
                    );
                    reference.metadata.insert("owner".into(), owner.clone());
                    reference.metadata.insert("reference".into(), "true".into());
                    entities.push(reference);
                }
            }
            entities.push(field);
        };

        for (offset, source) in block.iter().enumerate() {
            let line_no = index + 1 + offset;
            let current = source.trim();
            let current_indent = indentation(source);
            if current.is_empty()
                || current.starts_with('#')
                || (current_indent <= 2
                    && (current.starts_with("type:") || current.starts_with("table:")))
            {
                continue;
            }
            if current_indent <= 2 {
                if matches!(
                    current.trim_end_matches(':'),
                    "fields"
                        | "id"
                        | "oneToOne"
                        | "oneToMany"
                        | "manyToOne"
                        | "manyToMany"
                        | "many-to-one"
                        | "many-to-many"
                ) {
                    flush(active_field.take(), &field_data, &mut entities);
                    field_data.clear();
                    section = Some(current.trim_end_matches(':'));
                } else {
                    section = None;
                }
                continue;
            }
            let Some((key, value)) = current.split_once(':') else {
                continue;
            };
            let key = unquote(key.trim()).to_string();
            if current_indent == 4 {
                if let Some(current_section) = section {
                    flush(active_field.take(), &field_data, &mut entities);
                    field_data.clear();
                    let start = source.find(key.as_str()).unwrap_or(0);
                    active_field = Some((key, line_no, start));
                    if !value.trim().is_empty() {
                        field_data.push((line_no, format!("type: {}", unquote(value.trim()))));
                    }
                    if current_section.starts_with("one") || current_section.starts_with("many") {
                        field_data.push((line_no, format!("relation: {current_section}")));
                    }
                } else if active_field.is_some() {
                    field_data.push((line_no, current.to_string()));
                }
            } else if active_field.is_some() {
                field_data.push((line_no, current.to_string()));
            }
        }
        flush(active_field.take(), &field_data, &mut entities);
        index = block_end;
    }
    entities
}

fn make_entity(
    kind: EntityKind,
    name: &str,
    qualified_name: Option<String>,
    uri: &Url,
    line_number: usize,
    line: &str,
    byte_start: usize,
) -> Entity {
    let mut result = entity(
        kind,
        name.to_string(),
        qualified_name,
        uri,
        line_number,
        line,
        Confidence::Likely,
    );
    let start = line[..byte_start.min(line.len())].encode_utf16().count() as u32;
    let length = name.encode_utf16().count() as u32;
    result.range = Range {
        start: Position {
            line: line_number as u32,
            character: start,
        },
        end: Position {
            line: line_number as u32,
            character: start + length,
        },
    };
    result
}

fn declaration_name<'a>(line: &'a str, keyword: &str) -> Option<(usize, &'a str, usize)> {
    let start = line.find(keyword)? + keyword.len();
    let relative = line[start..]
        .find(|character: char| character.is_ascii_alphanumeric() || character == '_')?;
    let name_start = start + relative;
    let end = line[name_start..]
        .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .map(|offset| name_start + offset)
        .unwrap_or(line.len());
    Some((name_start, &line[name_start..end], name_start))
}

fn php_property_name(line: &str) -> Option<(&str, usize)> {
    let dollar = line.find('$')?;
    let start = dollar + 1;
    let end = line[start..]
        .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .map(|offset| start + offset)?;
    (end > start).then_some((&line[start..end], start))
}

fn has_attribute(attributes: &str, name: &str) -> bool {
    attributes
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .any(|part| part == name)
}

fn relation_name(attributes: &str) -> Option<&'static str> {
    [
        ("ManyToOne", "many-to-one"),
        ("OneToMany", "one-to-many"),
        ("OneToOne", "one-to-one"),
        ("ManyToMany", "many-to-many"),
    ]
    .into_iter()
    .find_map(|(attribute, relation)| has_attribute(attributes, attribute).then_some(relation))
}

fn named_string_argument(source: &str, name: &str) -> Option<String> {
    let index = source.find(name)? + name.len();
    let remainder = source[index..].trim_start();
    let remainder = remainder.strip_prefix(':')?.trim_start();
    let quote = remainder.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let end = remainder[1..].find(quote)? + 1;
    Some(remainder[1..end].to_string())
}

fn target_argument(attributes: &str) -> Option<(String, usize)> {
    let index = attributes.find("targetEntity")? + "targetEntity".len();
    let rest = attributes[index..]
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    let offset = attributes.len()
        - attributes[index..]
            .trim_start()
            .strip_prefix(':')?
            .trim_start()
            .len();
    if rest.starts_with('\'') || rest.starts_with('"') {
        let quote = rest.chars().next()?;
        let end = rest[1..].find(quote)? + 1;
        let target = rest[1..end].to_string();
        return Some((
            target.clone(),
            offset + 1 + rest[..end].find(target.as_str()).unwrap_or(0),
        ));
    }
    let length = rest.find("::class")?;
    let target = rest[..length].trim().trim_start_matches('\\').to_string();
    let start = offset + rest[..length].find(target.as_str()).unwrap_or(0);
    Some((target, start))
}

fn resolve_class(class: &str, namespace: &str, imports: &[(String, String)]) -> String {
    if class.contains('\\') {
        let first = class.split('\\').next().unwrap_or(class);
        if let Some((_, fqcn)) = imports.iter().find(|(alias, _)| alias == first) {
            return class.replacen(first, fqcn, 1);
        }
        return class.trim_start_matches('\\').to_string();
    }
    if let Some((_, fqcn)) = imports.iter().find(|(alias, _)| alias == class) {
        return fqcn.clone();
    }
    qualify(namespace, class)
}

fn qualify(namespace: &str, class: &str) -> String {
    if namespace.is_empty() {
        class.to_string()
    } else {
        format!("{namespace}\\{class}")
    }
}

fn xml_attribute(line: &str, name: &str) -> Option<(String, usize)> {
    let marker = format!("{name}=");
    let index = line.find(&marker)? + marker.len();
    let rest = line[index..].trim_start();
    let quote = rest.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let leading = line[index..].len() - rest.len();
    let end = rest[1..].find(quote)? + 1;
    Some((rest[1..end].to_string(), index + leading + 1))
}

fn xml_tag_name(line: &str) -> Option<&str> {
    line.strip_prefix('<')?
        .split(|character: char| character.is_whitespace() || character == '>' || character == '/')
        .next()
}

fn indentation(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn unquote(value: &str) -> &str {
    value.trim().trim_matches('\'').trim_matches('"')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri() -> Url {
        Url::parse("file:///tmp/Entity.php").unwrap()
    }

    #[test]
    fn parses_php_attribute_entity_fields_and_relation_reference() {
        let source = r#"<?php
namespace App\Entity;
use Doctrine\ORM\Mapping as ORM;
#[ORM\Entity]
#[ORM\Table(name: 'members')]
class Member {
    #[ORM\Id]
    #[ORM\Column(type: 'integer')]
    private int $id;

    #[ORM\ManyToOne(targetEntity: Group::class)]
    #[ORM\JoinColumn(name: 'group_id')]
    private ?Group $group = null;
}"#;
        let parsed = parse_php(source, &uri());
        let entity = parsed
            .iter()
            .find(|item| {
                item.kind == EntityKind::DoctrineEntity
                    && item.metadata.get("reference") != Some(&"true".into())
            })
            .unwrap();
        assert_eq!(
            entity.qualified_name.as_deref(),
            Some("App\\Entity\\Member")
        );
        assert_eq!(
            entity.metadata.get("table").map(String::as_str),
            Some("members")
        );
        let id = parsed
            .iter()
            .find(|item| item.kind == EntityKind::DoctrineField && item.name == "id")
            .unwrap();
        assert_eq!(id.metadata.get("type").map(String::as_str), Some("integer"));
        assert_eq!(id.range.start.line, 8);
        let group = parsed
            .iter()
            .find(|item| item.kind == EntityKind::DoctrineField && item.name == "group")
            .unwrap();
        assert_eq!(
            group.metadata.get("relation_target").map(String::as_str),
            Some("App\\Entity\\Group")
        );
        assert_eq!(
            group.metadata.get("column_name").map(String::as_str),
            Some("group_id")
        );
        assert!(parsed
            .iter()
            .any(
                |item| item.metadata.get("reference").map(String::as_str) == Some("true")
                    && item.name == "Group"
            ));
    }

    #[test]
    fn parses_doctrine_yaml_mapping() {
        let source = "App\\Entity\\Article:\n  type: entity\n  table: articles\n  fields:\n    title:\n      type: string\n  manyToOne:\n    author:\n      targetEntity: App\\Entity\\User\n";
        let parsed = parse_yaml(source, &uri());
        assert!(parsed.iter().any(|item| {
            item.kind == EntityKind::DoctrineEntity
                && item.qualified_name.as_deref() == Some("App\\Entity\\Article")
        }));
        let title = parsed
            .iter()
            .find(|item| item.kind == EntityKind::DoctrineField && item.name == "title")
            .unwrap();
        assert_eq!(
            title.metadata.get("type").map(String::as_str),
            Some("string")
        );
        let author = parsed
            .iter()
            .find(|item| item.kind == EntityKind::DoctrineField && item.name == "author")
            .unwrap();
        assert_eq!(
            author.metadata.get("relation_target").map(String::as_str),
            Some("App\\Entity\\User")
        );
    }

    #[test]
    fn parses_doctrine_xml_mapping() {
        let source = r#"<doctrine-mapping>
  <entity name="App\Entity\Post" table="posts">
    <field name="title" type="string" column="post_title" />
    <many-to-one field="author" target-entity="App\Entity\User" />
  </entity>
</doctrine-mapping>"#;
        let parsed = parse_xml(source, &uri());
        assert!(parsed
            .iter()
            .any(|item| item.kind == EntityKind::DoctrineEntity && item.name == "Post"));
        let title = parsed
            .iter()
            .find(|item| item.kind == EntityKind::DoctrineField && item.name == "title")
            .unwrap();
        assert_eq!(
            title.metadata.get("type").map(String::as_str),
            Some("string")
        );
        assert_eq!(
            title.metadata.get("column_name").map(String::as_str),
            Some("post_title")
        );
        assert!(parsed
            .iter()
            .any(
                |item| item.metadata.get("reference").map(String::as_str) == Some("true")
                    && item.qualified_name.as_deref() == Some("App\\Entity\\User")
            ));
    }
}
