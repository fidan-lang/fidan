//! `fidan-parser` — Recursive-descent parser + Pratt expression parser.

mod parser;
mod pratt;
mod recovery;

pub use parser::Parser;

use fidan_ast::Module;
use fidan_diagnostics::Diagnostic;
use fidan_lexer::{SymbolInterner, Token};
use fidan_source::FileId;
use std::sync::Arc;

/// Parse a flat token stream into a [`Module`] AST.
///
/// Returns the completed module and any diagnostics produced during parsing.
/// Even when diagnostics are present the module is valid — `Expr::Error` /
/// `Stmt::Error` placeholders are inserted so downstream passes can continue.
pub fn parse(
    tokens: &[Token],
    file_id: FileId,
    interner: Arc<SymbolInterner>,
) -> (Module, Vec<Diagnostic>) {
    let mut p = Parser::new(tokens, file_id, interner);
    p.parse_module();
    p.finish()
}
