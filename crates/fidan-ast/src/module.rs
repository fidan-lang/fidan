use crate::{AstArena, ItemId};
use fidan_source::FileId;

/// Root AST node for a single `.fdn` source file.
#[derive(Debug)]
pub struct Module {
    pub file:  FileId,
    pub items: Vec<ItemId>,
    pub arena: AstArena,
}

impl Module {
    pub fn new(file: FileId) -> Self {
        Self { file, items: Vec::new(), arena: AstArena::new() }
    }
}
