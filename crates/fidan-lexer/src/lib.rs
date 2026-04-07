//! `fidan-lexer` — Tokeniser, SynonymMap, SymbolInterner.
//!
//! ```
//! use std::sync::Arc;
//!
//! use fidan_lexer::{Lexer, SymbolInterner, TokenKind};
//! use fidan_source::{FileId, SourceFile};
//!
//! let file = SourceFile::new(
//!     FileId(0),
//!     "<doc>",
//!     "\"hello\nworld\"\nr\"alpha\nbeta\"",
//! );
//! let (tokens, diags) = Lexer::new(&file, Arc::new(SymbolInterner::new())).tokenise();
//!
//! assert!(diags.is_empty());
//! assert_eq!(tokens[0].kind, TokenKind::LitString("hello\nworld".to_string()));
//! assert_eq!(tokens[1].kind, TokenKind::Newline);
//! assert_eq!(tokens[2].kind, TokenKind::LitRawString("alpha\nbeta".to_string()));
//! ```

mod interner;
mod lexer;
mod synonyms;
mod token;

pub use interner::{Symbol, SymbolInterner};
pub use lexer::{ESCAPED_INTERP_CLOSE, ESCAPED_INTERP_OPEN, Lexer};
pub use token::{Token, TokenKind};
