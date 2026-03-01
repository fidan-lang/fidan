use crate::TokenKind;

/// Maps keyword strings and operator aliases to their canonical `TokenKind`.
///
/// Resolution happens at lex time so the parser never sees synonyms.
/// The original source span is always preserved for error messages.
pub fn lookup_keyword(s: &str) -> Option<TokenKind> {
    Some(match s {
        // ── Statement / declaration keywords ─────────────────────────────
        "var" => TokenKind::Var,
        "const" => TokenKind::Const,
        "tuple" => TokenKind::Tuple,
        "action" => TokenKind::Action,
        "object" => TokenKind::Object,
        "extends" => TokenKind::Extends,
        "return" => TokenKind::Return,
        "if" => TokenKind::If,
        "otherwise" | "else" => TokenKind::Otherwise,
        "when" => TokenKind::When,
        "then" => return None, // context keyword — kept as Ident, parser handles
        "for" => TokenKind::For,
        "in" => TokenKind::In,
        "while" => TokenKind::While,
        "break" | "stop" => TokenKind::Break,
        "continue" => TokenKind::Continue,
        "attempt" | "try" => TokenKind::Attempt,
        "catch" => TokenKind::Catch,
        "finally" => TokenKind::Finally,
        "panic" | "throw" => TokenKind::Panic,
        "use" => TokenKind::Use,
        "export" => TokenKind::Export,
        "check" => TokenKind::Check,
        "as" => TokenKind::As,
        "oftype" => TokenKind::Oftype,
        "required" => TokenKind::Required,
        "optional" => TokenKind::Optional,
        "dynamic" | "flexible" => TokenKind::Dynamic,
        "parallel" => TokenKind::Parallel,
        "concurrent" => TokenKind::Concurrent,
        "task" => TokenKind::Task,
        "spawn" => TokenKind::Spawn,
        "await" => TokenKind::Await,
        "Shared" => TokenKind::Shared,
        "Pending" => TokenKind::Pending,
        "WeakShared" => TokenKind::Weak,
        "with" => return None, // parameter-list context, kept as Ident

        // ── Literals ─────────────────────────────────────────────────────
        "nothing" => TokenKind::Nothing,
        "true" => TokenKind::LitBool(true),
        "false" => TokenKind::LitBool(false),
        "True" => TokenKind::LitBool(true),
        "False" => TokenKind::LitBool(false),

        // ── Operators / synonyms ──────────────────────────────────────────
        "and" => TokenKind::And, // `&&` handled at punct level
        "or" => TokenKind::Or,   // `||` handled at punct level
        "not" => TokenKind::Not, // `!`  handled at punct level
        "is" | "equals" => TokenKind::Is,
        "notequals" => TokenKind::NotEq,
        "greaterthan" => TokenKind::Gt,
        "lessthan" => TokenKind::Lt,
        "greaterthanorequals" => TokenKind::GtEq,
        "lessthanorequals" => TokenKind::LtEq,
        "also" => TokenKind::Comma,
        "set" => TokenKind::Set,
        "mod" => TokenKind::Percent,
        "pow" => TokenKind::StarStar,

        // ── Statement separator synonyms ─────────────────────────────────
        "sep" => TokenKind::Semicolon,

        // ── Object / method keywords ──────────────────────────────────────
        "this" => TokenKind::This,
        "parent" => TokenKind::Parent,
        "new" => TokenKind::New,

        _ => return None,
    })
}
