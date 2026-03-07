// fidan-parser — recursive-descent parser
use std::sync::Arc;

use fidan_ast::{
    BinOp, CatchClause, CheckArm, Decorator, ElseIf, EnumVariantDef, Expr, FieldDecl, Item,
    ItemId, Module, Param, Stmt, StmtId, Task, TypeExpr,
};
use fidan_diagnostics::Diagnostic;
use fidan_lexer::{Symbol, SymbolInterner, Token, TokenKind};
use fidan_source::{FileId, Span};

// ── Parser struct ─────────────────────────────────────────────────────────────

/// Maximum number of errors before the parser gives up entirely.
/// Prevents infinite loops caused by error-recovery that makes no token progress.
pub(crate) const MAX_ERRORS: usize = 50;

pub struct Parser<'t> {
    pub(crate) tokens: &'t [Token],
    pub(crate) pos: usize,
    pub(crate) module: Module,
    pub(crate) diags: Vec<Diagnostic>,
    pub(crate) interner: Arc<SymbolInterner>,
    /// When `true`, the parser is in post-error recovery mode.
    /// Further errors are suppressed until `synchronize()` resets this flag,
    /// preventing one bad token from producing hundreds of cascade diagnostics.
    pub(crate) recovering: bool,
    /// When `true`, the parser has hit the error cap and should unwind immediately.
    /// Checked by all major parsing loops to avoid infinite recovery cycles.
    pub(crate) bail: bool,
    // Contextual keyword symbols (interned once at construction)
    pub(crate) sym_with: Symbol,
    pub(crate) sym_returns: Symbol,
    pub(crate) sym_default: Symbol,
    pub(crate) sym_else: Symbol,
    /// `step` contextual keyword for slice expressions: `list[0..10 step 2]`.
    pub(crate) sym_step: Symbol,
    /// When `true`, `infix_bp` suppresses `..`/`...` so that `parse_expr_bp(0)` stops
    /// before the range operator during slice-start parsing.
    pub(crate) in_slice_start: bool,
    /// Sub-token stream for string-interpolation fragment re-lexing.
    /// When `Some`, `peek` / `advance` / `current_span` read from the fragment
    /// buffer instead of the main `tokens` slice.
    pub(crate) fragment: Option<(Vec<Token>, usize)>,
}

impl<'t> Parser<'t> {
    pub fn new(tokens: &'t [Token], file_id: FileId, interner: Arc<SymbolInterner>) -> Self {
        let sym_with = interner.intern("with");
        let sym_returns = interner.intern("returns");
        let sym_default = interner.intern("default");
        let sym_else = interner.intern("else");
        let sym_step = interner.intern("step");
        Self {
            tokens,
            pos: 0,
            module: Module::new(file_id),
            diags: Vec::new(),
            interner,
            recovering: false,
            bail: false,
            sym_with,
            sym_returns,
            sym_default,
            sym_else,
            sym_step,
            in_slice_start: false,
            fragment: None,
        }
    }

    pub fn finish(self) -> (Module, Vec<Diagnostic>) {
        (self.module, self.diags)
    }

    // ── Token navigation ──────────────────────────────────────────────────────

    pub(crate) fn peek(&self) -> &TokenKind {
        if let Some((ref toks, pos)) = self.fragment {
            &toks[pos.min(toks.len().saturating_sub(1))].kind
        } else {
            &self.tokens[self.pos].kind
        }
    }

    /// Look `n` tokens ahead without consuming.
    pub(crate) fn peek_nth(&self, n: usize) -> &TokenKind {
        &self.tokens[self.pos.saturating_add(n).min(self.tokens.len() - 1)].kind
    }

    pub(crate) fn current_span(&self) -> Span {
        if let Some((ref toks, pos)) = self.fragment {
            toks[pos.min(toks.len().saturating_sub(1))].span
        } else {
            self.tokens[self.pos].span
        }
    }

    pub(crate) fn advance(&mut self) -> Token {
        if let Some((ref toks, ref mut pos)) = self.fragment {
            let tok = toks[*pos].clone();
            if *pos + 1 < toks.len() {
                *pos += 1;
            }
            tok
        } else {
            let tok = self.tokens[self.pos].clone();
            if self.pos + 1 < self.tokens.len() {
                self.pos += 1;
            }
            tok
        }
    }

    /// Consume the current token if its discriminant matches `kind`.
    pub(crate) fn eat(&mut self, kind: &TokenKind) -> bool {
        if std::mem::discriminant(self.peek()) == std::mem::discriminant(kind) {
            // For data-carrying variants also match the payload where it matters
            match (self.peek(), kind) {
                (TokenKind::LitBool(a), TokenKind::LitBool(b)) if a != b => return false,
                _ => {}
            }
            self.advance();
            true
        } else {
            false
        }
    }

    /// Consume the current token exactly if `self.peek() == kind`, else emit an error.
    pub(crate) fn expect_tok(&mut self, kind: &TokenKind) -> Span {
        let span = self.current_span();
        if std::mem::discriminant(self.peek()) == std::mem::discriminant(kind) {
            self.advance();
        } else {
            self.error(
                &format!("expected `{:?}`, found `{:?}`", kind, self.peek()),
                span,
            );
        }
        span
    }

    pub(crate) fn skip_terminators(&mut self) {
        while matches!(self.peek(), TokenKind::Newline | TokenKind::Semicolon) {
            self.advance();
        }
    }

    pub(crate) fn skip_one_terminator(&mut self) {
        if matches!(self.peek(), TokenKind::Newline | TokenKind::Semicolon) {
            self.advance();
        }
    }

    /// Returns `true` if the current token is `Ident(sym)`.
    pub(crate) fn at_ident(&self, sym: Symbol) -> bool {
        matches!(self.peek(), TokenKind::Ident(s) if *s == sym)
    }

    /// Consume if the current token is `Ident(sym)`.
    pub(crate) fn eat_ident(&mut self, sym: Symbol) -> bool {
        if self.at_ident(sym) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Consume a type annotation introducer: `oftype` or `->` (Arrow).
    /// Returns `true` if a token was consumed.
    #[inline]
    pub(crate) fn eat_type_ann(&mut self) -> bool {
        if matches!(self.peek(), TokenKind::Oftype | TokenKind::Arrow) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Consume an identifier and return its symbol, or emit an error.
    pub(crate) fn expect_ident_sym(&mut self, msg: &str) -> Symbol {
        let span = self.current_span();
        if let TokenKind::Ident(sym) = self.peek().clone() {
            self.advance();
            sym
        } else {
            self.error(msg, span);
            self.interner.intern("<error>")
        }
    }

    /// Like `expect_ident_sym` but also accepts keyword tokens as field names.
    /// Keywords are legal identifiers after `.` in Fidan (e.g. `obj.set()`,
    /// `obj.new`) — same rule as Python, Swift, Kotlin, etc.
    pub(crate) fn expect_field_name(&mut self) -> Symbol {
        let span = self.current_span();
        let tok = self.peek().clone();
        if let TokenKind::Ident(sym) = tok {
            self.advance();
            return sym;
        }
        if let Some(kw) = tok.as_keyword_str() {
            self.advance();
            return self.interner.intern(kw);
        }
        self.error("expected field name after `.`", span);
        self.interner.intern("<error>")
    }

    // ── Module parsing ────────────────────────────────────────────────────────

    pub fn parse_module(&mut self) {
        loop {
            if self.bail {
                break;
            }
            self.skip_terminators();
            if matches!(self.peek(), TokenKind::Eof) {
                break;
            }
            let pos_before = self.pos;
            if let Some(id) = self.parse_top_level() {
                self.module.items.push(id);
            } else {
                self.synchronize();
                // If synchronize made no progress (e.g. stuck on a stray `}`),
                // force-advance to prevent an infinite loop.
                if self.pos == pos_before {
                    self.advance();
                }
            }
        }
    }

    fn parse_top_level(&mut self) -> Option<ItemId> {
        let decs = self.parse_decorators();
        self.skip_terminators(); // skip any newlines between decorator and declaration
        match self.peek().clone() {
            TokenKind::Object => Some(self.parse_object_decl()),
            TokenKind::Enum => Some(self.parse_enum_decl()),
            TokenKind::Action => Some(self.parse_action_decl(false, decs)),
            TokenKind::Use => Some(self.parse_use_decl(false)),
            TokenKind::Test => Some(self.parse_test_decl()),
            TokenKind::Export => {
                self.advance(); // eat `export`
                if matches!(self.peek(), TokenKind::Use) {
                    Some(self.parse_use_decl(true))
                } else {
                    let span = self.current_span();
                    self.error("expected `use` after `export`", span);
                    None
                }
            }
            TokenKind::Const => {
                // `const var name ...` — top-level immutable declaration
                let start = self.current_span().start;
                self.advance(); // eat `const`
                if !matches!(self.peek(), TokenKind::Var) {
                    let span = self.current_span();
                    self.error("expected `var` after `const`", span);
                    return None;
                }
                self.advance(); // eat `var`
                let name = self.expect_ident_sym("expected variable name");
                let ty = if matches!(self.peek(), TokenKind::Minus)
                    && matches!(self.peek_nth(1), TokenKind::Ident(_))
                {
                    let span = self.current_span();
                    self.error("invalid type annotation syntax: did you mean `->`?", span);
                    self.advance(); // eat the erroneous `-`
                    Some(self.parse_type_expr())
                } else if self.eat_type_ann() {
                    Some(self.parse_type_expr())
                } else {
                    None
                };
                let init = if matches!(self.peek(), TokenKind::Assign | TokenKind::Set) {
                    self.advance();
                    Some(self.parse_expr())
                } else {
                    None
                };
                let end = self.current_span().end;
                self.skip_one_terminator();
                Some(self.module.arena.alloc_item(Item::VarDecl {
                    name,
                    ty,
                    init,
                    is_const: true,
                    span: Span::new(self.module.file, start, end),
                }))
            }
            // Control-flow statements at module/top-level scope — delegate to parse_stmt.
            TokenKind::For
            | TokenKind::While
            | TokenKind::If
            | TokenKind::Check
            | TokenKind::Attempt
            | TokenKind::Panic
            | TokenKind::Break
            | TokenKind::Continue
            | TokenKind::Return
            | TokenKind::Concurrent => {
                if let Some(sid) = self.parse_stmt() {
                    return Some(self.module.arena.alloc_item(Item::Stmt(sid)));
                }
                return None;
            }
            TokenKind::Parallel => {
                // `parallel action` → declaration; `parallel for` / `parallel {` → stmt
                let span = self.current_span();
                let _ = span;
                if matches!(self.peek_nth(1), TokenKind::Action) {
                    self.advance(); // eat `parallel`
                    return Some(self.parse_action_decl(true, decs));
                } else if let Some(sid) = self.parse_stmt() {
                    return Some(self.module.arena.alloc_item(Item::Stmt(sid)));
                } else {
                    return None;
                }
            }
            TokenKind::Var => {
                // Top-level variable declaration (possibly tuple destructure)
                let start = self.current_span().start;
                self.advance(); // eat `var`
                // Tuple destructure: `var (a, b) = expr`
                if matches!(self.peek(), TokenKind::LParen) {
                    self.advance(); // eat `(`
                    let mut bindings = vec![];
                    loop {
                        self.skip_terminators();
                        if matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                            break;
                        }
                        bindings.push(self.expect_ident_sym("expected binding name"));
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect_tok(&TokenKind::RParen);
                    if !matches!(self.peek(), TokenKind::Assign | TokenKind::Set) {
                        let span = self.current_span();
                        self.error("expected `=` or `set` after binding list", span);
                    } else {
                        self.advance();
                    }
                    let value = self.parse_expr();
                    let end = self.current_span().end;
                    self.skip_one_terminator();
                    return Some(self.module.arena.alloc_item(Item::Destructure {
                        bindings,
                        value,
                        span: Span::new(self.module.file, start, end),
                    }));
                }
                let name = self.expect_ident_sym("expected variable name");
                let ty = if matches!(self.peek(), TokenKind::Minus)
                    && matches!(self.peek_nth(1), TokenKind::Ident(_))
                {
                    let span = self.current_span();
                    self.error("invalid type annotation syntax: did you mean `->`?", span);
                    self.advance(); // eat the erroneous `-`
                    Some(self.parse_type_expr())
                } else if self.eat_type_ann() {
                    Some(self.parse_type_expr())
                } else {
                    None
                };
                let init = if matches!(self.peek(), TokenKind::Assign | TokenKind::Set) {
                    self.advance();
                    Some(self.parse_expr())
                } else {
                    None
                };
                let end = self.current_span().end;
                self.skip_one_terminator();
                Some(self.module.arena.alloc_item(Item::VarDecl {
                    name,
                    ty,
                    init,
                    is_const: false,
                    span: Span::new(self.module.file, start, end),
                }))
            }
            _ => {
                // Top-level expression statement or assignment
                let start = self.current_span().start;
                let expr = self.parse_expr();
                // `x = rhs` or `x set rhs` at module / REPL scope
                if matches!(self.peek(), TokenKind::Assign | TokenKind::Set) {
                    let is_valid_lval = matches!(
                        self.module.arena.get_expr(expr),
                        Expr::Ident { .. } | Expr::Field { .. } | Expr::Index { .. }
                    );
                    if !is_valid_lval {
                        let span = self.module.arena.get_expr(expr).span();
                        self.error("invalid assignment target", span);
                        self.advance(); // eat `=` / `set`
                        self.parse_expr(); // consume RHS
                        self.skip_one_terminator();
                        return None;
                    }
                    self.advance(); // eat `=` / `set`
                    let value = self.parse_expr();
                    let end = self.current_span().end;
                    self.skip_one_terminator();
                    return Some(self.module.arena.alloc_item(Item::Assign {
                        target: expr,
                        value,
                        span: Span::new(self.module.file, start, end),
                    }));
                }
                // Compound assignment at module scope: `x += rhs`, etc.
                let compound_op = match self.peek() {
                    TokenKind::PlusEq => Some(BinOp::Add),
                    TokenKind::MinusEq => Some(BinOp::Sub),
                    TokenKind::StarEq => Some(BinOp::Mul),
                    TokenKind::SlashEq => Some(BinOp::Div),
                    _ => None,
                };
                if let Some(op) = compound_op {
                    self.advance();
                    let rhs = self.parse_expr();
                    let end = self.current_span().end;
                    let span = Span::new(self.module.file, start, end);
                    let bin_expr = self.module.arena.alloc_expr(Expr::Binary {
                        op,
                        lhs: expr,
                        rhs,
                        span,
                    });
                    self.skip_one_terminator();
                    return Some(self.module.arena.alloc_item(Item::Assign {
                        target: expr,
                        value: bin_expr,
                        span,
                    }));
                }
                let _span = self.module.arena.get_expr(expr).span();
                self.skip_one_terminator();
                Some(self.module.arena.alloc_item(Item::ExprStmt(expr)))
            }
        }
    }

    // ── Decorators ────────────────────────────────────────────────────────────

    fn parse_decorators(&mut self) -> Vec<Decorator> {
        let mut decs = vec![];
        while matches!(self.peek(), TokenKind::At) {
            let start = self.current_span().start;
            self.advance(); // eat `@`
            let name = self.expect_ident_sym("expected decorator name");
            let args = if matches!(self.peek(), TokenKind::LParen) {
                self.advance();
                let mut args = vec![];
                while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                    args.push(self.parse_expr());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect_tok(&TokenKind::RParen);
                args
            } else {
                vec![]
            };
            let end = self.current_span().end;
            decs.push(Decorator {
                name,
                args,
                span: Span::new(self.module.file, start, end),
            });
        }
        decs
    }

    // ── Object declaration ────────────────────────────────────────────────────

    fn parse_enum_decl(&mut self) -> ItemId {
        let start = self.current_span().start;
        self.advance(); // eat `enum`
        let name = self.expect_ident_sym("expected enum name");
        self.skip_terminators();
        self.expect_tok(&TokenKind::LBrace);

        let mut variants: Vec<EnumVariantDef> = vec![];

        loop {
            self.skip_terminators();
            match self.peek().clone() {
                TokenKind::RBrace | TokenKind::Eof => break,
                TokenKind::Ident(sym) => {
                    let var_start = self.current_span().start;
                    self.advance(); // eat variant name
                    // Optional payload: `Variant(Type, Type, ...)`
                    let mut payload_types = vec![];
                    if matches!(self.peek(), TokenKind::LParen) {
                        self.advance(); // eat `(`
                        while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                            payload_types.push(self.parse_type_expr());
                            if !self.eat(&TokenKind::Comma) {
                                break;
                            }
                        }
                        self.expect_tok(&TokenKind::RParen);
                    }
                    let var_end = self.current_span().end;
                    variants.push(EnumVariantDef {
                        name: sym,
                        payload_types,
                        span: Span::new(self.module.file, var_start, var_end),
                    });
                    self.eat(&TokenKind::Comma);
                }
                _ => {
                    let span = self.current_span();
                    self.error("expected variant name in enum body", span);
                    self.synchronize();
                }
            }
        }

        let end = self.current_span().end;
        self.expect_tok(&TokenKind::RBrace);
        self.module.arena.alloc_item(Item::EnumDecl {
            name,
            variants,
            span: Span::new(self.module.file, start, end),
        })
    }

    fn parse_object_decl(&mut self) -> ItemId {
        let start = self.current_span().start;
        self.advance(); // eat `object`
        let name = self.expect_ident_sym("expected object name");
        let parent = if self.eat(&TokenKind::Extends) {
            let mut path = vec![self.expect_ident_sym("expected parent name after `extends`")];
            while self.eat(&TokenKind::Dot) {
                path.push(self.expect_ident_sym("expected name after `.` in `extends` path"));
            }
            Some(path)
        } else {
            None
        };
        self.skip_terminators();
        self.expect_tok(&TokenKind::LBrace);

        let mut fields = vec![];
        let mut methods = vec![];

        loop {
            self.skip_terminators();
            // Parse any decorators before the next item in the object body.
            let method_decs = self.parse_decorators();
            if !method_decs.is_empty() {
                self.skip_terminators();
            }
            match self.peek().clone() {
                TokenKind::RBrace | TokenKind::Eof => break,
                TokenKind::Var => {
                    fields.push(self.parse_field_decl());
                }
                TokenKind::Action | TokenKind::Parallel => {
                    let is_par = if matches!(self.peek(), TokenKind::Parallel) {
                        self.advance();
                        true
                    } else {
                        false
                    };
                    methods.push(self.parse_action_decl(is_par, method_decs));
                }
                TokenKind::New => {
                    // Constructor block: `new with (params) { body }`
                    let item_start = self.current_span().start;
                    self.advance(); // eat `new`
                    let params = if self.at_ident(self.sym_with) {
                        self.advance();
                        self.parse_params()
                    } else {
                        vec![]
                    };
                    self.skip_terminators();
                    // `returns <type>` is invalid on a constructor — emit a
                    // targeted error and skip the annotation so the block can
                    // still be parsed (error recovery).
                    if self.at_ident(self.sym_returns) {
                        let returns_span = self.current_span();
                        self.advance(); // eat `returns`
                        self.parse_type_expr(); // consume and discard the type
                        self.error(
                            "constructors cannot declare a return type — the enclosing object is returned implicitly to the caller",
                            returns_span,
                        );
                        self.recovering = false; // allow errors inside the block to surface
                    }
                    let body = self.parse_block();
                    let end = self.current_span().end;
                    let ctor_name = self.interner.intern("new");
                    let ctor_id = self.module.arena.alloc_item(Item::ActionDecl {
                        name: ctor_name,
                        params,
                        return_ty: None,
                        body,
                        decorators: vec![],
                        is_parallel: false,
                        span: Span::new(self.module.file, item_start, end),
                    });
                    methods.push(ctor_id);
                }
                _ => {
                    let span = self.current_span();
                    self.error(
                        "expected field (`var`) or method (`action`) in object body",
                        span,
                    );
                    self.synchronize();
                }
            }
        }

        let end = self.current_span().end;
        self.expect_tok(&TokenKind::RBrace);
        self.module.arena.alloc_item(Item::ObjectDecl {
            name,
            parent,
            fields,
            methods,
            span: Span::new(self.module.file, start, end),
        })
    }

    // ── Field declaration (inside object body) ────────────────────────────────

    pub(crate) fn parse_field_decl(&mut self) -> FieldDecl {
        let start = self.current_span().start;
        self.advance(); // eat `var`
        let name = self.expect_ident_sym("expected field name");
        let ty = if self.eat_type_ann() {
            self.parse_type_expr()
        } else {
            TypeExpr::Dynamic {
                span: self.current_span(),
            }
        };
        let default = if matches!(self.peek(), TokenKind::Assign | TokenKind::Set) {
            self.advance();
            Some(self.parse_expr())
        } else if self.at_ident(self.sym_default) {
            self.advance();
            Some(self.parse_expr())
        } else {
            None
        };
        let end = self.current_span().end;
        self.skip_one_terminator();
        FieldDecl {
            name,
            ty,
            certain: false,
            default,
            span: Span::new(self.module.file, start, end),
        }
    }

    // ── Action declaration ────────────────────────────────────────────────────

    pub(crate) fn parse_action_decl(
        &mut self,
        is_parallel: bool,
        decorators: Vec<fidan_ast::Decorator>,
    ) -> ItemId {
        let start = self.current_span().start;
        self.advance(); // eat `action`
        let name = self.expect_ident_sym("expected action name");

        // Optional `extends TypeName`
        let extends = if self.eat(&TokenKind::Extends) {
            Some(self.expect_ident_sym("expected type name after `extends`"))
        } else {
            None
        };

        // Optional `with (params)` or `with params`
        let params = if self.at_ident(self.sym_with) {
            self.advance(); // eat `with`
            self.parse_params()
        } else {
            vec![]
        };

        // Optional `returns type`
        let return_ty = if self.at_ident(self.sym_returns) {
            self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };

        self.skip_terminators();
        let body = self.parse_block();
        let end = self.current_span().end;

        if let Some(ext) = extends {
            self.module.arena.alloc_item(Item::ExtensionAction {
                name,
                extends: ext,
                params,
                return_ty,
                body,
                decorators,
                is_parallel,
                span: Span::new(self.module.file, start, end),
            })
        } else {
            self.module.arena.alloc_item(Item::ActionDecl {
                name,
                params,
                return_ty,
                body,
                decorators,
                is_parallel,
                span: Span::new(self.module.file, start, end),
            })
        }
    }

    // ── Parameter list ────────────────────────────────────────────────────────
    //
    // Fidan params may be mixed: grouped in parens and ungrouped after `also`/Comma.
    // E.g.: `action init with (certain name oftype string) also optional age oftype integer = 18`

    pub(crate) fn parse_params(&mut self) -> Vec<Param> {
        let mut params = vec![];
        loop {
            self.skip_terminators();
            // Stop markers
            if self.at_ident(self.sym_returns) {
                break;
            }
            match self.peek() {
                TokenKind::LBrace | TokenKind::Eof => break,
                TokenKind::Comma => {
                    self.advance();
                    continue;
                }
                TokenKind::LParen => {
                    // Parenthesized sub-group: `(param, param)`
                    self.advance();
                    loop {
                        self.skip_terminators();
                        if matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                            break;
                        }
                        if matches!(self.peek(), TokenKind::Comma) {
                            self.advance();
                            continue;
                        }
                        match self.peek() {
                            TokenKind::Certain | TokenKind::Optional | TokenKind::Ident(_) => {
                                params.push(self.parse_single_param());
                            }
                            _ => break, // unrecognised token — stop before looping forever
                        }
                    }
                    self.eat(&TokenKind::RParen);
                }
                TokenKind::Certain | TokenKind::Optional | TokenKind::Ident(_) => {
                    params.push(self.parse_single_param());
                }
                _ => break,
            }
        }
        params
    }

    fn parse_single_param(&mut self) -> Param {
        let start = self.current_span().start;
        let certain = self.eat(&TokenKind::Certain);
        let optional = !certain && self.eat(&TokenKind::Optional);
        let name = self.expect_ident_sym("expected parameter name");
        let ty = if self.eat_type_ann() {
            self.parse_type_expr()
        } else {
            TypeExpr::Dynamic {
                span: self.current_span(),
            }
        };
        let default = if matches!(self.peek(), TokenKind::Assign | TokenKind::Set) {
            self.advance();
            Some(self.parse_expr())
        } else if self.at_ident(self.sym_default) {
            self.advance();
            Some(self.parse_expr())
        } else {
            None
        };
        let end = self.current_span().end;
        Param {
            name,
            ty,
            certain,
            optional,
            default,
            span: Span::new(self.module.file, start, end),
        }
    }

    // ── Type expressions ──────────────────────────────────────────────────────

    pub(crate) fn parse_type_expr(&mut self) -> TypeExpr {
        let span = self.current_span();
        match self.peek().clone() {
            TokenKind::Dynamic => {
                self.advance();
                TypeExpr::Dynamic { span }
            }
            TokenKind::Nothing => {
                self.advance();
                TypeExpr::Nothing { span }
            }
            // `action` as a type — first-class callable type
            TokenKind::Action => {
                self.advance();
                let name = self.interner.intern("action");
                TypeExpr::Named { name, span }
            }
            // `tuple` keyword — untyped tuple, elements unknown
            TokenKind::Tuple => {
                self.advance();
                TypeExpr::Tuple {
                    elements: vec![],
                    span,
                }
            }
            TokenKind::Ident(name) => {
                self.advance();
                let base = TypeExpr::Named { name, span };
                // `list oftype T`, `list -> T`, `Shared oftype T`, etc.
                if self.eat_type_ann() {
                    let param = self.parse_type_expr();
                    let end = param.span_end();
                    TypeExpr::Oftype {
                        base: Box::new(base),
                        param: Box::new(param),
                        span: Span::new(self.module.file, span.start, end),
                    }
                } else {
                    base
                }
            }
            TokenKind::Shared => {
                self.advance();
                let param = if self.eat_type_ann() {
                    self.parse_type_expr()
                } else {
                    TypeExpr::Dynamic {
                        span: self.current_span(),
                    }
                };
                let end = param.span_end();
                let name = self.interner.intern("Shared");
                TypeExpr::Oftype {
                    base: Box::new(TypeExpr::Named { name, span }),
                    param: Box::new(param),
                    span: Span::new(self.module.file, span.start, end),
                }
            }
            TokenKind::Pending => {
                self.advance();
                let param = if self.eat_type_ann() {
                    self.parse_type_expr()
                } else {
                    TypeExpr::Dynamic {
                        span: self.current_span(),
                    }
                };
                let end = param.span_end();
                let name = self.interner.intern("Pending");
                TypeExpr::Oftype {
                    base: Box::new(TypeExpr::Named { name, span }),
                    param: Box::new(param),
                    span: Span::new(self.module.file, span.start, end),
                }
            }
            TokenKind::LParen => {
                // Tuple type: `(T1, T2, ...)` — always emits TypeExpr::Tuple.
                // Single-element `(T)` = 1-tuple (wrapping is not transparent in types).
                self.advance(); // eat `(`
                let mut types: Vec<Box<TypeExpr>> = vec![];
                loop {
                    self.skip_terminators();
                    if matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                        break;
                    }
                    types.push(Box::new(self.parse_type_expr()));
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                let end = self.current_span().end;
                self.eat(&TokenKind::RParen);
                TypeExpr::Tuple {
                    elements: types,
                    span: Span::new(self.module.file, span.start, end),
                }
            }
            _ => {
                self.error("expected type expression", span);
                TypeExpr::Dynamic { span }
            }
        }
    }

    // ── Test block declaration ────────────────────────────────────────────────
    //
    // Syntax: `test "name" { stmts }`
    //
    // The name is any non-empty string literal.  The body is a regular
    // statement block.  Test blocks are only executed by `fidan test`.

    fn parse_test_decl(&mut self) -> ItemId {
        let start = self.current_span().start;
        self.advance(); // eat `test`

        // Expect a string literal name.
        let name = if let TokenKind::LitString(s) = self.peek() {
            let s = s.clone();
            self.advance();
            s
        } else {
            let span = self.current_span();
            self.error("expected string literal after `test`", span);
            String::from("<unnamed>")
        };

        self.skip_terminators();
        let body = self.parse_block();
        let end = self.current_span().end;
        self.skip_one_terminator();

        self.module.arena.alloc_item(Item::TestDecl {
            name,
            body,
            span: Span::new(self.module.file, start, end),
        })
    }

    fn parse_use_decl(&mut self, re_export: bool) -> ItemId {
        let start = self.current_span().start;
        self.advance(); // eat `use` / `export use`

        // File-path import: `use "some/path"` or `export use "some/path"`
        let file_path = if let TokenKind::LitString(s) = self.peek() {
            Some(s.clone())
        } else {
            None
        };
        if let Some(raw) = file_path {
            self.advance(); // eat the string literal
            let end = self.current_span().end;
            let alias = if self.eat(&TokenKind::As) {
                Some(self.expect_ident_sym("expected alias"))
            } else {
                None
            };
            self.skip_one_terminator();
            let sym = self.interner.intern(&raw);
            return self.module.arena.alloc_item(Item::Use {
                path: vec![sym],
                alias,
                re_export,
                grouped: false,
                span: Span::new(self.module.file, start, end),
            });
        }

        let mut path = vec![self.expect_ident_sym("expected module name")];

        // Grouped import flag: `use std.io.{print, readFile, writeFile}`
        let mut grouped_names: Option<Vec<Symbol>> = None;

        loop {
            if !self.eat(&TokenKind::Dot) && !self.eat(&TokenKind::DoubleColon) {
                break;
            }
            // Grouped import: `use std.io.{print, readFile}`
            if matches!(self.peek(), TokenKind::LBrace) {
                self.advance(); // eat `{`
                let mut names = vec![];
                loop {
                    self.skip_terminators();
                    if matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
                        break;
                    }
                    names.push(self.expect_ident_sym("expected import name"));
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.eat(&TokenKind::RBrace);
                grouped_names = Some(names);
                break;
            }
            path.push(self.expect_ident_sym("expected path segment"));
        }

        let end = self.current_span().end;
        self.skip_one_terminator();

        if let Some(names) = grouped_names {
            // Emit one Use item per grouped name; push extras directly onto the module.
            let mut first_id: Option<ItemId> = None;
            for name in names {
                let mut full_path = path.clone();
                full_path.push(name);
                let id = self.module.arena.alloc_item(Item::Use {
                    path: full_path,
                    alias: None,
                    re_export,
                    grouped: true,
                    span: Span::new(self.module.file, start, end),
                });
                if first_id.is_none() {
                    first_id = Some(id);
                } else {
                    self.module.items.push(id);
                }
            }
            // If the group was empty, emit a single Use with the prefix path.
            first_id.unwrap_or_else(|| {
                self.module.arena.alloc_item(Item::Use {
                    path,
                    alias: None,
                    re_export,
                    grouped: false,
                    span: Span::new(self.module.file, start, end),
                })
            })
        } else {
            let alias = if self.eat(&TokenKind::As) {
                Some(self.expect_ident_sym("expected alias"))
            } else {
                None
            };
            self.module.arena.alloc_item(Item::Use {
                path,
                alias,
                re_export,
                grouped: false,
                span: Span::new(self.module.file, start, end),
            })
        }
    }

    // ── Block & statement parsing ─────────────────────────────────────────────

    pub(crate) fn parse_block(&mut self) -> Vec<StmtId> {
        self.expect_tok(&TokenKind::LBrace);
        let mut stmts = vec![];
        loop {
            if self.bail {
                break;
            }
            self.skip_terminators();
            if matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
                break;
            }
            let pos_before = self.pos;
            if let Some(s) = self.parse_stmt() {
                stmts.push(s);
            } else {
                self.synchronize();
                if self.pos == pos_before {
                    self.advance();
                }
            }
        }
        self.expect_tok(&TokenKind::RBrace);
        stmts
    }

    pub fn parse_stmt(&mut self) -> Option<StmtId> {
        self.skip_terminators();
        let s = match self.peek().clone() {
            TokenKind::Const => self.parse_var_decl_stmt(true),
            TokenKind::Var => self.parse_var_decl_stmt(false),
            TokenKind::Return => self.parse_return_stmt(),
            TokenKind::Break => {
                let sp = self.current_span();
                self.advance();
                self.skip_one_terminator();
                self.module.arena.alloc_stmt(Stmt::Break { span: sp })
            }
            TokenKind::Continue => {
                let sp = self.current_span();
                self.advance();
                self.skip_one_terminator();
                self.module.arena.alloc_stmt(Stmt::Continue { span: sp })
            }
            TokenKind::If => self.parse_if_stmt(),
            TokenKind::For => self.parse_for_stmt(),
            TokenKind::While => self.parse_while_stmt(),
            TokenKind::Attempt => self.parse_attempt_stmt(),
            TokenKind::Panic => self.parse_panic_stmt(),
            TokenKind::Check => self.parse_check_stmt(),
            TokenKind::Parallel => {
                let span = self.current_span();
                self.advance();
                if matches!(self.peek(), TokenKind::For) {
                    self.parse_parallel_for()
                } else if matches!(self.peek(), TokenKind::LBrace) {
                    self.parse_task_block(true)
                } else {
                    self.error("expected `for` or `{` after `parallel`", span);
                    return Some(self.error_stmt(span));
                }
            }
            TokenKind::Concurrent => {
                self.advance();
                self.parse_task_block(false)
            }
            TokenKind::RBrace | TokenKind::Eof => return None,
            _ => self.parse_assign_or_expr_stmt(),
        };
        Some(s)
    }

    fn parse_var_decl_stmt(&mut self, is_const: bool) -> StmtId {
        let start = self.current_span().start;
        // Eat `const` if present, then eat `var`.
        if is_const {
            self.advance(); // eat `const`
        }
        self.advance(); // eat `var`

        // Tuple destructure: `var (a, b) = expr`
        if matches!(self.peek(), TokenKind::LParen) {
            self.advance(); // eat `(`
            let mut bindings = vec![];
            loop {
                self.skip_terminators();
                if matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                    break;
                }
                bindings.push(self.expect_ident_sym("expected binding name"));
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect_tok(&TokenKind::RParen);
            // Require `=` / `set`
            if !matches!(self.peek(), TokenKind::Assign | TokenKind::Set) {
                let span = self.current_span();
                self.error("expected `=` or `set` after binding list", span);
            } else {
                self.advance();
            }
            let value = self.parse_expr();
            let end = self.current_span().end;
            self.skip_one_terminator();
            return self.module.arena.alloc_stmt(Stmt::Destructure {
                bindings,
                value,
                span: Span::new(self.module.file, start, end),
            });
        }

        let name = self.expect_ident_sym("expected variable name");
        let ty = if matches!(self.peek(), TokenKind::Minus)
            && matches!(self.peek_nth(1), TokenKind::Ident(_))
        {
            // Catch the common typo `var x-integer` (missing `>` in `->`)
            // before it silently produces a confusing type-mismatch later.
            let span = self.current_span();
            self.error("invalid type annotation syntax: did you mean `->`?", span);
            self.advance(); // eat the erroneous `-`
            Some(self.parse_type_expr())
        } else if self.eat_type_ann() {
            Some(self.parse_type_expr())
        } else {
            None
        };
        let init = if matches!(self.peek(), TokenKind::Assign | TokenKind::Set) {
            self.advance();
            Some(self.parse_expr())
        } else {
            None
        };
        let end = self.current_span().end;
        self.skip_one_terminator();
        self.module.arena.alloc_stmt(Stmt::VarDecl {
            name,
            ty,
            init,
            is_const,
            span: Span::new(self.module.file, start, end),
        })
    }

    fn parse_return_stmt(&mut self) -> StmtId {
        let start = self.current_span().start;
        self.advance(); // eat `return`
        let value = if !matches!(
            self.peek(),
            TokenKind::Newline | TokenKind::Semicolon | TokenKind::RBrace | TokenKind::Eof
        ) {
            Some(self.parse_expr())
        } else {
            None
        };
        let end = self.current_span().end;
        self.skip_one_terminator();
        self.module.arena.alloc_stmt(Stmt::Return {
            value,
            span: Span::new(self.module.file, start, end),
        })
    }

    fn parse_panic_stmt(&mut self) -> StmtId {
        let start = self.current_span().start;
        self.advance(); // eat `panic`
        self.expect_tok(&TokenKind::LParen);
        let value = self.parse_expr();
        let end = self.current_span().end;
        self.expect_tok(&TokenKind::RParen);
        self.skip_one_terminator();
        self.module.arena.alloc_stmt(Stmt::Panic {
            value,
            span: Span::new(self.module.file, start, end),
        })
    }

    // ── Check / pattern-match statement ──────────────────────────────────────
    //
    // Syntax:
    //   check <expr> {
    //     <pattern> => { <body> }   -- block arm
    //     <pattern> => <expr>       -- expression arm
    //     otherwise => { <body> }   -- wildcard arm
    //   }

    fn parse_check_stmt(&mut self) -> StmtId {
        let start = self.current_span().start;
        self.advance(); // eat `check`
        let scrutinee = self.parse_expr();
        self.skip_terminators();
        self.expect_tok(&TokenKind::LBrace);
        let mut arms = vec![];
        loop {
            self.skip_terminators();
            if matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
                break;
            }
            let arm_start = self.current_span().start;
            // Pattern: `otherwise` becomes wildcard (`_`), else any expression
            let pattern = if matches!(self.peek(), TokenKind::Otherwise) {
                let sp = self.current_span();
                self.advance();
                let wild = self.interner.intern("_");
                self.module.arena.alloc_expr(Expr::Ident {
                    name: wild,
                    span: sp,
                })
            } else {
                self.parse_expr()
            };
            self.expect_tok(&TokenKind::FatArrow); // `=>`
            // Body: block `{ ... }` or a single expression wrapped as ExprStmt
            let body = if matches!(self.peek(), TokenKind::LBrace) {
                self.parse_block()
            } else {
                let expr = self.parse_expr();
                let expr_span = self.module.arena.get_expr(expr).span();
                self.skip_one_terminator();
                vec![self.module.arena.alloc_stmt(Stmt::Expr {
                    expr,
                    span: expr_span,
                })]
            };
            let arm_end = self.current_span().end;
            arms.push(CheckArm {
                pattern,
                body,
                span: Span::new(self.module.file, arm_start, arm_end),
            });
        }
        let end = self.current_span().end;
        self.expect_tok(&TokenKind::RBrace);
        self.module.arena.alloc_stmt(Stmt::Check {
            scrutinee,
            arms,
            span: Span::new(self.module.file, start, end),
        })
    }

    fn parse_if_stmt(&mut self) -> StmtId {
        let start = self.current_span().start;
        self.advance(); // eat `if`
        let condition = self.parse_expr();
        self.skip_terminators();
        let then_body = self.parse_block();

        let mut else_ifs = vec![];
        let mut else_body = None;

        loop {
            self.skip_terminators();
            if matches!(self.peek(), TokenKind::Otherwise) {
                self.advance(); // eat `otherwise` / `else` (both lex to Otherwise)
                // `otherwise when` or `else if` (else→Otherwise, then If) = else-if chain
                if matches!(self.peek(), TokenKind::When | TokenKind::If) {
                    self.advance(); // eat `when` or `if`
                    let cond = self.parse_expr();
                    self.skip_terminators();
                    let body = self.parse_block();
                    let end = self.current_span().end;
                    else_ifs.push(ElseIf {
                        condition: cond,
                        body,
                        span: Span::new(self.module.file, start, end),
                    });
                } else {
                    // `otherwise { ... }` / `else { ... }` = plain else
                    self.skip_terminators();
                    else_body = Some(self.parse_block());
                    break;
                }
            } else {
                break;
            }
        }

        let end = self.current_span().end;
        self.module.arena.alloc_stmt(Stmt::If {
            condition,
            then_body,
            else_ifs,
            else_body,
            span: Span::new(self.module.file, start, end),
        })
    }

    fn parse_for_stmt(&mut self) -> StmtId {
        let start = self.current_span().start;
        self.advance(); // eat `for`
        let binding = self.expect_ident_sym("expected loop variable");
        self.expect_tok(&TokenKind::In);
        let iterable = self.parse_expr();
        self.skip_terminators();
        let body = self.parse_block();
        let end = self.current_span().end;
        self.module.arena.alloc_stmt(Stmt::For {
            binding,
            iterable,
            body,
            span: Span::new(self.module.file, start, end),
        })
    }

    fn parse_while_stmt(&mut self) -> StmtId {
        let start = self.current_span().start;
        self.advance(); // eat `while`
        let condition = self.parse_expr();
        self.skip_terminators();
        let body = self.parse_block();
        let end = self.current_span().end;
        self.module.arena.alloc_stmt(Stmt::While {
            condition,
            body,
            span: Span::new(self.module.file, start, end),
        })
    }

    fn parse_attempt_stmt(&mut self) -> StmtId {
        let start = self.current_span().start;
        self.advance(); // eat `attempt`/`try`
        self.skip_terminators();
        let body = self.parse_block();

        let mut catches = vec![];
        let mut otherwise = None;
        let mut finally = None;

        loop {
            self.skip_terminators();
            match self.peek().clone() {
                TokenKind::Catch => {
                    self.advance();
                    let binding = if matches!(self.peek(), TokenKind::Ident(_)) {
                        Some(self.expect_ident_sym("expected catch binding"))
                    } else {
                        None
                    };
                    let ty = if self.eat_type_ann() {
                        Some(self.parse_type_expr())
                    } else {
                        None
                    };
                    self.skip_terminators();
                    let cbody = self.parse_block();
                    let end = self.current_span().end;
                    catches.push(CatchClause {
                        binding,
                        ty,
                        body: cbody,
                        span: Span::new(self.module.file, start, end),
                    });
                }
                TokenKind::Otherwise => {
                    self.advance();
                    // `otherwise {` = no-error block (not `otherwise when`)
                    if matches!(self.peek(), TokenKind::When) {
                        // This shouldn't occur inside attempt — ignore
                    } else {
                        self.skip_terminators();
                        otherwise = Some(self.parse_block());
                    }
                }
                TokenKind::Finally => {
                    self.advance();
                    self.skip_terminators();
                    finally = Some(self.parse_block());
                }
                _ => break,
            }
        }

        let end = self.current_span().end;
        self.module.arena.alloc_stmt(Stmt::Attempt {
            body,
            catches,
            otherwise,
            finally,
            span: Span::new(self.module.file, start, end),
        })
    }

    fn parse_parallel_for(&mut self) -> StmtId {
        let start = self.current_span().start;
        self.advance(); // eat `for`
        let binding = self.expect_ident_sym("expected loop variable");
        self.expect_tok(&TokenKind::In);
        let iterable = self.parse_expr();
        self.skip_terminators();
        let body = self.parse_block();
        let end = self.current_span().end;
        self.module.arena.alloc_stmt(Stmt::ParallelFor {
            binding,
            iterable,
            body,
            span: Span::new(self.module.file, start, end),
        })
    }

    fn parse_task_block(&mut self, is_parallel: bool) -> StmtId {
        let start = self.current_span().start;
        self.expect_tok(&TokenKind::LBrace);
        let mut tasks = vec![];
        loop {
            if self.bail {
                break;
            }
            self.skip_terminators();
            if matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
                break;
            }
            if matches!(self.peek(), TokenKind::Task) {
                let ts = self.current_span().start;
                self.advance();
                let name = if matches!(self.peek(), TokenKind::Ident(_)) {
                    Some(self.expect_ident_sym("task name"))
                } else {
                    None
                };
                self.skip_terminators();
                let body = self.parse_block();
                let te = self.current_span().end;
                tasks.push(Task {
                    name,
                    body,
                    span: Span::new(self.module.file, ts, te),
                });
            } else {
                let span = self.current_span();
                self.error("expected `task` inside concurrent/parallel block", span);
                let pos_before = self.pos;
                self.synchronize();
                // If synchronize made no progress (hit a keyword sync-point immediately),
                // force-consume one token to guarantee the loop terminates.
                if self.pos == pos_before {
                    self.advance();
                }
            }
        }
        let end = self.current_span().end;
        self.expect_tok(&TokenKind::RBrace);
        self.module.arena.alloc_stmt(Stmt::ConcurrentBlock {
            is_parallel,
            tasks,
            span: Span::new(self.module.file, start, end),
        })
    }

    fn parse_assign_or_expr_stmt(&mut self) -> StmtId {
        let start = self.current_span().start;
        let expr_id = self.parse_expr();
        // Assignment: `lhs = rhs` or `lhs set rhs`
        if matches!(self.peek(), TokenKind::Assign | TokenKind::Set) {
            // Reject non-lvalue targets (e.g. `x+y = 1`, `+x = 1`) before they
            // produce confusing type-mismatch errors in the typechecker.
            let is_valid_lval = matches!(
                self.module.arena.get_expr(expr_id),
                Expr::Ident { .. } | Expr::Field { .. } | Expr::Index { .. }
            );
            if !is_valid_lval {
                let span = self.module.arena.get_expr(expr_id).span();
                self.error("invalid assignment target", span);
                self.advance(); // eat `=` / `set`
                self.parse_expr(); // consume RHS so parsing can continue
                let end = self.current_span().end;
                self.skip_one_terminator();
                return self.error_stmt(Span::new(self.module.file, start, end));
            }
            self.advance();
            let value = self.parse_expr();
            let end = self.current_span().end;
            self.skip_one_terminator();
            return self.module.arena.alloc_stmt(Stmt::Assign {
                target: expr_id,
                value,
                span: Span::new(self.module.file, start, end),
            });
        }
        // Compound assignment: `lhs += rhs`, `lhs -= rhs`, `lhs *= rhs`, `lhs /= rhs`
        // Desugar to `lhs = lhs op rhs`.
        let compound_op = match self.peek() {
            TokenKind::PlusEq => Some(BinOp::Add),
            TokenKind::MinusEq => Some(BinOp::Sub),
            TokenKind::StarEq => Some(BinOp::Mul),
            TokenKind::SlashEq => Some(BinOp::Div),
            _ => None,
        };
        if let Some(op) = compound_op {
            self.advance(); // consume the op= token
            let rhs = self.parse_expr();
            let end = self.current_span().end;
            let span = Span::new(self.module.file, start, end);
            // Build `lhs op rhs`
            let bin_expr = self.module.arena.alloc_expr(Expr::Binary {
                op,
                lhs: expr_id,
                rhs,
                span,
            });
            self.skip_one_terminator();
            return self.module.arena.alloc_stmt(Stmt::Assign {
                target: expr_id,
                value: bin_expr,
                span,
            });
        }
        // Expression statement
        let end = self.module.arena.get_expr(expr_id).span().end;
        self.skip_one_terminator();
        self.module.arena.alloc_stmt(Stmt::Expr {
            expr: expr_id,
            span: Span::new(self.module.file, start, end),
        })
    }
}
