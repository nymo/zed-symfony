use std::fs;
use std::path::PathBuf;

use zed_extension_api::{self as zed, Result};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const PATH_BINARY_NAME: &str = "symfony-lsp";
const DEVELOPMENT_VERSION: &str = "0.1.0";
const RELEASE_REPOSITORY: &str = "nymo/zed-symfony";

struct SymfonyExtension {
    cached_binary_path: Option<String>,
}

impl zed::Extension for SymfonyExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let binary_path = self.language_server_binary_path(worktree)?;

        Ok(zed::Command {
            command: binary_path,
            args: Vec::new(),
            env: worktree.shell_env(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(
            zed::settings::LspSettings::for_worktree("symfony-lsp", worktree)
                .ok()
                .and_then(|settings| settings.initialization_options),
        )
    }

    fn language_server_workspace_configuration(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(
            zed::settings::LspSettings::for_worktree("symfony-lsp", worktree)
                .ok()
                .and_then(|settings| settings.settings),
        )
    }
}

impl SymfonyExtension {
    fn language_server_binary_path(&mut self, worktree: &zed::Worktree) -> Result<String> {
        if let Some(cached_path) = &self.cached_binary_path {
            if fs::metadata(cached_path).is_ok() {
                return Ok(cached_path.clone());
            }
        }

        let binary_name = Self::platform_binary_name_for_current_host();

        // Dev extensions are commonly installed from a checkout before a
        // GitHub release exists. Resolve the workspace build directly so a
        // locally built server does not fall through to a guaranteed 404.
        if let Some(path) = Self::local_workspace_binary(worktree) {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        let version_dir = format!("symfony-lsp-{VERSION}");
        let binary_path = format!("{version_dir}/{binary_name}");

        if fs::metadata(&binary_path).is_ok() {
            self.cached_binary_path = Some(binary_path.clone());
            return Ok(binary_path);
        }

        // A PATH binary is useful for local development and for users who
        // package the server themselves. It also avoids requiring a release
        // asset while the extension is being developed.
        if let Some(path) = worktree.which(&binary_name) {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }
        if binary_name != PATH_BINARY_NAME {
            if let Some(path) = worktree.which(PATH_BINARY_NAME) {
                self.cached_binary_path = Some(path.clone());
                return Ok(path);
            }
        }

        let downloaded_path = self.download_binary(&binary_name, &version_dir)?;
        self.cached_binary_path = Some(downloaded_path.clone());
        Ok(downloaded_path)
    }

    fn local_workspace_binary(worktree: &zed::Worktree) -> Option<String> {
        let binary_name = if matches!(zed::current_platform().0, zed::Os::Windows) {
            "symfony-lsp.exe"
        } else {
            PATH_BINARY_NAME
        };
        let root = PathBuf::from(worktree.root_path());

        // The repository is a Cargo workspace, so the normal output directory
        // is rooted beside Cargo.toml. Keep the nested path as a fallback for
        // checkouts that build the LSP crate independently.
        let roots = [root.clone(), root.join("symfony-lsp")];
        let mut first_candidate = None;
        for project_root in roots {
            for profile in ["debug", "release"] {
                let path = project_root.join("target").join(profile).join(binary_name);
                let path_string = path.to_string_lossy().into_owned();
                if first_candidate.is_none() {
                    first_candidate = Some(path_string.clone());
                }
                if fs::metadata(&path).is_ok() {
                    return Some(path_string);
                }
            }
        }

        // WASM extensions cannot always stat an absolute host path even though
        // Zed can launch that path. Version 0.1.0 is the dev-extension version,
        // so return the conventional workspace debug path as a final fallback
        // instead of attempting a release download that does not exist yet.
        // Only use the conventional fallback inside this extension's own
        // checkout. For another project, returning a nonexistent
        // `<project>/target/...` path prevents the PATH lookup below from
        // being useful and makes Zed report an opaque spawn error.
        if VERSION == DEVELOPMENT_VERSION && root.join("extension.toml").is_file() {
            first_candidate
        } else {
            None
        }
    }

    fn download_binary(&self, binary_name: &str, version_dir: &str) -> Result<String> {
        let binary_path = format!("{version_dir}/{binary_name}");
        if fs::metadata(&binary_path).is_ok() {
            return Ok(binary_path);
        }

        let (os, _) = zed::current_platform();
        let archive_ext = match os {
            zed::Os::Windows => "zip",
            _ => "tar.gz",
        };
        let archive_name = format!("{binary_name}.{archive_ext}");
        let release_url = format!(
            "https://github.com/{RELEASE_REPOSITORY}/releases/download/{VERSION}/{archive_name}"
        );
        let file_type = match os {
            zed::Os::Windows => zed::DownloadedFileType::Zip,
            _ => zed::DownloadedFileType::GzipTar,
        };

        zed::download_file(&release_url, version_dir, file_type).map_err(|error| {
            format!("Failed to download Symfony LSP binary from {release_url}: {error}. For local development, run `cargo build -p symfony-lsp` or publish the matching release asset.")
        })?;

        if fs::metadata(&binary_path).is_err() {
            return Err(format!(
                "Symfony LSP binary was not found after extraction; expected {binary_path}"
            ));
        }

        zed::make_file_executable(&binary_path)
            .map_err(|error| format!("Failed to make Symfony LSP executable: {error}"))?;

        Ok(binary_path)
    }

    fn platform_binary_name_for_current_host() -> String {
        let (os, architecture) = zed::current_platform();
        Self::platform_binary_name(os, architecture)
    }

    fn platform_binary_name(os: zed::Os, architecture: zed::Architecture) -> String {
        match (os, architecture) {
            (zed::Os::Mac, zed::Architecture::Aarch64) => "symfony-lsp-macos-arm64".to_string(),
            (zed::Os::Mac, zed::Architecture::X8664) => "symfony-lsp-macos-x64".to_string(),
            (zed::Os::Windows, zed::Architecture::Aarch64) => {
                "symfony-lsp-windows-arm64.exe".to_string()
            }
            (zed::Os::Windows, zed::Architecture::X8664) => {
                "symfony-lsp-windows-x64.exe".to_string()
            }
            (zed::Os::Linux, zed::Architecture::Aarch64) => "symfony-lsp-linux-arm64".to_string(),
            (zed::Os::Linux, zed::Architecture::X8664) => "symfony-lsp-linux-x64".to_string(),
            (zed::Os::Mac, _) => "symfony-lsp-macos".to_string(),
            (zed::Os::Windows, _) => "symfony-lsp-windows.exe".to_string(),
            (zed::Os::Linux, _) => "symfony-lsp-linux".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zed::{Architecture, Os};

    #[test]
    fn platform_binary_names_are_stable() {
        assert_eq!(
            SymfonyExtension::platform_binary_name(Os::Mac, Architecture::Aarch64),
            "symfony-lsp-macos-arm64"
        );
        assert_eq!(
            SymfonyExtension::platform_binary_name(Os::Mac, Architecture::X8664),
            "symfony-lsp-macos-x64"
        );
        assert_eq!(
            SymfonyExtension::platform_binary_name(Os::Linux, Architecture::X8664),
            "symfony-lsp-linux-x64"
        );
        assert_eq!(
            SymfonyExtension::platform_binary_name(Os::Linux, Architecture::Aarch64),
            "symfony-lsp-linux-arm64"
        );
        assert_eq!(
            SymfonyExtension::platform_binary_name(Os::Windows, Architecture::X8664),
            "symfony-lsp-windows-x64.exe"
        );
        assert_eq!(
            SymfonyExtension::platform_binary_name(Os::Windows, Architecture::Aarch64),
            "symfony-lsp-windows-arm64.exe"
        );
    }
}

zed::register_extension!(SymfonyExtension);
