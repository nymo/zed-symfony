use std::path::Path;

use tower_lsp::lsp_types::{Position, Range, Url};

use super::{entity, Confidence, Entity, EntityKind};

/// Byte span within a source file, converted to an LSP range when an entity is built.
#[derive(Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
}

/// Shared source context used to build entities with precise, UTF-16-correct ranges.
struct Source<'a> {
    uri: &'a Url,
    text: &'a str,
    lines: &'a [usize],
}

impl<'a> Source<'a> {
    fn new(uri: &'a Url, text: &'a str, lines: &'a [usize]) -> Self {
        Self { uri, text, lines }
    }

    fn entity(
        &self,
        kind: EntityKind,
        name: String,
        qualified: Option<String>,
        span: Span,
        confidence: Confidence,
    ) -> Entity {
        let line = line_for_offset(self.lines, span.start);
        let mut result = entity(
            kind,
            name,
            qualified,
            self.uri,
            line,
            line_slice(self.text, self.lines, span.start),
            confidence,
        );
        result.range = range_for(self.text, self.lines, span.start, span.end);
        result
    }

    fn push_named(
        &self,
        entities: &mut Vec<Entity>,
        kind: EntityKind,
        name: String,
        qualified: Option<String>,
        span: Span,
        confidence: Confidence,
    ) {
        entities.push(self.entity(kind, name, qualified, span, confidence));
    }

    fn push_reference(
        &self,
        entities: &mut Vec<Entity>,
        kind: EntityKind,
        name: String,
        span: Span,
    ) {
        let mut reference = self.entity(kind, name, None, span, Confidence::Likely);
        reference.metadata.insert("reference".into(), "true".into());
        entities.push(reference);
    }
}

pub(super) fn parse(root: &Path, path: &Path, text: &str, uri: &Url) -> Vec<Entity> {
    let mut entities = Vec::new();
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("php") => parse_php(text, uri, &mut entities),
        Some("twig") => parse_twig(root, path, text, uri, &mut entities),
        _ => {}
    }
    entities
}

fn parse_php(text: &str, uri: &Url, entities: &mut Vec<Entity>) {
    let lines = line_starts(text);
    let source = Source::new(uri, text, &lines);
    let php_classes = class_declarations(text, &lines);
    let form_types = php_classes
        .iter()
        .filter(|class| {
            class.header.to_ascii_lowercase().contains("abstracttype")
                || class
                    .header
                    .to_ascii_lowercase()
                    .contains("formtypeinterface")
        })
        .collect::<Vec<_>>();

    for class in &php_classes {
        let header = class.header.to_ascii_lowercase();
        let body = text[class.body_start.min(text.len())..class.body_end.min(text.len())]
            .to_ascii_lowercase();
        if header.contains("abstractextension")
            || header.contains("twig\\extensioninterface")
            || [
                "twigfunction",
                "twigfilter",
                "twigtest",
                "astwigfunction",
                "astwigfilter",
                "astwigtest",
            ]
            .iter()
            .any(|marker| body.contains(marker))
        {
            let mut extension = entity(
                EntityKind::TwigExtension,
                class.name.clone(),
                Some(class.qualified_name.clone()),
                uri,
                class.line,
                &text[lines[class.line]..line_end(text, lines[class.line])],
                Confidence::Likely,
            );
            extension.range = range_for(text, &lines, class.name_start, class.name_end);
            entities.push(extension);
        }
    }

    for class in &form_types {
        let mut form_type = entity(
            EntityKind::FormType,
            class.name.clone(),
            Some(class.qualified_name.clone()),
            uri,
            class.line,
            &text[lines[class.line]..line_end(text, lines[class.line])],
            Confidence::Exact,
        );
        form_type.range = range_for(text, &lines, class.name_start, class.name_end);
        if let Some((source_name, target, start, end)) = data_class_target(text, class) {
            form_type
                .metadata
                .insert("data_class".into(), target.clone());
            let mut reference = source.entity(
                EntityKind::Class,
                source_name,
                Some(target),
                Span { start, end },
                Confidence::Likely,
            );
            reference.metadata.insert("reference".into(), "true".into());
            reference
                .metadata
                .insert("form_type".into(), class.qualified_name.clone());
            entities.push(reference);
        }
        entities.push(form_type);
    }

    // Symfony's component attribute is intentionally recognized by its fully spelled
    // attribute name, not by an arbitrary class containing the word "Component".
    for (attribute_start, name_arg) in twig_component_attributes(text) {
        let Some(class) = php_classes
            .iter()
            .filter(|class| class.name_start >= attribute_start)
            .min_by_key(|class| class.name_start)
        else {
            continue;
        };
        let component_name = name_arg
            .as_ref()
            .map(|name| name.value.clone())
            .unwrap_or_else(|| class.name.clone());
        let mut component = entity(
            EntityKind::TwigComponent,
            component_name.clone(),
            Some(class.qualified_name.clone()),
            uri,
            class.line,
            &text[lines[class.line]..line_end(text, lines[class.line])],
            Confidence::Exact,
        );
        if let Some(name_arg) = &name_arg {
            component.range = range_for(text, &lines, name_arg.start, name_arg.end);
        } else {
            component.range = range_for(
                text,
                &lines,
                attribute_start,
                attribute_start + "AsTwigComponent".len(),
            );
        }
        component
            .metadata
            .insert("class".into(), class.qualified_name.clone());
        component
            .metadata
            .insert("component".into(), component_name.clone());
        let explicit_template = component_template(text, attribute_start);
        let template = explicit_template
            .as_ref()
            .map(|template| template.value.clone())
            .unwrap_or_else(|| {
                format!(
                    "components/{}.html.twig",
                    component_name
                        .replace([':', '\\'], "/")
                        .to_ascii_lowercase()
                )
            });
        component.metadata.insert("template".into(), template);
        entities.push(component);
        if let Some(template) = explicit_template {
            source.push_reference(
                entities,
                EntityKind::TwigTemplate,
                template.value,
                Span {
                    start: template.start,
                    end: template.end,
                },
            );
        }

        for (prop_name, start, end, ty) in public_properties(text, class) {
            let mut prop = entity(
                EntityKind::TwigComponentProp,
                prop_name.clone(),
                Some(format!("{}::{prop_name}", class.qualified_name)),
                uri,
                line_for_offset(&lines, start),
                line_slice(text, &lines, start),
                Confidence::Likely,
            );
            prop.range = range_for(text, &lines, start, end);
            prop.metadata
                .insert("component".into(), component_name.clone());
            if let Some(ty) = ty {
                prop.metadata.insert("type".into(), ty);
            }
            entities.push(prop);
        }
    }

    for registration in find_named_calls(text, &["TwigFunction", "TwigFilter", "TwigTest"]) {
        if let Some(name) = registration
            .args
            .first()
            .and_then(|arg| string_literal(text, arg.0, arg.1))
        {
            let kind = match registration.name.to_ascii_lowercase().as_str() {
                "twigfunction" => EntityKind::TwigFunction,
                "twigfilter" => EntityKind::TwigFilter,
                _ => EntityKind::TwigTest,
            };
            source.push_named(
                entities,
                kind,
                name.value,
                None,
                Span {
                    start: name.start,
                    end: name.end,
                },
                Confidence::Exact,
            );
        }
    }

    for registration in find_named_calls(text, &["AsTwigFunction", "AsTwigFilter", "AsTwigTest"]) {
        if let Some(name) = registration
            .args
            .first()
            .and_then(|arg| string_literal(text, arg.0, arg.1))
        {
            let kind = match registration.name.to_ascii_lowercase().as_str() {
                "astwigfunction" => EntityKind::TwigFunction,
                "astwigfilter" => EntityKind::TwigFilter,
                _ => EntityKind::TwigTest,
            };
            source.push_named(
                entities,
                kind,
                name.value,
                None,
                Span {
                    start: name.start,
                    end: name.end,
                },
                Confidence::Exact,
            );
        }
    }

    // A token parser advertises the custom tag with getTag(); only that explicit
    // declaration is indexed, avoiding arbitrary PHP strings that happen to look like tags.
    if text.contains("TokenParser") || text.contains("token_parser") {
        for value in get_tag_returns(text) {
            source.push_named(
                entities,
                EntityKind::TwigTag,
                value.value,
                None,
                Span {
                    start: value.start,
                    end: value.end,
                },
                Confidence::Likely,
            );
        }
    }

    if !form_types.is_empty() {
        for call in find_named_calls(text, &["add"]) {
            if !is_builder_add(text, call.start) {
                continue;
            }
            let Some(field_name) = call
                .args
                .first()
                .and_then(|arg| string_literal(text, arg.0, arg.1))
            else {
                continue;
            };
            let parent = form_types
                .iter()
                .filter(|class| class.body_start <= call.start && call.start <= class.body_end)
                .min_by_key(|class| class.body_end.saturating_sub(class.body_start))
                .or_else(|| form_types.first());
            let qualified = parent.map(|class| class.qualified_name.clone());
            let mut field = source.entity(
                EntityKind::FormField,
                field_name.value.clone(),
                qualified.clone(),
                Span {
                    start: field_name.start,
                    end: field_name.end,
                },
                Confidence::Exact,
            );
            if let Some(form_type) = qualified {
                field.metadata.insert("form_type".into(), form_type);
            }
            entities.push(field);

            if let Some(options) = call.args.get(2) {
                for (option, start, end) in array_keys(text, options.0, options.1) {
                    let mut item = source.entity(
                        EntityKind::FormOption,
                        option,
                        None,
                        Span { start, end },
                        Confidence::Likely,
                    );
                    item.metadata
                        .insert("form_field".into(), field_name.value.clone());
                    if let Some(parent) = parent {
                        item.metadata
                            .insert("form_type".into(), parent.qualified_name.clone());
                    }
                    entities.push(item);
                }
            }
        }
    }
}

fn data_class_target(text: &str, class: &ClassDecl) -> Option<(String, String, usize, usize)> {
    let body_start = class.body_start.min(text.len());
    let body_end = class.body_end.min(text.len());
    let body = &text[body_start..body_end];
    let data_class = body.find("data_class")? + "data_class".len();
    let tail = &body[data_class..];
    let arrow = tail.find("=>")?;
    let value_start = body_start + data_class + arrow + 2;
    let value_tail = &text[value_start..body_end];
    let class_suffix = value_tail.find("::class")?;
    let expression = &value_tail[..class_suffix];
    let expression_end = expression.trim_end().len();
    let expression = &expression[..expression_end];
    let class_start = expression
        .rfind(|character: char| {
            !character.is_ascii_alphanumeric() && character != '_' && character != '\\'
        })
        .map(|index| index + 1)
        .unwrap_or(0);
    let raw_name = &expression[class_start..];
    let fully_qualified = raw_name.starts_with('\\');
    let source_name = raw_name.trim_start_matches('\\');
    if source_name.is_empty() {
        return None;
    }
    let imports = php_imports(text);
    let first = source_name.split('\\').next().unwrap_or(source_name);
    let resolved = if fully_qualified {
        source_name.to_string()
    } else if let Some((_, imported)) = imports.iter().find(|(alias, _)| alias == first) {
        source_name.replacen(first, imported, 1)
    } else if source_name.contains('\\') {
        source_name.to_string()
    } else if let Some((namespace, _)) = class.qualified_name.rsplit_once('\\') {
        format!("{namespace}\\{source_name}")
    } else {
        source_name.to_string()
    };
    let start = value_start + class_start + if fully_qualified { 1 } else { 0 };
    Some((
        source_name.to_string(),
        resolved,
        start,
        start + source_name.len(),
    ))
}

fn php_imports(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let value = line.trim().strip_prefix("use ")?.strip_suffix(';')?;
            if value.starts_with("function ") || value.starts_with("const ") {
                return None;
            }
            let (name, alias) = if let Some((name, alias)) = value.split_once(" as ") {
                (name.trim(), alias.trim())
            } else {
                let name = value.trim();
                (name, name.rsplit('\\').next()?)
            };
            Some((alias.to_string(), name.trim_start_matches('\\').to_string()))
        })
        .collect()
}

fn parse_twig(root: &Path, path: &Path, text: &str, uri: &Url, entities: &mut Vec<Entity>) {
    let lines = line_starts(text);
    let source = Source::new(uri, text, &lines);
    // Preserve the common component invocation forms and the explicit <twig:...> syntax.
    for call in find_named_calls(text, &["component"]) {
        if in_twig_expression(text, call.start) {
            if let Some(name) = call
                .args
                .first()
                .and_then(|arg| string_literal(text, arg.0, arg.1))
            {
                source.push_reference(
                    entities,
                    EntityKind::TwigComponent,
                    name.value,
                    Span {
                        start: name.start,
                        end: name.end,
                    },
                );
            }
        }
    }
    scan_component_elements(text, uri, &lines, entities);

    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed_start = line.len() - line.trim_start().len();
        let content = line.trim();
        let line_offset = offset + trimmed_start;
        let tag_body = twig_tag_body(content);
        if let Some((tag, tag_offset)) = tag_body {
            let standard = [
                "if",
                "elseif",
                "else",
                "endif",
                "for",
                "endfor",
                "while",
                "endwhile",
                "set",
                "with",
                "endwith",
                "block",
                "endblock",
                "extends",
                "include",
                "embed",
                "endembed",
                "import",
                "from",
                "macro",
                "endmacro",
                "trans",
                "endtrans",
                "autoescape",
                "endautoescape",
                "apply",
                "endapply",
                "sandbox",
                "endsandbox",
                "verbatim",
                "endverbatim",
                "flush",
                "do",
                "use",
                "deprecated",
                "cache",
                "endcache",
                "php",
                "endphp",
            ];
            if !standard.contains(&tag.to_ascii_lowercase().as_str()) {
                let start = line_offset + tag_offset;
                source.push_reference(
                    entities,
                    EntityKind::TwigTag,
                    tag.to_string(),
                    Span {
                        start,
                        end: start + tag.len(),
                    },
                );
            }
        }
        if let Some((expression_start, expression_end)) = twig_expression_bounds(line) {
            let body_start = offset + expression_start;
            let body_end = offset + expression_end;
            scan_twig_calls(text, body_start, body_end, uri, &lines, entities);
            scan_filters_and_tests(text, body_start, body_end, uri, &lines, entities);
        }
        // Also inspect expressions in Twig statements such as {% set x = fn() %}.
        if let Some((open, close)) = statement_bounds(line) {
            let body_start = offset + open;
            let body_end = offset + close;
            scan_twig_calls(text, body_start, body_end, uri, &lines, entities);
            scan_filters_and_tests(text, body_start, body_end, uri, &lines, entities);
        }
        offset += line.len();
    }

    // Component-style prop references (<twig:alert title="...">) are attached to
    // the nearest component tag and use exact identifier ranges.
    let _ = (root, path);
}

fn scan_twig_calls(
    text: &str,
    start: usize,
    end: usize,
    uri: &Url,
    lines: &[usize],
    entities: &mut Vec<Entity>,
) {
    let source = Source::new(uri, text, lines);
    let bytes = text.as_bytes();
    let mut cursor = start;
    while cursor < end {
        if cursor + 5 < end
            && bytes[cursor..cursor + 5].eq_ignore_ascii_case(b"form.")
            && (cursor == start || !is_ident_continue(bytes[cursor - 1]))
            && is_ident_start(bytes[cursor + 5])
        {
            let field_start = cursor + 5;
            let mut field_end = field_start + 1;
            while field_end < end && is_ident_continue(bytes[field_end]) {
                field_end += 1;
            }
            source.push_reference(
                entities,
                EntityKind::FormField,
                text[field_start..field_end].to_string(),
                Span {
                    start: field_start,
                    end: field_end,
                },
            );
            cursor = field_end;
        }
        if cursor < end && is_ident_start(bytes[cursor]) {
            let name_start = cursor;
            cursor += 1;
            while cursor < end && is_ident_continue(bytes[cursor]) {
                cursor += 1;
            }
            let name_end = cursor;
            let mut after = cursor;
            while after < end && bytes[after].is_ascii_whitespace() {
                after += 1;
            }
            let previous = text[..name_start]
                .chars()
                .rev()
                .find(|ch| !ch.is_whitespace());
            if after < end && bytes[after] == b'(' && previous != Some('.') && previous != Some('|')
            {
                // Twig calls are only collected inside explicit expression/statement bodies.
                let name = text[name_start..name_end].to_string();
                source.push_reference(
                    entities,
                    EntityKind::TwigFunction,
                    name,
                    Span {
                        start: name_start,
                        end: name_end,
                    },
                );
            }
        } else {
            cursor += 1;
        }
    }
}

fn scan_filters_and_tests(
    text: &str,
    start: usize,
    end: usize,
    uri: &Url,
    lines: &[usize],
    entities: &mut Vec<Entity>,
) {
    let source = Source::new(uri, text, lines);
    let bytes = text.as_bytes();
    let mut cursor = start;
    while cursor < end {
        if bytes[cursor] == b'|' && bytes.get(cursor + 1) != Some(&b'|') {
            let mut name_start = cursor + 1;
            while name_start < end && bytes[name_start].is_ascii_whitespace() {
                name_start += 1;
            }
            if name_start < end && is_ident_start(bytes[name_start]) {
                let mut name_end = name_start + 1;
                while name_end < end && is_ident_continue(bytes[name_end]) {
                    name_end += 1;
                }
                source.push_reference(
                    entities,
                    EntityKind::TwigFilter,
                    text[name_start..name_end].to_string(),
                    Span {
                        start: name_start,
                        end: name_end,
                    },
                );
                cursor = name_end;
                continue;
            }
        }
        if keyword_at(bytes, cursor, end, b"is") {
            let mut name_start = cursor + 2;
            while name_start < end && bytes[name_start].is_ascii_whitespace() {
                name_start += 1;
            }
            if keyword_at(bytes, name_start, end, b"not") {
                name_start += 3;
                while name_start < end && bytes[name_start].is_ascii_whitespace() {
                    name_start += 1;
                }
            }
            if name_start < end && is_ident_start(bytes[name_start]) {
                let mut name_end = name_start + 1;
                while name_end < end && is_ident_continue(bytes[name_end]) {
                    name_end += 1;
                }
                let name = text[name_start..name_end].to_ascii_lowercase();
                if ![
                    "defined",
                    "sameas",
                    "even",
                    "odd",
                    "iterable",
                    "constant",
                    "divisibleby",
                    "empty",
                    "null",
                    "none",
                    "true",
                    "false",
                ]
                .contains(&name.as_str())
                {
                    source.push_reference(
                        entities,
                        EntityKind::TwigTest,
                        text[name_start..name_end].to_string(),
                        Span {
                            start: name_start,
                            end: name_end,
                        },
                    );
                }
                cursor = name_end;
                continue;
            }
        }
        cursor += 1;
    }
}

fn scan_component_elements(text: &str, uri: &Url, lines: &[usize], entities: &mut Vec<Entity>) {
    let source = Source::new(uri, text, lines);
    let bytes = text.as_bytes();
    let mut cursor = 0;
    while cursor + 6 < bytes.len() {
        if bytes[cursor..].starts_with(b"<twig:") {
            let name_start = cursor + 6;
            let mut name_end = name_start;
            while name_end < bytes.len()
                && (is_ident_continue(bytes[name_end])
                    || bytes[name_end] == b':'
                    || bytes[name_end] == b'-')
            {
                name_end += 1;
            }
            if name_end > name_start {
                let name = text[name_start..name_end].to_string();
                source.push_reference(
                    entities,
                    EntityKind::TwigComponent,
                    name.clone(),
                    Span {
                        start: name_start,
                        end: name_end,
                    },
                );
                let tag_end = text[name_end..]
                    .find('>')
                    .map(|at| name_end + at)
                    .unwrap_or(name_end);
                let mut prop_cursor = name_end;
                while prop_cursor < tag_end {
                    if is_ident_start(bytes[prop_cursor]) {
                        let prop_start = prop_cursor;
                        prop_cursor += 1;
                        while prop_cursor < tag_end && is_ident_continue(bytes[prop_cursor]) {
                            prop_cursor += 1;
                        }
                        let mut equals = prop_cursor;
                        while equals < tag_end && bytes[equals].is_ascii_whitespace() {
                            equals += 1;
                        }
                        if equals < tag_end && bytes[equals] == b'=' {
                            source.push_reference(
                                entities,
                                EntityKind::TwigComponentProp,
                                text[prop_start..prop_cursor].to_string(),
                                Span {
                                    start: prop_start,
                                    end: prop_cursor,
                                },
                            );
                            if let Some(reference) = entities.last_mut() {
                                reference.metadata.insert("component".into(), name.clone());
                            }
                        }
                    } else {
                        prop_cursor += 1;
                    }
                }
                cursor = tag_end.saturating_add(1);
                continue;
            }
        }
        cursor += 1;
    }
}

#[derive(Clone)]
struct ClassDecl {
    name: String,
    qualified_name: String,
    line: usize,
    name_start: usize,
    name_end: usize,
    header: String,
    body_start: usize,
    body_end: usize,
}

fn class_declarations(text: &str, lines: &[usize]) -> Vec<ClassDecl> {
    let mut namespace = String::new();
    let mut classes = Vec::new();
    for (line_no, line) in text.split_inclusive('\n').enumerate() {
        let trimmed = line.trim();
        if let Some(value) = trimmed
            .strip_prefix("namespace ")
            .and_then(|value| value.strip_suffix(';'))
        {
            namespace = value.trim().to_string();
        }
        if let Some(at) = trimmed.find("class ") {
            let rest = &trimmed[at + 6..];
            let name = rest
                .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
                .next()
                .unwrap_or("");
            if !name.is_empty() {
                let line_start = lines.get(line_no).copied().unwrap_or(0);
                let name_start = line_start + line.find(name).unwrap_or(0);
                let header_start = line_start + line.find("class ").unwrap_or(0);
                let after = (header_start..text.len())
                    .find(|index| text.as_bytes()[*index] == b'{')
                    .unwrap_or(line_start + line.len());
                let body_start = after.saturating_add(1);
                let body_end = matching_brace(text, after).unwrap_or(text.len());
                let header = text[header_start..after].to_string();
                classes.push(ClassDecl {
                    name: name.into(),
                    qualified_name: if namespace.is_empty() {
                        name.into()
                    } else {
                        format!("{namespace}\\{name}")
                    },
                    line: line_no,
                    name_start,
                    name_end: name_start + name.len(),
                    header,
                    body_start,
                    body_end,
                });
            }
        }
    }
    classes
}

fn public_properties(text: &str, class: &ClassDecl) -> Vec<(String, usize, usize, Option<String>)> {
    let body = &text[class.body_start.min(text.len())..class.body_end.min(text.len())];
    let mut result = Vec::new();
    let mut base = class.body_start;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if !trimmed.starts_with("public ")
            || trimmed.starts_with("public function")
            || trimmed.starts_with("public const")
        {
            base += line.len();
            continue;
        }
        if let Some(dollar) = trimmed.find('$') {
            let name_start_local = dollar + 1;
            let name = trimmed[name_start_local..]
                .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
                .next()
                .unwrap_or("");
            if !name.is_empty() {
                let start = base + indent + name_start_local;
                let prefix = trimmed[7..dollar].trim();
                let ty = (!prefix.is_empty()).then(|| prefix.to_string());
                result.push((name.into(), start, start + name.len(), ty));
            }
        }
        base += line.len();
    }
    result
}

#[derive(Clone)]
struct Call {
    name: String,
    start: usize,
    args: Vec<(usize, usize)>,
}

#[derive(Clone)]
struct StringValue {
    value: String,
    start: usize,
    end: usize,
}

fn find_named_calls(text: &str, names: &[&str]) -> Vec<Call> {
    let lower = text.to_ascii_lowercase();
    let mut calls = Vec::new();
    for wanted in names {
        let wanted_lower = wanted.to_ascii_lowercase();
        let mut from = 0;
        while let Some(relative) = lower[from..].find(&wanted_lower) {
            let start = from + relative;
            let end = start + wanted.len();
            let before_ok = start == 0 || !is_ident_continue(text.as_bytes()[start - 1]);
            let after_ok = end == text.len() || !is_ident_continue(text.as_bytes()[end]);
            if before_ok && after_ok {
                let mut open = end;
                while open < text.len() && text.as_bytes()[open].is_ascii_whitespace() {
                    open += 1;
                }
                if text.as_bytes().get(open) == Some(&b'(') {
                    if let Some(close) = matching_paren(text, open) {
                        calls.push(Call {
                            name: wanted.to_string(),
                            start,
                            args: split_arguments(text, open + 1, close),
                        });
                    }
                }
            }
            from = end;
        }
    }
    calls.sort_by_key(|call| call.start);
    calls
}

fn split_arguments(text: &str, start: usize, end: usize) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let (mut quote, mut depth, mut arg_start) = (None, 0i32, start);
    let mut args = Vec::new();
    let mut index = start;
    while index < end {
        let ch = bytes[index];
        if let Some(q) = quote {
            if ch == b'\\' {
                index += 2;
                continue;
            }
            if ch == q {
                quote = None;
            }
        } else if ch == b'\'' || ch == b'"' {
            quote = Some(ch);
        } else if matches!(ch, b'(' | b'[' | b'{') {
            depth += 1;
        } else if matches!(ch, b')' | b']' | b'}') {
            depth -= 1;
        } else if ch == b',' && depth == 0 {
            args.push(trim_span(text, arg_start, index));
            arg_start = index + 1;
        }
        index += 1;
    }
    if arg_start < end {
        args.push(trim_span(text, arg_start, end));
    }
    args
}

fn string_literal(text: &str, start: usize, end: usize) -> Option<StringValue> {
    let bytes = text.as_bytes();
    let mut cursor = start;
    while cursor < end && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    let quote = *bytes.get(cursor)?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let value_start = cursor + 1;
    let mut value_end = value_start;
    while value_end < end {
        if bytes[value_end] == b'\\' {
            value_end += 2;
            continue;
        }
        if bytes[value_end] == quote {
            return Some(StringValue {
                value: text[value_start..value_end].to_string(),
                start: value_start,
                end: value_end,
            });
        }
        value_end += 1;
    }
    None
}

fn array_keys(text: &str, start: usize, end: usize) -> Vec<(String, usize, usize)> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut cursor = start;
    while cursor < end {
        if bytes[cursor] == b'\'' || bytes[cursor] == b'"' {
            if let Some(value) = string_literal(text, cursor, end) {
                let mut after = value.end + 1;
                while after < end && bytes[after].is_ascii_whitespace() {
                    after += 1;
                }
                if text[after..end].starts_with("=>") {
                    result.push((value.value, value.start, value.end));
                }
                cursor = value.end + 1;
                continue;
            }
        }
        cursor += 1;
    }
    result
}

fn is_builder_add(text: &str, call_start: usize) -> bool {
    let before = text[..call_start].trim_end();
    let Some(before_arrow) = before.strip_suffix("->") else {
        return false;
    };
    before_arrow.trim_end().ends_with("$builder")
}

fn get_tag_returns(text: &str) -> Vec<StringValue> {
    let lower = text.to_ascii_lowercase();
    let mut result = Vec::new();
    let mut from = 0;
    while let Some(relative) = lower[from..].find("gettag") {
        let start = from + relative;
        let Some(open) = text[start..].find('{').map(|at| start + at) else {
            break;
        };
        let Some(close) = matching_brace(text, open) else {
            break;
        };
        let body = &text[open + 1..close];
        if let Some(return_at) = body.find("return") {
            let value_start = open + 1 + return_at + "return".len();
            if let Some(value) = string_literal(text, value_start, close) {
                result.push(value);
            }
        }
        from = close.saturating_add(1);
    }
    result
}

fn component_template(text: &str, attribute_start: usize) -> Option<StringValue> {
    let attribute_open = text[..attribute_start].rfind("#[")?;
    let attribute_end = text[attribute_start..]
        .find(']')
        .map(|offset| attribute_start + offset)?;
    let source = &text[attribute_open..attribute_end];
    let template_key = source.find("template")? + "template".len();
    let value_start = attribute_open + template_key;
    let colon = text[value_start..attribute_end].find(':')? + value_start + 1;
    string_literal(text, colon, attribute_end)
}

fn twig_component_attributes(text: &str) -> Vec<(usize, Option<StringValue>)> {
    let lower = text.to_ascii_lowercase();
    let mut result = Vec::new();
    let mut from = 0;
    while let Some(relative) = lower[from..].find("astwigcomponent") {
        let start = from + relative;
        let attribute_prefix = text[..start].trim_end();
        if attribute_prefix.ends_with("#[") {
            let mut after = start + "AsTwigComponent".len();
            while after < text.len() && text.as_bytes()[after].is_ascii_whitespace() {
                after += 1;
            }
            let name = if text.as_bytes().get(after) == Some(&b'(') {
                matching_paren(text, after).and_then(|close| {
                    split_arguments(text, after + 1, close)
                        .first()
                        .and_then(|arg| string_literal(text, arg.0, arg.1))
                })
            } else {
                None
            };
            result.push((start, name));
        }
        from = start + "AsTwigComponent".len();
    }
    result
}

fn twig_tag_body(line: &str) -> Option<(&str, usize)> {
    let open = line.find("{%")? + 2;
    let body = &line[open..];
    let leading = body.len() - body.trim_start().len();
    let start = open + leading;
    let name = line[start..]
        .split(|ch: char| !is_ident_continue(ch as u8))
        .next()?;
    (!name.is_empty()).then_some((name, start))
}

fn twig_expression_bounds(line: &str) -> Option<(usize, usize)> {
    let open = line.find("{{")? + 2;
    let close = line[open..].find("}}")? + open;
    Some((open, close))
}

fn statement_bounds(line: &str) -> Option<(usize, usize)> {
    let open = line.find("%{").or_else(|| line.find("{%"))?;
    let open = open + 2;
    let close = line[open..].find("%}")? + open;
    Some((open, close))
}

fn in_twig_expression(text: &str, offset: usize) -> bool {
    let before = &text[..offset];
    before
        .rfind("{{")
        .is_some_and(|open| before[open + 2..].rfind("}}").is_none())
        || before
            .rfind("{% ")
            .is_some_and(|open| before[open + 3..].rfind("%}").is_none())
        || before
            .rfind("{%")
            .is_some_and(|open| before[open + 2..].rfind("%}").is_none())
}

fn matching_paren(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut quote = None;
    let mut cursor = open;
    while cursor < bytes.len() {
        let ch = bytes[cursor];
        if let Some(q) = quote {
            if ch == b'\\' {
                cursor += 2;
                continue;
            }
            if ch == q {
                quote = None;
            }
        } else if ch == b'\'' || ch == b'"' {
            quote = Some(ch);
        } else if ch == b'(' {
            depth += 1;
        } else if ch == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(cursor);
            }
        }
        cursor += 1;
    }
    None
}

fn matching_brace(text: &str, open: usize) -> Option<usize> {
    if open >= text.len() || text.as_bytes()[open] != b'{' {
        return None;
    }
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut quote = None;
    let mut cursor = open;
    while cursor < bytes.len() {
        let ch = bytes[cursor];
        if let Some(q) = quote {
            if ch == b'\\' {
                cursor += 2;
                continue;
            }
            if ch == q {
                quote = None;
            }
        } else if ch == b'\'' || ch == b'"' {
            quote = Some(ch);
        } else if ch == b'{' {
            depth += 1;
        } else if ch == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(cursor);
            }
        }
        cursor += 1;
    }
    None
}

fn keyword_at(bytes: &[u8], cursor: usize, end: usize, word: &[u8]) -> bool {
    cursor + word.len() <= end
        && bytes[cursor..cursor + word.len()].eq_ignore_ascii_case(word)
        && (cursor == 0 || !is_ident_continue(bytes[cursor - 1]))
        && (cursor + word.len() == end || !is_ident_continue(bytes[cursor + word.len()]))
}

fn is_ident_start(ch: u8) -> bool {
    ch.is_ascii_alphabetic() || ch == b'_'
}
fn is_ident_continue(ch: u8) -> bool {
    ch.is_ascii_alphanumeric() || ch == b'_'
}

fn trim_span(text: &str, start: usize, end: usize) -> (usize, usize) {
    let slice = &text[start..end];
    let left = slice.len() - slice.trim_start().len();
    let right = slice.trim_end().len();
    (start + left, start + right)
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn line_for_offset(lines: &[usize], offset: usize) -> usize {
    lines
        .partition_point(|start| *start <= offset)
        .saturating_sub(1)
}

fn line_end(text: &str, start: usize) -> usize {
    text[start..]
        .find('\n')
        .map(|at| start + at)
        .unwrap_or(text.len())
}

fn line_slice<'a>(text: &'a str, lines: &[usize], offset: usize) -> &'a str {
    let line = line_for_offset(lines, offset);
    &text[lines[line]..line_end(text, lines[line])]
}

fn range_for(text: &str, lines: &[usize], start: usize, end: usize) -> Range {
    let position = |offset: usize| {
        let line = line_for_offset(lines, offset);
        let start = lines[line];
        let column = text[start..offset].encode_utf16().count() as u32;
        Position {
            line: line as u32,
            character: column,
        }
    };
    Range {
        start: position(start),
        end: position(end),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_text(path: &str, text: &str) -> Vec<Entity> {
        let path = Path::new(path);
        let uri = Url::from_file_path(path).unwrap();
        parse(Path::new("/tmp"), path, text, &uri)
    }

    #[test]
    fn indexes_form_types_fields_and_options_with_exact_name_ranges() {
        let entities = parse_text("/tmp/MessageType.php", "<?php\nnamespace App\\Form;\nclass MessageType extends AbstractType {\n  public function buildForm($builder, array $options) {\n    $builder->add('subject', TextType::class, ['required' => false]);\n  }\n}\n");
        let field = entities
            .iter()
            .find(|entity| entity.kind == EntityKind::FormField)
            .unwrap();
        assert_eq!(field.name, "subject");
        assert_eq!(
            field.qualified_name.as_deref(),
            Some("App\\Form\\MessageType")
        );
        assert_eq!(field.range.end.character - field.range.start.character, 7);
        let option = entities
            .iter()
            .find(|entity| entity.kind == EntityKind::FormOption)
            .unwrap();
        assert_eq!(option.name, "required");
    }

    #[test]
    fn indexes_form_data_class_for_entity_navigation() {
        let entities = parse_text(
            "/tmp/UserType.php",
            "<?php\nnamespace App\\Form;\nuse App\\Entity\\User;\nclass UserType extends AbstractType {\n public function configureOptions($resolver) {\n  $resolver->setDefaults(['data_class' => User::class]);\n }\n}\n",
        );
        let form_type = entities
            .iter()
            .find(|entity| entity.kind == EntityKind::FormType)
            .unwrap();
        assert_eq!(
            form_type.metadata.get("data_class").map(String::as_str),
            Some("App\\Entity\\User")
        );
        assert!(entities.iter().any(|entity| {
            entity.kind == EntityKind::Class
                && entity
                    .metadata
                    .get("reference")
                    .is_some_and(|value| value == "true")
                && entity.qualified_name.as_deref() == Some("App\\Entity\\User")
        }));
    }

    #[test]
    fn indexes_twig_registrations_and_component_props() {
        let entities = parse_text("/tmp/Alert.php", "<?php\nnamespace App\\Twig;\n#[AsTwigComponent('alert', template: 'components/custom-alert.html.twig')]\nclass Alert {\n  public string $title;\n}\n#[AsTwigComponent('card')]\nclass Card {}\nclass MyExtension extends AbstractExtension {\n  public function getFunctions() { return [new TwigFunction('price_label', $this)]; }\n}\n");
        let alert = entities
            .iter()
            .find(|item| item.kind == EntityKind::TwigComponent && item.name == "alert")
            .unwrap();
        assert_eq!(
            alert.metadata.get("template").map(String::as_str),
            Some("components/custom-alert.html.twig")
        );
        assert!(entities.iter().any(|item| {
            item.kind == EntityKind::TwigTemplate
                && item.name == "components/custom-alert.html.twig"
                && item
                    .metadata
                    .get("reference")
                    .is_some_and(|value| value == "true")
        }));
        let card = entities
            .iter()
            .find(|item| item.kind == EntityKind::TwigComponent && item.name == "card")
            .unwrap();
        assert_eq!(
            card.metadata.get("template").map(String::as_str),
            Some("components/card.html.twig")
        );
        assert!(entities
            .iter()
            .any(|item| item.kind == EntityKind::TwigComponentProp && item.name == "title"));
        assert!(entities
            .iter()
            .any(|item| item.kind == EntityKind::TwigFunction && item.name == "price_label"));
        assert!(entities
            .iter()
            .any(|item| item.kind == EntityKind::TwigExtension));
    }

    #[test]
    fn twig_references_only_expression_calls_and_component_use() {
        let entities = parse_text("/tmp/page.html.twig", "{{ price_label(item.price)|money_format is currency }}\n<twig:alert title=\"Hello\" />\n{% if active %}yes{% endif %}\n");
        let references = entities
            .iter()
            .filter(|item| {
                item.metadata
                    .get("reference")
                    .is_some_and(|value| value == "true")
            })
            .collect::<Vec<_>>();
        assert!(references
            .iter()
            .any(|item| item.kind == EntityKind::TwigFunction && item.name == "price_label"));
        assert!(references
            .iter()
            .any(|item| item.kind == EntityKind::TwigFilter && item.name == "money_format"));
        assert!(references
            .iter()
            .any(|item| item.kind == EntityKind::TwigTest && item.name == "currency"));
        assert!(references
            .iter()
            .any(|item| item.kind == EntityKind::TwigComponent && item.name == "alert"));
        assert!(references
            .iter()
            .any(|item| item.kind == EntityKind::TwigComponentProp && item.name == "title"));
        assert!(!references
            .iter()
            .any(|item| item.kind == EntityKind::TwigTag && item.name == "if"));
    }
}
