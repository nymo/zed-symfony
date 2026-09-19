use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Conventional Symfony directories that carry framework metadata rather than PHP classes.
const CONVENTIONAL_METADATA_DIRECTORIES: [&str; 5] =
    ["config", "templates", "translations", "public", "bin"];

/// Conventional PHP source directory used by Symfony Flex applications.
const CONVENTIONAL_SOURCE_DIRECTORY: &str = "src";

/// Composer packages that identify a Symfony application.
const SYMFONY_APPLICATION_PACKAGES: [&str; 2] = ["symfony/framework-bundle", "symfony/symfony"];

/// Files that identify a Symfony application when Composer metadata is absent or inconclusive.
const SYMFONY_STRUCTURE_MARKERS: [&str; 4] = [
    "config/bundles.php",
    "src/Kernel.php",
    "bin/console",
    "symfony.lock",
];

/// Directories that never contain application source and are skipped while scanning.
const SKIPPED_DIRECTORIES: [&str; 3] = ["vendor", "node_modules", "var"];

/// How confidently the workspace was identified as a Symfony application.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProjectKind {
    /// Composer or structural markers identify a Symfony application.
    Symfony,
    /// Composer metadata is present and clearly describes a non-Symfony project.
    NotSymfony,
    /// There is not enough information to decide, so Symfony behavior stays enabled.
    #[default]
    Unknown,
}

/// A detected Symfony major/minor version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SymfonyVersion {
    pub major: u32,
    pub minor: u32,
}

impl SymfonyVersion {
    /// Whether the version is within the supported release range (6.4 and newer).
    pub fn is_supported(self) -> bool {
        self.major > 6 || (self.major == 6 && self.minor >= 4)
    }
}

/// Composer-derived project metadata used for detection and source discovery.
#[derive(Clone, Debug, Default)]
pub struct ComposerProject {
    pub kind: ProjectKind,
    /// Directories that may contain PHP classes, relative to the project root.
    pub source_roots: Vec<PathBuf>,
    /// Required package names, used for optional-component feature detection.
    pub packages: BTreeSet<String>,
    /// Detected Symfony version from Composer metadata, when it can be determined.
    pub symfony_version: Option<SymfonyVersion>,
}

impl ComposerProject {
    /// Reads `composer.json` from `root` and derives detection and source roots.
    pub fn load(root: &Path) -> Self {
        let mut project = Self::default();
        if let Ok(text) = fs::read_to_string(root.join("composer.json")) {
            if let Ok(composer) = serde_json::from_str::<serde_json::Value>(&text) {
                project.collect_packages(&composer);
                project.collect_source_roots(&composer);
            }
        }
        project.kind = project.detect_kind(root);
        project.symfony_version = project.detect_version(root);
        project
    }

    pub fn is_symfony(&self) -> bool {
        self.kind == ProjectKind::Symfony
    }

    pub fn has_package(&self, name: &str) -> bool {
        self.packages.contains(name)
    }

    /// Directories to scan, combining conventional Symfony paths with Composer autoload roots.
    pub fn scan_directories(&self) -> Vec<PathBuf> {
        let mut directories = Vec::with_capacity(self.source_roots.len() + 6);
        directories.push(PathBuf::from(CONVENTIONAL_SOURCE_DIRECTORY));
        directories.extend(self.source_roots.iter().cloned());
        directories.extend(CONVENTIONAL_METADATA_DIRECTORIES.iter().map(PathBuf::from));
        directories
    }

    /// Whether a directory should be skipped while scanning source roots.
    pub fn is_skipped_directory(name: &str) -> bool {
        SKIPPED_DIRECTORIES.contains(&name)
    }

    fn collect_packages(&mut self, composer: &serde_json::Value) {
        for section in ["require", "require-dev"] {
            if let Some(packages) = composer.get(section).and_then(|value| value.as_object()) {
                self.packages.extend(packages.keys().cloned());
            }
        }
    }

    fn collect_source_roots(&mut self, composer: &serde_json::Value) {
        for section in ["autoload", "autoload-dev"] {
            let Some(autoload) = composer.get(section) else {
                continue;
            };
            for strategy in ["psr-4", "psr-0"] {
                let Some(mapping) = autoload.get(strategy).and_then(|value| value.as_object())
                else {
                    continue;
                };
                for paths in mapping.values() {
                    match paths {
                        serde_json::Value::String(path) => self.push_source_root(path),
                        serde_json::Value::Array(items) => {
                            for item in items {
                                if let Some(path) = item.as_str() {
                                    self.push_source_root(path);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn push_source_root(&mut self, path: &str) {
        let trimmed = path.trim().trim_matches('/');
        if trimmed.is_empty() {
            return;
        }
        let root = PathBuf::from(trimmed);
        if !self.source_roots.contains(&root) {
            self.source_roots.push(root);
        }
    }

    fn detect_kind(&self, root: &Path) -> ProjectKind {
        if self
            .packages
            .iter()
            .any(|package| SYMFONY_APPLICATION_PACKAGES.contains(&package.as_str()))
        {
            return ProjectKind::Symfony;
        }
        if SYMFONY_STRUCTURE_MARKERS
            .iter()
            .any(|marker| root.join(marker).exists())
        {
            return ProjectKind::Symfony;
        }
        if self.packages.is_empty() {
            ProjectKind::Unknown
        } else {
            ProjectKind::NotSymfony
        }
    }

    /// Detects the installed Symfony version from `composer.lock`, falling back to
    /// the `composer.json` constraint. Returns `None` when it cannot be determined
    /// confidently, so version-sensitive behavior stays disabled.
    fn detect_version(&self, root: &Path) -> Option<SymfonyVersion> {
        Self::version_from_lock(root).or_else(|| Self::version_from_manifest(root))
    }

    fn version_from_lock(root: &Path) -> Option<SymfonyVersion> {
        let text = fs::read_to_string(root.join("composer.lock")).ok()?;
        let lock = serde_json::from_str::<serde_json::Value>(&text).ok()?;
        for section in ["packages", "packages-dev"] {
            let Some(packages) = lock.get(section).and_then(|value| value.as_array()) else {
                continue;
            };
            for package in packages {
                if !matches!(
                    package.get("name").and_then(|value| value.as_str()),
                    Some("symfony/framework-bundle" | "symfony/symfony")
                ) {
                    continue;
                }
                if let Some(version) = package
                    .get("version")
                    .and_then(|value| value.as_str())
                    .and_then(parse_major_minor)
                {
                    return Some(version);
                }
            }
        }
        None
    }

    fn version_from_manifest(root: &Path) -> Option<SymfonyVersion> {
        let text = fs::read_to_string(root.join("composer.json")).ok()?;
        let composer = serde_json::from_str::<serde_json::Value>(&text).ok()?;
        for section in ["require", "require-dev"] {
            let Some(requirements) = composer.get(section).and_then(|value| value.as_object())
            else {
                continue;
            };
            for package in ["symfony/framework-bundle", "symfony/symfony"] {
                if let Some(version) = requirements
                    .get(package)
                    .and_then(|value| value.as_str())
                    .and_then(parse_major_minor)
                {
                    return Some(version);
                }
            }
        }
        None
    }
}

/// Extracts the first `major.minor` numeric pair from a version or constraint.
fn parse_major_minor(value: &str) -> Option<SymfonyVersion> {
    let mut numbers: Vec<String> = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if character.is_ascii_digit() {
            current.push(character);
        } else if !current.is_empty() {
            numbers.push(std::mem::take(&mut current));
            if numbers.len() == 2 {
                break;
            }
        }
    }
    if numbers.len() < 2 && !current.is_empty() {
        numbers.push(current);
    }
    if numbers.len() < 2 {
        return None;
    }
    Some(SymfonyVersion {
        major: numbers[0].parse().ok()?,
        minor: numbers[1].parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_project(label: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "symfony-project-detection-{}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        for (relative, contents) in files {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        root
    }

    #[test]
    fn detects_symfony_from_framework_bundle() {
        let root = write_project(
            "framework-bundle",
            &[(
                "composer.json",
                r#"{"require": {"php": ">=8.1", "symfony/framework-bundle": "6.4.*"}}"#,
            )],
        );
        let project = ComposerProject::load(&root);
        assert_eq!(project.kind, ProjectKind::Symfony);
        assert!(project.is_symfony());
        assert!(project.has_package("symfony/framework-bundle"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_symfony_from_structure_without_composer() {
        let root = write_project(
            "structure",
            &[("config/bundles.php", "<?php\nreturn [];\n")],
        );
        let project = ComposerProject::load(&root);
        assert_eq!(project.kind, ProjectKind::Symfony);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_non_symfony_composer_project() {
        let root = write_project(
            "non-symfony",
            &[(
                "composer.json",
                r#"{"require": {"php": ">=8.1", "monolog/monolog": "^3"}}"#,
            )],
        );
        let project = ComposerProject::load(&root);
        assert_eq!(project.kind, ProjectKind::NotSymfony);
        assert!(!project.is_symfony());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reports_unknown_without_composer_or_structure() {
        let root = write_project("unknown", &[("README.md", "hello")]);
        let project = ComposerProject::load(&root);
        assert_eq!(project.kind, ProjectKind::Unknown);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collects_psr4_and_psr0_source_roots_from_strings_and_arrays() {
        let root = write_project(
            "psr-roots",
            &[(
                "composer.json",
                r#"{
                "autoload": {
                    "psr-4": {"App\\": "src/", "Domain\\": ["lib/Domain", "lib/Shared"]},
                    "psr-0": {"Legacy_": "legacy"}
                },
                "autoload-dev": {"psr-4": {"App\\Tests\\": "tests/"}}
            }"#,
            )],
        );
        let project = ComposerProject::load(&root);
        assert_eq!(
            project.source_roots,
            vec![
                PathBuf::from("src"),
                PathBuf::from("lib/Domain"),
                PathBuf::from("lib/Shared"),
                PathBuf::from("legacy"),
                PathBuf::from("tests"),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scan_directories_include_conventional_and_composer_roots() {
        let root = write_project(
            "scan-directories",
            &[(
                "composer.json",
                r#"{"autoload": {"psr-4": {"App\\": "lib/"}}}"#,
            )],
        );
        let project = ComposerProject::load(&root);
        let directories = project.scan_directories();
        assert!(directories.contains(&PathBuf::from("src")));
        assert!(directories.contains(&PathBuf::from("lib")));
        assert!(directories.contains(&PathBuf::from("config")));
        assert!(directories.contains(&PathBuf::from("templates")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_symfony_version_from_lock() {
        let root = write_project(
            "version-lock",
            &[(
                "composer.lock",
                r#"{"packages":[{"name":"symfony/framework-bundle","version":"v6.4.7"}],"packages-dev":[]}"#,
            )],
        );
        let project = ComposerProject::load(&root);
        assert_eq!(
            project.symfony_version,
            Some(SymfonyVersion { major: 6, minor: 4 })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_symfony_version_from_manifest_constraint() {
        let root = write_project(
            "version-manifest",
            &[(
                "composer.json",
                r#"{"require":{"symfony/framework-bundle":"^7.4"}}"#,
            )],
        );
        let project = ComposerProject::load(&root);
        assert_eq!(
            project.symfony_version,
            Some(SymfonyVersion { major: 7, minor: 4 })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prefers_lock_version_over_manifest() {
        let root = write_project(
            "version-precedence",
            &[
                (
                    "composer.json",
                    r#"{"require":{"symfony/framework-bundle":"6.4.*"}}"#,
                ),
                (
                    "composer.lock",
                    r#"{"packages":[{"name":"symfony/framework-bundle","version":"8.1.0"}]}"#,
                ),
            ],
        );
        let project = ComposerProject::load(&root);
        assert_eq!(
            project.symfony_version,
            Some(SymfonyVersion { major: 8, minor: 1 })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reports_unknown_version_without_metadata() {
        let root = write_project(
            "version-unknown",
            &[("config/bundles.php", "<?php\nreturn [];\n")],
        );
        let project = ComposerProject::load(&root);
        assert_eq!(project.kind, ProjectKind::Symfony);
        assert_eq!(project.symfony_version, None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn classifies_supported_and_unsupported_versions() {
        for (value, expected) in [
            ("6.4.*", Some((6, 4))),
            ("^7.4", Some((7, 4))),
            (">=8.1", Some((8, 1))),
            ("v6.4.7", Some((6, 4))),
            ("dev-main", None),
        ] {
            let parsed = parse_major_minor(value);
            assert_eq!(
                parsed.map(|version| (version.major, version.minor)),
                expected,
                "unexpected parse for {value:?}"
            );
        }
        assert!((SymfonyVersion { major: 6, minor: 4 }).is_supported());
        assert!((SymfonyVersion { major: 7, minor: 4 }).is_supported());
        assert!((SymfonyVersion { major: 8, minor: 1 }).is_supported());
        assert!(!(SymfonyVersion { major: 6, minor: 3 }).is_supported());
        assert!(!(SymfonyVersion { major: 5, minor: 4 }).is_supported());
    }
}
