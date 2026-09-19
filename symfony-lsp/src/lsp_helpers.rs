use crate::index::{Entity, EntityKind};

pub fn token_at(line: &str, character: usize) -> String {
    let end = byte_offset_for_utf16(line, character);
    let bytes = line.as_bytes();
    let mut start = end;
    while start > 0 && is_token_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut finish = end;
    while finish < bytes.len() && is_token_byte(bytes[finish]) {
        finish += 1;
    }
    line[start..finish]
        .trim_matches(['\'', '"', '(', ')', '{', '}', '%'])
        .to_string()
}

pub fn byte_offset_for_utf16(line: &str, character: usize) -> usize {
    let mut utf16_offset = 0;
    for (byte_offset, character_at_offset) in line.char_indices() {
        let next_offset = utf16_offset + character_at_offset.len_utf16();
        if character < next_offset {
            return byte_offset;
        }
        utf16_offset = next_offset;
        if character == utf16_offset {
            return byte_offset + character_at_offset.len_utf8();
        }
    }
    line.len()
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/' | b'{' | b'}' | b'%')
}

pub fn completion_prefix(line: &str, character: usize) -> String {
    token_at(line, character)
}

pub fn context_kind(line: &str) -> Option<EntityKind> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("autowire")
        || lower.contains("service(")
        || lower.contains("arguments:")
        || lower.contains("bind:")
        || lower.contains("type=\"service\"")
        || lower.contains("type='service'")
        || lower.contains("'@")
        || lower.contains("\"@")
        || lower.trim_start().starts_with("- @")
    {
        Some(EntityKind::Service)
    } else if lower.contains("%env(") || lower.contains("env(") {
        Some(EntityKind::EnvironmentVariable)
    } else if lower.contains("path(")
        || lower.contains("url(")
        || lower.contains("redirecttoroute(")
        || lower.contains("generateurl(")
        || lower.contains("generate(")
        || lower.contains("route(")
    {
        Some(EntityKind::Route)
    } else if lower.contains("astwigcomponent") {
        Some(EntityKind::TwigComponent)
    } else if lower.contains("include(")
        || lower.contains("{% include")
        || lower.contains("extends ")
        || lower.contains("{% extends")
        || lower.contains("embed(")
        || lower.contains("{% embed")
        || lower.contains("render(")
        || lower.contains("template")
    {
        Some(EntityKind::TwigTemplate)
    } else if lower.contains("trans(") || lower.contains("|trans") || lower.contains("translation")
    {
        Some(EntityKind::TranslationKey)
    } else if lower.contains("data_class") {
        Some(EntityKind::Class)
    } else if lower.contains("targetentity") {
        Some(EntityKind::DoctrineEntity)
    } else if lower.contains("<twig:") {
        Some(EntityKind::TwigComponent)
    } else if lower.contains("form.") {
        Some(EntityKind::FormField)
    } else if lower.contains("->add(") && lower.contains("['") {
        Some(EntityKind::FormOption)
    } else if lower.contains("->add(") {
        Some(EntityKind::FormField)
    } else if lower.contains("andwhere(")
        || lower.contains("orwhere(")
        || lower.contains("orderby(")
        || lower.contains("groupby(")
    {
        Some(EntityKind::DoctrineField)
    } else if lower.contains("->from(") {
        Some(EntityKind::DoctrineEntity)
    } else if (lower.contains("{{") || lower.contains("{%")) && lower.contains('|') {
        Some(EntityKind::TwigFilter)
    } else if (lower.contains("{{") || lower.contains("{%")) && lower.contains(" is ") {
        Some(EntityKind::TwigTest)
    } else if (lower.contains("{{") || lower.contains("{%")) && lower.contains('(') {
        Some(EntityKind::TwigFunction)
    } else if lower.contains("{%") {
        Some(EntityKind::TwigTag)
    } else {
        None
    }
}

pub fn completion_name(entity: &Entity) -> String {
    if matches!(entity.kind, EntityKind::Class | EntityKind::Interface) {
        entity
            .qualified_name
            .clone()
            .unwrap_or_else(|| entity.name.clone())
    } else if entity.kind == EntityKind::Route {
        entity
            .metadata
            .get("route_name")
            .cloned()
            .unwrap_or_else(|| entity.name.clone())
    } else {
        entity.name.clone()
    }
}

pub fn entity_detail(entity: &Entity) -> String {
    match entity.kind {
        EntityKind::Route => {
            let path = entity
                .metadata
                .get("path")
                .map(String::as_str)
                .unwrap_or(&entity.name);
            let mut detail = format!("Symfony route `{}` → `{path}`", completion_name(entity));
            if let Some(controller) = entity.metadata.get("controller") {
                detail.push_str(&format!("\n\nController: `{controller}`"));
            }
            detail
        }
        EntityKind::TwigTemplate => format!("Twig template `{}`", entity.name),
        EntityKind::EnvironmentVariable => format!("Environment variable `{}`", entity.name),
        EntityKind::TranslationKey => entity
            .metadata
            .get("domain")
            .map(|domain| format!("Translation key `{}` (domain `{domain}`)", entity.name))
            .unwrap_or_else(|| format!("Translation key `{}`", entity.name)),
        EntityKind::Service => entity
            .metadata
            .get("class")
            .map(|class| format!("Symfony service `{}` for `{class}`", entity.name))
            .unwrap_or_else(|| format!("Symfony service `{}`", entity.name)),
        EntityKind::DoctrineEntity => format!(
            "Doctrine entity `{}`",
            entity.qualified_name.as_deref().unwrap_or(&entity.name)
        ),
        EntityKind::DoctrineField => entity
            .metadata
            .get("owner")
            .map(|owner| format!("Doctrine field `{}::{}`", owner, entity.name))
            .unwrap_or_else(|| format!("Doctrine field `{}`", entity.name)),
        EntityKind::FormType => format!(
            "Symfony form type `{}`",
            entity.qualified_name.as_deref().unwrap_or(&entity.name)
        ),
        EntityKind::FormField => format!("Symfony form field `{}`", entity.name),
        EntityKind::FormOption => format!("Symfony form option `{}`", entity.name),
        EntityKind::TwigExtension => format!(
            "Twig extension `{}`",
            entity.qualified_name.as_deref().unwrap_or(&entity.name)
        ),
        EntityKind::TwigFunction => format!("Twig function `{}`", entity.name),
        EntityKind::TwigFilter => format!("Twig filter `{}`", entity.name),
        EntityKind::TwigTest => format!("Twig test `{}`", entity.name),
        EntityKind::TwigTag => format!("Twig tag `{}`", entity.name),
        EntityKind::TwigComponent => entity
            .metadata
            .get("class")
            .map(|class| format!("Twig component `{}` (`{class}`)", entity.name))
            .unwrap_or_else(|| format!("Twig component `{}`", entity.name)),
        EntityKind::TwigComponentProp => entity
            .metadata
            .get("component")
            .map(|component| format!("Twig component property `{}` on `{component}`", entity.name))
            .unwrap_or_else(|| format!("Twig component property `{}`", entity.name)),
        _ => format!("Symfony {:?} `{}`", entity.kind, entity.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_existing_framework_contexts() {
        assert_eq!(context_kind("{{ path('home') }}"), Some(EntityKind::Route));
        assert_eq!(
            context_kind("{{ include('base.html.twig') }}"),
            Some(EntityKind::TwigTemplate)
        );
        assert_eq!(
            context_kind("env('DATABASE_URL')"),
            Some(EntityKind::EnvironmentVariable)
        );
        assert_eq!(
            context_kind("{{ 'welcome'|trans }}"),
            Some(EntityKind::TranslationKey)
        );
        assert_eq!(
            context_kind("        arguments: ['@']"),
            Some(EntityKind::Service)
        );
        assert_eq!(
            context_kind("#[Autowire(service: 'mailer')"),
            Some(EntityKind::Service)
        );
        assert_eq!(context_kind("service('mailer')"), Some(EntityKind::Service));
        assert_eq!(
            context_kind("{% include 'base.html.twig' %}"),
            Some(EntityKind::TwigTemplate)
        );
    }

    #[test]
    fn detects_doctrine_form_and_twig_contexts() {
        assert_eq!(
            context_kind("$qb->andWhere('u.name = :name')"),
            Some(EntityKind::DoctrineField)
        );
        assert_eq!(
            context_kind("$qb->from(User::class, 'u')"),
            Some(EntityKind::DoctrineEntity)
        );
        assert_eq!(
            context_kind("$builder->add('email', TextType::class, ['required' => true])"),
            Some(EntityKind::FormOption)
        );
        assert_eq!(
            context_kind("{{ price_label(item)|money_format is currency }}"),
            Some(EntityKind::TwigFilter)
        );
        assert_eq!(
            context_kind("{% if is_granted('ROLE_USER') %}"),
            Some(EntityKind::TwigFunction)
        );
        assert_eq!(context_kind("{% custom_tag %}"), Some(EntityKind::TwigTag));
        assert_eq!(
            context_kind("<twig:alert title=\"Hello\" />"),
            Some(EntityKind::TwigComponent)
        );
    }

    #[test]
    fn finds_token_at_cursor() {
        assert_eq!(token_at("path('admin.home')", 14), "admin.home");
    }

    #[test]
    fn token_ranges_use_utf16_offsets_with_non_ascii_prefixes() {
        let line = "é {{ render('home/index.html.twig') }}";
        let token_start = line.find("index").unwrap();
        let cursor = line[..token_start].encode_utf16().count() + 2;
        assert_eq!(token_at(line, cursor), "home/index.html.twig");
    }
}
