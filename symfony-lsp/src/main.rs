mod server;

use std::env;

use tokio::io::{stdin, stdout};
use tower_lsp::{LspService, Server};
use tracing::info;
use tracing_subscriber::EnvFilter;

use server::SymfonyLanguageServer;

#[tokio::main]
async fn main() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("symfony_lsp=info,tower_lsp=warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    info!(
        "Symfony LSP starting, version {}",
        env!("CARGO_PKG_VERSION")
    );

    let (service, socket) = LspService::new(SymfonyLanguageServer::new);
    Server::new(stdin(), stdout(), socket).serve(service).await;

    info!("Symfony LSP stopped");
}
