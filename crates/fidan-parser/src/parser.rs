// fidan-parser stubs — implementation in Phase 2
use fidan_ast::Module;
use fidan_source::SourceFile;
use fidan_lexer::SymbolInterner;
use std::sync::Arc;

pub struct Parser;
impl Parser {
    pub fn new(_file: &SourceFile, _interner: Arc<SymbolInterner>) -> Self { Parser }
    pub fn parse(self) -> Module { todo!("Phase 2: Parser not yet implemented") }
}
