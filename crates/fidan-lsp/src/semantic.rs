//! Semantic token computation — walks the flat lexer token stream and assigns
//! an LSP semantic token type to every identifier based on syntactic context.
//!
//! Token types:
//!   0 = function   (action declarations and call sites)
//!   1 = method     (method calls after `.`)
//!   2 = class      (object declarations, type references, constructors)
//!   3 = enumMember (enum variants like `Direction.Up`)
//!   4 = variable   (var/const declarations and general identifier usages)
//!   5 = parameter  (action parameter declarations)
//!   6 = property   (field accesses after `.`)
//!   7 = type       (built-in type names after `oftype` / `->`)
//!   8 = keyword    (word-alias synonym tokens: `also`, `sep`)
//!
//! Token modifiers (bitmask):
//!   bit 0 = declaration
//!   bit 1 = readonly

use crate::symbols::{SymKind, SymbolTable};
use fidan_ast::{Item, Module};
use fidan_config::is_type_like_name;
use fidan_lexer::{Symbol, SymbolInterner, Token, TokenKind};
use fidan_source::SourceFile;
use tower_lsp::lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend,
};

// ── Legend indices ────────────────────────────────────────────────────────────

pub const TT_FUNCTION: u32 = 0;
pub const TT_METHOD: u32 = 1;
pub const TT_CLASS: u32 = 2;
pub const TT_ENUM_MEMBER: u32 = 3;
pub const TT_VARIABLE: u32 = 4;
pub const TT_PARAMETER: u32 = 5;
pub const TT_PROPERTY: u32 = 6;
pub const TT_TYPE: u32 = 7;
pub const TT_KEYWORD: u32 = 8;

pub const TM_DECLARATION: u32 = 1 << 0;
pub const TM_READONLY: u32 = 1 << 1;

/// Build the `SemanticTokensLegend` that must be advertised in the server
/// capabilities (`initialize` response). The indices here MUST stay in sync
/// with the `TT_*` / `TM_*` constants above.
pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::FUNCTION,    // 0
            SemanticTokenType::METHOD,      // 1
            SemanticTokenType::CLASS,       // 2
            SemanticTokenType::ENUM_MEMBER, // 3
            SemanticTokenType::VARIABLE,    // 4
            SemanticTokenType::PARAMETER,   // 5
            SemanticTokenType::PROPERTY,    // 6
            SemanticTokenType::TYPE,        // 7
            SemanticTokenType::KEYWORD,     // 8
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
    module: &Module,
    symbol_table: &SymbolTable,
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
    let enum_variant_decl_starts = enum_variant_declaration_starts(module);

    // Pre-pass: collect the set of symbols that appear in parameter-declaration
    // position so that `ident(` can be coloured as TT_PARAMETER (red) rather
    // than TT_FUNCTION (blue) when the callee is an action-typed parameter.
    let mut param_syms = std::collections::HashSet::<Symbol>::new();
    let mut import_namespace_syms = std::collections::HashSet::<Symbol>::new();
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

    let mut idx = 0usize;
    while idx < n {
        if !matches!(meaningful[idx].kind, TokenKind::Use) {
            idx += 1;
            continue;
        }

        idx += 1;
        let mut grouped = false;
        let mut last_segment: Option<Symbol> = None;
        let mut alias: Option<Symbol> = None;

        while idx < n {
            match &meaningful[idx].kind {
                TokenKind::Ident(sym) => {
                    last_segment = Some(*sym);
                    idx += 1;
                }
                TokenKind::Dot | TokenKind::Comma => {
                    idx += 1;
                }
                TokenKind::As => {
                    if idx + 1 < n
                        && let TokenKind::Ident(sym) = meaningful[idx + 1].kind
                    {
                        alias = Some(sym);
                    }
                    idx += 1;
                }
                TokenKind::LBrace => {
                    grouped = true;
                    break;
                }
                TokenKind::Newline | TokenKind::Semicolon | TokenKind::Eof => {
                    break;
                }
                _ => {
                    idx += 1;
                }
            }
        }

        if !grouped && let Some(bound) = alias.or(last_segment) {
            import_namespace_syms.insert(bound);
        }

        while idx < n
            && !matches!(
                meaningful[idx].kind,
                TokenKind::Newline | TokenKind::Semicolon | TokenKind::Eof
            )
        {
            idx += 1;
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

        if enum_variant_decl_starts.contains(&tok.span.start) {
            let (line1, col1) = file.line_col(tok.span.start);
            let line = line1.saturating_sub(1);
            let start = col1.saturating_sub(1);
            let len = tok.span.end - tok.span.start;
            raw.push((line, start, len, TT_ENUM_MEMBER, TM_DECLARATION));
            prev_emitted_tt = Some(TT_ENUM_MEMBER);
            continue;
        }

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
        let visible_kind = symbol_table
            .lookup_visible(tok.span.start, sym_str.as_ref())
            .map(|entry| &entry.kind);
        let qualified_kind = if matches!(prev, Some(TokenKind::Dot)) && idx >= 2 {
            match (&meaningful[idx - 2].kind, &meaningful[idx - 1].kind) {
                (TokenKind::Ident(parent), TokenKind::Dot) => {
                    let parent_name = interner.resolve(*parent);
                    symbol_table
                        .get(&format!("{}.{}", parent_name, sym_str))
                        .map(|entry| &entry.kind)
                }
                _ => None,
            }
        } else {
            None
        };
        let (tt, mods) = if prev_ident_str == Some("returns") {
            if sym_str.starts_with(|c: char| c.is_uppercase()) {
                (TT_CLASS, 0)
            } else {
                (TT_TYPE, 0)
            }
        } else if import_namespace_syms.contains(&sym) && matches!(next, Some(TokenKind::Dot)) {
            (TT_CLASS, 0)
        } else if param_syms.contains(&sym) && matches!(next, Some(TokenKind::LParen)) {
            // An action-typed parameter being invoked — keep the parameter
            // colour (red) instead of falling through to the blue function-call
            // classification.  E.g. `fn()` inside `action register with (fn oftype action)`.
            (TT_PARAMETER, 0)
        } else if matches!(prev, Some(TokenKind::Dot))
            && prev_emitted_tt == Some(TT_CLASS)
            && qualified_kind.is_none()
        {
            // Qualified type path after `extends`: `extends module.Foo` or `extends a.b.Foo`.
            // The preceding identifier was classified as TT_CLASS (e.g. the namespace
            // `module` after `extends`), so keep the class color for the next segment too.
            (TT_CLASS, 0)
        } else {
            classify(&sym_str, prev, next, visible_kind, qualified_kind)
        };
        prev_emitted_tt = Some(tt);

        let (line1, col1) = file.line_col(tok.span.start);
        let line = line1.saturating_sub(1);
        let start = col1.saturating_sub(1);
        let len = tok.span.end - tok.span.start;

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

fn classify(
    sym: &str,
    prev: Option<&TokenKind>,
    next: Option<&TokenKind>,
    visible_kind: Option<&SymKind>,
    qualified_kind: Option<&SymKind>,
) -> (u32, u32) {
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

    // `enum NAME`
    if matches!(prev, Some(TokenKind::Enum)) {
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
        if is_type_like_name(sym) {
            return (TT_TYPE, 0);
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

    if matches!(visible_kind, Some(SymKind::Object | SymKind::Enum)) {
        return (TT_CLASS, 0);
    }

    // ── Member access: `expr.IDENT` ──────────────────────────────────────────
    if matches!(prev, Some(TokenKind::Dot)) {
        if matches!(qualified_kind, Some(SymKind::EnumVariant)) {
            return (TT_ENUM_MEMBER, 0);
        }
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

fn enum_variant_declaration_starts(module: &Module) -> std::collections::HashSet<u32> {
    module
        .items
        .iter()
        .filter_map(|item_id| match module.arena.get_item(*item_id) {
            Item::EnumDecl { variants, .. } => {
                Some(variants.iter().map(|variant| variant.span.start))
            }
            _ => None,
        })
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_lexer::Lexer;
    use fidan_source::{FileId, SourceFile};
    use std::sync::Arc;

    fn semantic_token_types_for(src: &str) -> Vec<u32> {
        let interner = Arc::new(SymbolInterner::new());
        let file = SourceFile::new(FileId(0), "<semantic>", src);
        let (tokens, _) = Lexer::new(&file, Arc::clone(&interner)).tokenise();
        let (module, diags) = fidan_parser::parse(&tokens, FileId(0), Arc::clone(&interner));
        assert!(diags.is_empty(), "parser diagnostics: {diags:#?}");
        let typed = fidan_typeck::typecheck_full(&module, Arc::clone(&interner));
        let symbol_table = crate::symbols::build(&module, &typed, &interner);
        compute(&tokens, &file, &interner, &module, &symbol_table)
            .into_iter()
            .map(|token| token.token_type)
            .collect()
    }

    #[test]
    fn namespace_import_usage_keeps_class_coloring() {
        let token_types = semantic_token_types_for("use std.math\nvar x = math.round(3.5)\n");
        assert!(
            token_types.iter().filter(|&&tt| tt == TT_CLASS).count() >= 2,
            "expected namespace import declaration and usage to be class-colored: {token_types:?}"
        );
    }

    #[test]
    fn namespace_import_alias_usage_keeps_class_coloring() {
        let token_types = semantic_token_types_for("use std.math as m\nvar x = m.round(3.5)\n");
        assert!(
            token_types.iter().filter(|&&tt| tt == TT_CLASS).count() >= 3,
            "expected namespace alias declaration and usage to be class-colored: {token_types:?}"
        );
    }

    #[test]
    fn enum_declarations_and_variant_uses_are_class_colored() {
        let token_types =
            semantic_token_types_for("enum Direction { North }\nvar d = Direction.North\n");
        assert!(
            token_types.iter().filter(|&&tt| tt == TT_CLASS).count() >= 2,
            "expected enum declaration and enum type path to be class-colored: {token_types:?}"
        );
        assert!(
            token_types
                .iter()
                .filter(|&&tt| tt == TT_ENUM_MEMBER)
                .count()
                >= 2,
            "expected enum declaration variant and usage to use enum-member coloring: {token_types:?}"
        );
    }

    #[test]
    fn object_type_stays_class_colored_while_field_stays_property_colored() {
        let token_types = semantic_token_types_for(
            "object MyObject { var richtung oftype Direction }\nMyObject.richtung\n",
        );
        assert!(
            token_types.contains(&TT_CLASS),
            "expected object type name to stay class-colored: {token_types:?}"
        );
        assert!(
            token_types.contains(&TT_PROPERTY),
            "expected object field member to stay property-colored: {token_types:?}"
        );
    }

    #[test]
    fn handle_type_references_are_type_colored() {
        let token_types = semantic_token_types_for(
            "action native returns handle {\n    var raw oftype handle\n    return raw\n}\n",
        );
        assert!(
            token_types.iter().filter(|&&tt| tt == TT_TYPE).count() >= 2,
            "expected handle return/type references to use type coloring: {token_types:?}"
        );
    }

    #[test]
    fn handle_param_types_are_type_colored() {
        let token_types = semantic_token_types_for(
            "action cppFreeHandle with (certain h oftype handle) returns nothing {\n    return nothing\n}\n",
        );
        assert!(
            token_types.iter().filter(|&&tt| tt == TT_TYPE).count() >= 1,
            "expected handle parameter type references to use type coloring: {token_types:?}"
        );
    }
}
