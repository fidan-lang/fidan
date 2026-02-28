//! `fidan-parser` — Recursive-descent parser + Pratt expression parser.

mod parser;
mod pratt;
mod recovery;

pub use parser::Parser;
