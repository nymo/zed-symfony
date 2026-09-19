use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use tower_lsp::jsonrpc;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use tracing::{debug, info, warn};

#[derive(Clone)]
pub struct SymfonyLanguageServer {
    client: Client,
    root_uri: Arc<RwLock<Option<Url>>>,
    documents: Arc<RwLock<HashMap<Url, (String, i32)>>>,
}

impl SymfonyLanguageServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            root_uri: Arc::new(RwLock::new(None)),
            documents: Arc::new(RwLock::new(HashMap::new())),
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

#[tower_lsp::async_trait]
impl LanguageServer for SymfonyLanguageServer {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        let root_uri = Self::workspace_root(&params);
        if let Ok(mut root) = self.root_uri.write() {
            *root = root_uri.clone();
        }

        if let Some(options) = params.initialization_options.as_ref() {
            info!("received initialization options: {options}");
        }
        info!("initializing Symfony LSP for workspace {:?}", root_uri);

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                definition_provider: Some(OneOf::Left(true)),
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
                "Symfony LSP initialized; static analysis is not enabled yet.",
            )
            .await;
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        info!("shutting down Symfony LSP");
        if let Ok(mut documents) = self.documents.write() {
            documents.clear();
        }
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        info!("opened {}", document.uri);
        if let Ok(mut documents) = self.documents.write() {
            documents.insert(document.uri, (document.text, document.version));
        }
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
        if let Ok(mut documents) = self.documents.write() {
            documents.insert(uri, (change.text, params.text_document.version));
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        debug!("saved {}", params.text_document.uri);
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        debug!("closed {}", params.text_document.uri);
        if let Ok(mut documents) = self.documents.write() {
            documents.remove(&params.text_document.uri);
        }
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        debug!("configuration changed: {}", params.settings);
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        debug!("received {} watched-file changes", params.changes.len());
    }

    async fn hover(&self, _: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        Ok(None)
    }

    async fn completion(&self, _: CompletionParams) -> jsonrpc::Result<Option<CompletionResponse>> {
        Ok(None)
    }

    async fn goto_definition(
        &self,
        _: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        Ok(None)
    }

    async fn did_change_workspace_folders(&self, _: DidChangeWorkspaceFoldersParams) {
        warn!("workspace folders changed; multi-root re-discovery is not implemented yet");
    }
}
