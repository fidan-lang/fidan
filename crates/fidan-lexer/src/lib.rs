//! `fidan-lexer` — Tokeniser, SynonymMap, SymbolInterner.

mod interner;
mod lexer;
mod synonyms;
mod token;

pub use interner::{Symbol, SymbolInterner};
pub use lexer::{ESCAPED_INTERP_CLOSE, ESCAPED_INTERP_OPEN, Lexer};
pub use token::{Token, TokenKind};
