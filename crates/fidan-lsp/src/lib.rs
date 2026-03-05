//! `fidan-lsp` — Language Server Protocol server for the Fidan language.
//!
//! Entry point for the `fidan lsp` CLI subcommand:
//!
//! ```no_run
//! fidan_lsp::run();
//! ```

mod analysis;
mod convert;
mod document;
mod semantic;
mod server;
mod store;
pub mod symbols;

use server::FidanLsp;
use tower_lsp::{LspService, Server};

/// Start the LSP server reading from stdin and writing to stdout.
///
/// This function blocks until the editor sends a `shutdown` request.
pub fn run() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
        .block_on(async {
            let stdin = tokio::io::stdin();
            let stdout = tokio::io::stdout();
            let (service, socket) = LspService::new(FidanLsp::new);
            Server::new(stdin, stdout, socket).serve(service).await;
        });
}
