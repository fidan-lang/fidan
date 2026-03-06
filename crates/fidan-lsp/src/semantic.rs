//! Semantic token computation — walks the flat lexer token stream and assigns
//! an LSP semantic token type to every identifier based on syntactic context.
//!
//! Token types:
//!   0 = function   (action declarations and call sites)
//!   1 = method     (method calls after `.`)
//!   2 = class      (object declarations, type references, constructors)
//!   3 = variable   (var/const declarations and general identifier usages)
//!   4 = parameter  (action parameter declarations)
//!   5 = property   (field accesses after `.`)
//!   6 = type       (built-in type names after `oftype` / `->`)
//!   7 = keyword    (word-alias synonym tokens: `also`, `sep`)
//!
//! Token modifiers (bitmask):
//!   bit 0 = declaration
//!   bit 1 = readonly

use fidan_lexer::{Symbol, SymbolInterner, Token, TokenKind};
use fidan_source::SourceFile;
use tower_lsp::lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend,
};

// ── Legend indices ────────────────────────────────────────────────────────────

pub const TT_FUNCTION: u32 = 0;
pub const TT_METHOD: u32 = 1;
pub const TT_CLASS: u32 = 2;
pub const TT_VARIABLE: u32 = 3;
pub const TT_PARAMETER: u32 = 4;
pub const TT_PROPERTY: u32 = 5;
pub const TT_TYPE: u32 = 6;
pub const TT_KEYWORD: u32 = 7;

pub const TM_DECLARATION: u32 = 1 << 0;
pub const TM_READONLY: u32 = 1 << 1;

/// Build the `SemanticTokensLegend` that must be advertised in the server
/// capabilities (`initialize` response). The indices here MUST stay in sync
/// with the `TT_*` / `TM_*` constants above.
pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::FUNCTION,  // 0
            SemanticTokenType::METHOD,    // 1
            SemanticTokenType::CLASS,     // 2
            SemanticTokenType::VARIABLE,  // 3
            SemanticTokenType::PARAMETER, // 4
            SemanticTokenType::PROPERTY,  // 5
            SemanticTokenType::TYPE,      // 6
            SemanticTokenType::KEYWORD,   // 7
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION, // bit 0
            SemanticTokenModifier::READONLY,    // bit 1
        ],
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Walk `tokens` and produce a delta-encoded `Vec<SemanticToken>` ready to be
/// sent in a `textDocument/semanticTokens/full` response.
pub fn compute(
    tokens: &[Token],
    file: &SourceFile,
    interner: &SymbolInterner,
) -> Vec<SemanticToken> {
    // Only consider tokens that carry semantic meaning (skip whitespace noise).
    let meaningful: Vec<&Token> = tokens
        .iter()
        .filter(|t| {
            !matches!(
                t.kind,
                TokenKind::Newline | TokenKind::Eof | TokenKind::Unknown(_)
            )
        })
        .collect();

    let n = meaningful.len();

    // Pre-pass: collect the set of symbols that appear in parameter-declaration
    // position so that `ident(` can be coloured as TT_PARAMETER (red) rather
    // than TT_FUNCTION (blue) when the callee is an action-typed parameter.
    let mut param_syms = std::collections::HashSet::<Symbol>::new();
    for i in 0..n {
        let sym = match &meaningful[i].kind {
            TokenKind::Ident(s) => *s,
            _ => continue,
        };
        let prev = if i > 0 {
            Some(&meaningful[i - 1].kind)
        } else {
            None
        };
        let next = if i + 1 < n {
            Some(&meaningful[i + 1].kind)
        } else {
            None
        };
        let after_param_prefix = matches!(
            prev,
            Some(TokenKind::Certain)
                | Some(TokenKind::Optional)
                | Some(TokenKind::LParen)
                | Some(TokenKind::Comma)
        );
        let before_type_ann = matches!(next, Some(TokenKind::Oftype) | Some(TokenKind::Arrow));
        if after_param_prefix && before_type_ann {
            param_syms.insert(sym);
        }
    }

    // Raw tokens before delta-encoding: (line, start_char, length, type, mods)
    // Both line and start_char are 0-based.
    let mut raw: Vec<(u32, u32, u32, u32, u32)> = Vec::new();
    // Track the semantic token type last emitted for an identifier so that
    // dotted type-qualifier chains (e.g. `extends module.Foo`) can inherit
    // the class classification from the preceding segment.
    let mut prev_emitted_tt: Option<u32> = None;

    for idx in 0..n {
        let tok = meaningful[idx];

        // Word-alias synonyms that lex as punctuation tokens:
        // `also` → TokenKind::Comma  (span length 4; `,` span length 1)
        // `sep`  → TokenKind::Semicolon (span length 3; `;` span length 1)
        // Emit an explicit TT_KEYWORD so these are always coloured as keywords
        // regardless of which TextMate scope happens to apply nearby.
        let span_len = tok.span.end - tok.span.start;
        if matches!(tok.kind, TokenKind::Comma | TokenKind::Semicolon) && span_len > 1 {
            let (line1, col1) = file.line_col(tok.span.start);
            let line = line1.saturating_sub(1);
            let start = col1.saturating_sub(1);
            raw.push((line, start, span_len, TT_KEYWORD, 0));
            prev_emitted_tt = None;
            continue;
        }

        // We only emit semantic tokens for bare identifiers.
        let sym = match &tok.kind {
            TokenKind::Ident(s) => *s,
            _ => continue,
        };

        let sym_str = interner.resolve(sym);

        // Contextual keywords (`with`, `returns`, `then`) are kept as `Ident`
        // by the lexer but the TextMate grammar already colours them as
        // `keyword.other.modifier.fidan`.  Don't override that — skip them.
        if matches!(&*sym_str, "with" | "returns" | "then") {
            continue;
        }

        let prev = if idx > 0 {
            Some(&meaningful[idx - 1].kind)
        } else {
            None
        };
        let next = if idx + 1 < n {
            Some(&meaningful[idx + 1].kind)
        } else {
            None
        };

        // `returns TYPE` — `returns` is kept as Ident by the lexer (contextual
        // keyword), so we check the previous token's string to detect the type
        // position that classify() cannot see through a bare TokenKind.
        let prev_resolved: Option<std::sync::Arc<str>> = prev.and_then(|k| {
            if let TokenKind::Ident(s) = k {
                Some(interner.resolve(*s))
            } else {
                None
            }
        });
        let prev_ident_str: Option<&str> = prev_resolved.as_deref();
        let (tt, mods) = if prev_ident_str == Some("returns") {
            if sym_str.starts_with(|c: char| c.is_uppercase()) {
                (TT_CLASS, 0)
            } else {
                (TT_TYPE, 0)
            }
        } else if param_syms.contains(&sym) && matches!(next, Some(TokenKind::LParen)) {
            // An action-typed parameter being invoked — keep the parameter
            // colour (red) instead of falling through to the blue function-call
            // classification.  E.g. `fn()` inside `action register with (fn oftype action)`.
            (TT_PARAMETER, 0)
        } else if matches!(prev, Some(TokenKind::Dot)) && prev_emitted_tt == Some(TT_CLASS) {
            // Qualified type path after `extends`: `extends module.Foo` or `extends a.b.Foo`.
            // The preceding identifier was classified as TT_CLASS (e.g. the namespace
            // `module` after `extends`), so keep the class color for the next segment too.
            (TT_CLASS, 0)
        } else {
            classify(&sym_str, prev, next)
        };
        prev_emitted_tt = Some(tt);

        let (line1, col1) = file.line_col(tok.span.start);
        let line = line1.saturating_sub(1);
        let start = col1.saturating_sub(1);
        let len = (tok.span.end - tok.span.start) as u32;

        raw.push((line, start, len, tt, mods));
    }

    // Tokens must be sorted by position for delta-encoding to be valid.
    // They should already be in document order from the lexer, but be safe.
    raw.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    // Delta-encode: each token's line/start is relative to the previous one.
    let mut result = Vec::with_capacity(raw.len());
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;

    for (line, start, len, tt, mods) in raw {
        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 {
            start - prev_start
        } else {
            start
        };
        prev_line = line;
        prev_start = start;
        result.push(SemanticToken {
            delta_line,
            delta_start,
            length: len,
            token_type: tt,
            token_modifiers_bitset: mods,
        });
    }

    result
}

// ── Classification ────────────────────────────────────────────────────────────

fn classify(sym: &str, prev: Option<&TokenKind>, next: Option<&TokenKind>) -> (u32, u32) {
    // ── Declaration sites ─────────────────────────────────────────────────────

    // `var NAME` / `const NAME`
    if matches!(prev, Some(TokenKind::Var)) {
        return (TT_VARIABLE, TM_DECLARATION);
    }
    if matches!(prev, Some(TokenKind::Const)) {
        return (TT_VARIABLE, TM_DECLARATION | TM_READONLY);
    }

    // `action NAME` (covers plain, extension, and parallel-action — the
    // `parallel` keyword precedes `action`, not directly the name token)
    if matches!(prev, Some(TokenKind::Action)) {
        return (TT_FUNCTION, TM_DECLARATION);
    }

    // `object NAME`
    if matches!(prev, Some(TokenKind::Object)) {
        return (TT_CLASS, TM_DECLARATION);
    }

    // `task NAME`  (inside concurrent/parallel blocks)
    if matches!(prev, Some(TokenKind::Task)) {
        return (TT_FUNCTION, TM_DECLARATION);
    }

    // `test "name" { ... }` — the name is a string literal, not an ident,
    // so nothing to classify here.

    // ── Catch binding `catch NAME` / `catch NAME oftype T` ───────────────────
    if matches!(prev, Some(TokenKind::Catch)) {
        return (TT_VARIABLE, TM_DECLARATION);
    }

    // ── Loop variable `for NAME in ...` ──────────────────────────────────────
    if matches!(prev, Some(TokenKind::For)) {
        return (TT_VARIABLE, TM_DECLARATION);
    }

    // ── `use MODULE.PATH` — colour the first segment after `use` as a class
    // (yellow). The outer-loop `prev_emitted_tt` chain propagates TT_CLASS
    // through subsequent dotted segments automatically.
    if matches!(prev, Some(TokenKind::Use)) {
        return (TT_CLASS, 0);
    }

    // ── Import alias `use ... as NAME` ───────────────────────────────────────
    // Always a namespace/module alias — colour it like a class (yellow).
    if matches!(prev, Some(TokenKind::As)) {
        return (TT_CLASS, TM_DECLARATION);
    }

    // ── `extends TYPE` ────────────────────────────────────────────────────────
    if matches!(prev, Some(TokenKind::Extends)) {
        return (TT_CLASS, 0);
    }

    // ── `@DECORATOR_NAME` ────────────────────────────────────────────────────
    if matches!(prev, Some(TokenKind::At)) {
        return (TT_FUNCTION, 0);
    }

    // ── Type position: `oftype NAME` or `-> NAME` ────────────────────────────
    if matches!(prev, Some(TokenKind::Oftype) | Some(TokenKind::Arrow)) {
        // PascalCase → user-defined object type; lowercase → built-in type name
        if sym.starts_with(|c: char| c.is_uppercase()) {
            return (TT_CLASS, 0);
        }
        return (TT_TYPE, 0);
    }

    // ── Parameter declarations ────────────────────────────────────────────────
    // Pattern: `(certain|optional) PARAM oftype/→` or `'(' PARAM oftype/→`
    //          or `, PARAM oftype/→`  (comma = also-alias too, both → TokenKind::Comma)
    let after_param_prefix = matches!(
        prev,
        Some(TokenKind::Certain)
            | Some(TokenKind::Optional)
            | Some(TokenKind::LParen)
            | Some(TokenKind::Comma)
    );
    let before_type_ann = matches!(next, Some(TokenKind::Oftype) | Some(TokenKind::Arrow));
    if after_param_prefix && before_type_ann {
        return (TT_PARAMETER, TM_DECLARATION);
    }

    // ── Member access: `expr.IDENT` ──────────────────────────────────────────
    if matches!(prev, Some(TokenKind::Dot)) {
        if matches!(next, Some(TokenKind::LParen)) {
            return (TT_METHOD, 0);
        }
        return (TT_PROPERTY, 0);
    }

    // ── Call sites: `IDENT(` ─────────────────────────────────────────────────
    if matches!(next, Some(TokenKind::LParen)) {
        // PascalCase → constructor / type call (class color)
        if sym.starts_with(|c: char| c.is_uppercase()) {
            return (TT_CLASS, 0);
        }
        return (TT_FUNCTION, 0);
    }

    // ── Everything else: treated as a variable usage ──────────────────────────
    (TT_VARIABLE, 0)
}
