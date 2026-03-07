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
    /// `const` — immutable variable declaration modifier
    Const,
    /// `tuple` — generic untyped tuple type keyword
    Tuple,
    Set, // assignment: canonical form of `set`
    Action,
    Object,
    Extends,
    Return,
    If,
    Otherwise, // `else`
    When,
    For,
    In,
    While,
    Break, // `stop`
    Continue,
    Attempt, // `try`
    Catch,
    Finally,
    Panic,  // `throw`
    Check,  // pattern match statement (`check x { ... }`)
    Use,    // imports
    Export, // `export use`
    As,
    Oftype, // type annotation (`->` is also accepted in the parser)
    Certain,
    Optional,
    Dynamic, // surface keyword: `flexible` (alias: `dynamic`)
    Parallel,
    Concurrent,
    Task,
    Spawn,
    Await,
    Shared,
    Pending,
    Weak, // WeakShared
    /// `test` — top-level test block declaration
    Test,
    /// `enum` — enumeration type declaration
    Enum,
    And,
    Or,
    Not,
    Is, // `is` — part of `is not` (normalised to NotEq in parser)
    This,
    Parent,
    New,
    True,
    False,

    // ── Operators ────────────────────────────────────────────────────────────
    Assign,       // `=` | `set` (in assignment position)
    Eq,           // `==` | `is` | `equals`
    NotEq,        // `!=` | `notequals` | `is not` (two-token, folded in parser)
    Lt,           // `<`  | `lessthan`
    LtEq,         // `<=` | `lessthanorequals`
    Gt,           // `>`  | `greaterthan`
    GtEq,         // `>=` | `greaterthanorequals`
    Plus,         // `+`
    PlusEq,       // `+=`
    Minus,        // `-`
    MinusEq,      // `-=`
    Star,         // `*`
    StarEq,       // `*=`
    Slash,        // `/`
    SlashEq,      // `/=`
    Percent,      // `%`
    PercentEq,    // `%=`
    Caret,        // `^` (bitwise XOR)
    Ampersand,    // `&`  (bitwise AND)
    Pipe,         // `|`  (bitwise OR)
    LtLt,         // `<<` (shift left)
    GtGt,         // `>>` (shift right)
    StarStar,     // `**` | `pow` (exponentiation)
    NullCoalesce, // `??`

    // ── Delimiters ───────────────────────────────────────────────────────────
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma, // `,` | `also`
    Dot,
    DotDot,    // `..`  exclusive range
    DotDotDot, // `...` inclusive range
    Colon,
    DoubleColon, // `::`
    Arrow,       // `->`
    FatArrow,    // `=>`
    /// `;` | `sep` — inline statement separator.
    Semicolon,
    /// Emitted at the end of a logical line (Go-style automatic insertion).
    Newline,
    Hash, // `#` decorator prefix (the `@` is decorator, `#` is comment start — already consumed)
    At,   // `@` decorator prefix

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

    /// Returns the canonical keyword string for this token, if it is a keyword.
    /// Used to allow keywords as field / method names after `.` (e.g. `obj.set()`).
    pub fn as_keyword_str(&self) -> Option<&'static str> {
        Some(match self {
            TokenKind::Var => "var",
            TokenKind::Const => "const",
            TokenKind::Tuple => "tuple",
            TokenKind::Set => "set",
            TokenKind::Action => "action",
            TokenKind::Object => "object",
            TokenKind::Extends => "extends",
            TokenKind::Return => "return",
            TokenKind::If => "if",
            TokenKind::Otherwise => "otherwise",
            TokenKind::When => "when",
            TokenKind::For => "for",
            TokenKind::In => "in",
            TokenKind::While => "while",
            TokenKind::Break => "break",
            TokenKind::Continue => "continue",
            TokenKind::Attempt => "attempt",
            TokenKind::Catch => "catch",
            TokenKind::Finally => "finally",
            TokenKind::Panic => "panic",
            TokenKind::Check => "check",
            TokenKind::Use => "use",
            TokenKind::Export => "export",
            TokenKind::As => "as",
            TokenKind::Oftype => "oftype",
            TokenKind::Certain => "certain",
            TokenKind::Optional => "optional",
            TokenKind::Dynamic => "dynamic",
            TokenKind::Parallel => "parallel",
            TokenKind::Concurrent => "concurrent",
            TokenKind::Task => "task",
            TokenKind::Spawn => "spawn",
            TokenKind::Await => "await",
            TokenKind::Shared => "shared",
            TokenKind::Pending => "pending",
            TokenKind::Weak => "weak",
            TokenKind::And => "and",
            TokenKind::Or => "or",
            TokenKind::Not => "not",
            TokenKind::Is => "is",
            TokenKind::This => "this",
            TokenKind::Parent => "parent",
            TokenKind::New => "new",
            TokenKind::True => "true",
            TokenKind::False => "false",
            TokenKind::Nothing => "nothing",
            _ => return None,
        })
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
