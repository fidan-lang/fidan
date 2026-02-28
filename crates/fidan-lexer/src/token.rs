use crate::Symbol;
use fidan_source::Span;

/// Every distinct kind of token the Fidan lexer can produce.
///
/// Synonyms are resolved to their canonical `TokenKind` at lex time,
/// so the parser only ever sees canonical tokens.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // ── Literals ─────────────────────────────────────────────────────────────
    LitInteger(i64),
    LitFloat(f64),
    /// The string body *excluding* the surrounding quotes.
    /// Interpolation markers (`{...}`) are preserved as-is; the parser splits them.
    LitString(String),
    LitBool(bool),
    /// `nothing`
    Nothing,

    // ── Keywords ─────────────────────────────────────────────────────────────
    Var,
    Set,        // assignment: canonical form of `set`
    Action,
    Object,
    Extends,
    Return,
    If,
    Otherwise,
    When,
    For,
    In,
    While,
    Break,
    Continue,
    Attempt,    // `try`
    Catch,
    Finally,
    Panic,      // `throw`
    Use,        // imports
    As,
    Oftype,     // type annotation
    Required,
    Optional,
    Dynamic,
    Parallel,
    Concurrent,
    Task,
    Spawn,
    Await,
    Shared,
    Pending,
    Weak,       // WeakShared
    And,
    Or,
    Not,
    Is,         // `is` — part of `is not` (normalised to NotEq in parser)
    This,
    Parent,
    New,
    True,
    False,

    // ── Operators ────────────────────────────────────────────────────────────
    Assign,         // `=` | `set` (in assignment position)
    Eq,             // `==` | `is` | `equals`
    NotEq,          // `!=` | `notequals`
    Lt,             // `<`  | `lessthan`
    LtEq,           // `<=` | `lessthanorequals`
    Gt,             // `>`  | `greaterthan`
    GtEq,           // `>=` | `greaterthanorequals`
    Plus,           // `+`
    PlusEq,         // `+=`
    Minus,          // `-`
    MinusEq,        // `-=`
    Star,           // `*`
    StarEq,         // `*=`
    Slash,          // `/`
    SlashEq,        // `/=`
    Percent,        // `%`
    PercentEq,      // `%=`
    Caret,          // `^`
    NullCoalesce,   // `??`

    // ── Delimiters ───────────────────────────────────────────────────────────
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,          // `,` | `also`
    Dot,
    DotDot,         // `..`  range
    Colon,
    DoubleColon,    // `::`
    Arrow,          // `->`
    FatArrow,       // `=>`
    /// `;` | `stop` | `separate` — inline statement separator.
    Semicolon,
    /// Emitted at the end of a logical line (Go-style automatic insertion).
    Newline,
    Hash,           // `#` decorator prefix (the `@` is decorator, `#` is comment start — already consumed)
    At,             // `@` decorator prefix

    // ── Identifiers ──────────────────────────────────────────────────────────
    Ident(Symbol),

    // ── Special ──────────────────────────────────────────────────────────────
    Eof,
    /// Error recovery: an unexpected character was encountered.
    Unknown(char),
}

impl TokenKind {
    /// Returns `true` if a `Newline` should be emitted *after* this token
    /// (Go-style automatic newline insertion rules).
    pub fn terminates_statement(&self) -> bool {
        matches!(
            self,
            TokenKind::LitInteger(_)
                | TokenKind::LitFloat(_)
                | TokenKind::LitString(_)
                | TokenKind::LitBool(_)
                | TokenKind::Nothing
                | TokenKind::Ident(_)
                | TokenKind::RParen
                | TokenKind::RBrace
                | TokenKind::RBracket
                | TokenKind::Return
                | TokenKind::Break
                | TokenKind::Continue
                | TokenKind::This
                | TokenKind::Parent
                | TokenKind::True
                | TokenKind::False
        )
    }
}

/// A single lexed token: its kind plus the source span it covers.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}
