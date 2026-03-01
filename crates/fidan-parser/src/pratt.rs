// fidan-parser — Pratt (top-down operator precedence) expression parser
use fidan_ast::{Arg, BinOp, CheckArm, Expr, ExprId, InterpPart, Stmt, UnOp};
use fidan_lexer::TokenKind;
use fidan_source::Span;
use crate::parser::Parser;

// ── Binding-power table (ascending precedence) ───────────────────────────────
//
//  0  ternary `value if condition else fallback`  (handled separately)
//  1  `??`  null-coalesce
//  3  `or`
//  5  `and`
//  7  comparison  `== != < > <= >= is`
//  9  additive    `+ -`
// 11  multiplicative `* / %`
// 13  power/xor `**` right-assoc; `^` bitwise-xor (same precedence slot)
// 15  unary prefix  `- not spawn await`
// 17  postfix   `.`  `()`  `[]`

impl<'t> Parser<'t> {
    // ── Public entry point ────────────────────────────────────────────────────

    pub(crate) fn parse_expr(&mut self) -> ExprId {
        let lhs = self.parse_expr_bp(0);
        self.maybe_ternary(lhs)
    }

    // ── Ternary: `then_val if condition else else_val` ────────────────────────

    fn maybe_ternary(&mut self, then_val: ExprId) -> ExprId {
        if !matches!(self.peek(), TokenKind::If) {
            return then_val;
        }
        let start = self.module.arena.get_expr(then_val).span().start;
        self.advance(); // eat `if`

        // Special Fidan shorthand: `if is not nothing` (implicit subject = then_val)
        let condition = match self.peek().clone() {
            TokenKind::Is => {
                self.advance(); // eat `is`
                if matches!(self.peek(), TokenKind::Not) {
                    self.advance(); // eat `not`
                    let rhs_span = self.current_span();
                    if matches!(self.peek(), TokenKind::Nothing) {
                        self.advance();
                        let n = self.module.arena.alloc_expr(Expr::Nothing { span: rhs_span });
                        let end = rhs_span.end;
                        self.module.arena.alloc_expr(Expr::Binary {
                            op: BinOp::NotEq, lhs: then_val, rhs: n,
                            span: Span::new(self.module.file, start, end),
                        })
                    } else {
                        let rhs = self.parse_expr_bp(7);
                        let end = self.module.arena.get_expr(rhs).span().end;
                        self.module.arena.alloc_expr(Expr::Binary {
                            op: BinOp::NotEq, lhs: then_val, rhs,
                            span: Span::new(self.module.file, start, end),
                        })
                    }
                } else {
                    let rhs = self.parse_expr_bp(7);
                    let end = self.module.arena.get_expr(rhs).span().end;
                    self.module.arena.alloc_expr(Expr::Binary {
                        op: BinOp::Eq, lhs: then_val, rhs,
                        span: Span::new(self.module.file, start, end),
                    })
                }
            }
            TokenKind::Eq | TokenKind::NotEq | TokenKind::Gt
            | TokenKind::GtEq | TokenKind::Lt | TokenKind::LtEq => {
                let op_tok = self.advance().kind.clone();
                let op  = Self::tok_to_binop_cmp(&op_tok).unwrap();
                let rhs = self.parse_expr_bp(7);
                let end = self.module.arena.get_expr(rhs).span().end;
                self.module.arena.alloc_expr(Expr::Binary {
                    op, lhs: then_val, rhs,
                    span: Span::new(self.module.file, start, end),
                })
            }
            _ => self.parse_expr_bp(0),
        };

        // `else` and `otherwise` both lex to `TokenKind::Otherwise`
        if !self.eat_ident(self.sym_else) && !self.eat(&TokenKind::Otherwise) {
            let sp = self.current_span();
            self.error("expected `else` in ternary expression", sp);
        }

        let else_val = self.parse_expr_bp(0);
        let end      = self.module.arena.get_expr(else_val).span().end;
        self.module.arena.alloc_expr(Expr::Ternary {
            condition, then_val, else_val,
            span: Span::new(self.module.file, start, end),
        })
    }

    // ── Pratt loop ────────────────────────────────────────────────────────────

    pub(crate) fn parse_expr_bp(&mut self, min_bp: u8) -> ExprId {
        let mut lhs = self.parse_prefix();

        loop {
            // `is not` → NotEq normalization (special two-token sequence)
            if matches!(self.peek(), TokenKind::Is)
                && matches!(self.peek_nth(1), TokenKind::Not)
            {
                if 7 < min_bp { break; }
                let start = self.module.arena.get_expr(lhs).span().start;
                self.advance(); // eat `is`
                self.advance(); // eat `not`
                let rhs = self.parse_expr_bp(8);
                let end = self.module.arena.get_expr(rhs).span().end;
                lhs = self.module.arena.alloc_expr(Expr::Binary {
                    op: BinOp::NotEq, lhs, rhs,
                    span: Span::new(self.module.file, start, end),
                });
                continue;
            }

            let Some((l_bp, r_bp)) = self.infix_bp(self.peek()) else { break };
            if l_bp < min_bp { break; }

            let start   = self.module.arena.get_expr(lhs).span().start;
            let op_kind = self.advance().kind.clone();

            match &op_kind {
                // ── Postfix: call ─────────────────────────────────────────────
                TokenKind::LParen => {
                    lhs = self.parse_call(lhs, start);
                    continue;
                }
                // ── Postfix: member access ────────────────────────────────────
                TokenKind::Dot => {
                    let field = self.expect_field_name();
                    let end   = self.current_span().end;
                    lhs = self.module.arena.alloc_expr(Expr::Field {
                        object: lhs, field,
                        span: Span::new(self.module.file, start, end),
                    });
                    continue;
                }
                // ── Postfix: index ────────────────────────────────────────────
                TokenKind::LBracket => {
                    let index = self.parse_expr_bp(0);
                    let end   = self.current_span().end;
                    self.expect_tok(&TokenKind::RBracket);
                    lhs = self.module.arena.alloc_expr(Expr::Index {
                        object: lhs, index,
                        span: Span::new(self.module.file, start, end),
                    });
                    continue;
                }
                // ── NullCoalesce ──────────────────────────────────────────────
                TokenKind::NullCoalesce => {
                    let rhs = self.parse_expr_bp(r_bp);
                    let end = self.module.arena.get_expr(rhs).span().end;
                    lhs = self.module.arena.alloc_expr(Expr::NullCoalesce {
                        lhs, rhs,
                        span: Span::new(self.module.file, start, end),
                    });
                    continue;
                }
                _ => {}
            }

            // ── Binary operator ───────────────────────────────────────────────
            if let Some(op) = Self::tok_to_binop(&op_kind) {
                let rhs = self.parse_expr_bp(r_bp);
                let end = self.module.arena.get_expr(rhs).span().end;
                lhs = self.module.arena.alloc_expr(Expr::Binary {
                    op, lhs, rhs,
                    span: Span::new(self.module.file, start, end),
                });
            }
        }

        lhs
    }

    // ── Prefix / primary ──────────────────────────────────────────────────────

    fn parse_prefix(&mut self) -> ExprId {
        let span = self.current_span();
        match self.peek().clone() {
            TokenKind::Minus => {
                self.advance();
                let operand = self.parse_expr_bp(15);
                let end     = self.module.arena.get_expr(operand).span().end;
                self.module.arena.alloc_expr(Expr::Unary {
                    op: UnOp::Neg, operand,
                    span: Span::new(self.module.file, span.start, end),
                })
            }
            // Unary plus — no semantic effect, just parse the operand
            TokenKind::Plus => {
                self.advance();
                self.parse_expr_bp(15)
            }
            TokenKind::Not => {
                self.advance();
                let operand = self.parse_expr_bp(15);
                let end     = self.module.arena.get_expr(operand).span().end;
                self.module.arena.alloc_expr(Expr::Unary {
                    op: UnOp::Not, operand,
                    span: Span::new(self.module.file, span.start, end),
                })
            }
            TokenKind::Spawn => {
                self.advance();
                let expr = self.parse_expr_bp(15);
                let end  = self.module.arena.get_expr(expr).span().end;
                self.module.arena.alloc_expr(Expr::Spawn {
                    expr, span: Span::new(self.module.file, span.start, end),
                })
            }
            TokenKind::Await => {
                self.advance();
                let expr = self.parse_expr_bp(15);
                let end  = self.module.arena.get_expr(expr).span().end;
                self.module.arena.alloc_expr(Expr::Await {
                    expr, span: Span::new(self.module.file, span.start, end),
                })
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> ExprId {
        let span = self.current_span();
        match self.peek().clone() {
            TokenKind::LitInteger(v) => { self.advance(); self.module.arena.alloc_expr(Expr::IntLit  { value: v, span }) }
            TokenKind::LitFloat(v)   => { self.advance(); self.module.arena.alloc_expr(Expr::FloatLit { value: v, span }) }
            TokenKind::LitBool(b)    => { self.advance(); self.module.arena.alloc_expr(Expr::BoolLit  { value: b, span }) }
            TokenKind::Nothing       => { self.advance(); self.module.arena.alloc_expr(Expr::Nothing   { span }) }
            TokenKind::This          => { self.advance(); self.module.arena.alloc_expr(Expr::This      { span }) }
            TokenKind::Parent        => { self.advance(); self.module.arena.alloc_expr(Expr::Parent    { span }) }
            TokenKind::LitString(s)  => { self.advance(); self.parse_string_interp(s, span) }
            TokenKind::Ident(sym) => {
                // Don't consume contextual keywords that belong to outer syntax
                if sym == self.sym_else { return self.error_expr(span); }
                self.advance();
                self.module.arena.alloc_expr(Expr::Ident { name: sym, span })
            }
            TokenKind::LParen => {
                self.advance(); // eat `(`
                let first = self.parse_expr();
                // If a comma follows, it's a tuple literal: `(a, b, ...)`.
                if self.eat(&TokenKind::Comma) {
                    let mut elements = vec![first];
                    loop {
                        self.skip_terminators();
                        if matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) { break; }
                        elements.push(self.parse_expr());
                        if !self.eat(&TokenKind::Comma) { break; }
                    }
                    let end = self.current_span().end;
                    self.expect_tok(&TokenKind::RParen);
                    self.module.arena.alloc_expr(Expr::Tuple {
                        elements,
                        span: Span::new(self.module.file, span.start, end),
                    })
                } else {
                    // Single expression — transparent grouping.
                    self.expect_tok(&TokenKind::RParen);
                    first
                }
            }
            TokenKind::LBracket => {
                self.advance();
                let mut elems = vec![];
                while !matches!(self.peek(), TokenKind::RBracket | TokenKind::Eof) {
                    elems.push(self.parse_expr());
                    if !self.eat(&TokenKind::Comma) { break; }
                }
                let end = self.current_span().end;
                self.expect_tok(&TokenKind::RBracket);
                self.module.arena.alloc_expr(Expr::List {
                    elements: elems,
                    span: Span::new(self.module.file, span.start, end),
                })
            }
            TokenKind::LBrace => {
                // Dict literal: `{ key: value, key: value }`
                // Blocks are never primary expressions in Fidan, so `{` here is always a dict.
                self.advance(); // eat `{`
                let mut entries = vec![];
                loop {
                    self.skip_terminators();
                    if matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) { break; }
                    let key   = self.parse_expr();
                    self.expect_tok(&TokenKind::Colon);
                    let value = self.parse_expr();
                    entries.push((key, value));
                    // Allow comma or newline between entries; trailing comma/newline is fine
                    if !self.eat(&TokenKind::Comma) {
                        self.skip_terminators();
                    }
                }
                let end = self.current_span().end;
                self.expect_tok(&TokenKind::RBrace);
                self.module.arena.alloc_expr(Expr::Dict {
                    entries,
                    span: Span::new(self.module.file, span.start, end),
                })
            }
            TokenKind::Check => {
                // `check <expr> { pattern => body, ... }` used as an expression-value
                self.advance(); // eat `check`
                let scrutinee = self.parse_expr_bp(0);
                self.skip_terminators();
                self.expect_tok(&TokenKind::LBrace);
                let mut arms = vec![];
                loop {
                    self.skip_terminators();
                    if matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) { break; }
                    let arm_start = self.current_span().start;
                    let pattern = if matches!(self.peek(), TokenKind::Otherwise) {
                        let sp = self.current_span();
                        self.advance();
                        let wild = self.interner.intern("_");
                        self.module.arena.alloc_expr(Expr::Ident { name: wild, span: sp })
                    } else {
                        self.parse_expr_bp(0)
                    };
                    self.expect_tok(&TokenKind::FatArrow);
                    let body = if matches!(self.peek(), TokenKind::LBrace) {
                        self.parse_block()
                    } else {
                        let e = self.parse_expr_bp(0);
                        let es = self.module.arena.get_expr(e).span();
                        self.skip_one_terminator();
                        vec![self.module.arena.alloc_stmt(Stmt::Expr { expr: e, span: es })]
                    };
                    let arm_end = self.current_span().end;
                    arms.push(CheckArm { pattern, body, span: Span::new(self.module.file, arm_start, arm_end) });
                }
                let end = self.current_span().end;
                self.expect_tok(&TokenKind::RBrace);
                self.module.arena.alloc_expr(Expr::Check {
                    scrutinee, arms,
                    span: Span::new(self.module.file, span.start, end),
                })
            }
            // `Shared(value)` / `Pending(value)` — wrap keyword as Ident so infix `(` handles the call
            TokenKind::Shared | TokenKind::Pending => {
                let name_str = if matches!(self.peek(), TokenKind::Shared) { "Shared" } else { "Pending" };
                self.advance();
                let name = self.interner.intern(name_str);
                self.module.arena.alloc_expr(Expr::Ident { name, span })
            }
            _ => {
                // Always advance so callers never loop on an unrecognised token.
                self.error(&format!("unexpected token in expression: {:?}", self.peek()), span);
                self.advance();
                self.error_expr(span)
            }
        }
    }

    // ── Call argument list ─────────────────────────────────────────────────────
    // `(` already consumed by caller.

    fn parse_call(&mut self, callee: ExprId, start: u32) -> ExprId {
        let mut args = vec![];
        while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
            self.skip_terminators();
            if matches!(self.peek(), TokenKind::RParen) { break; }

            let arg_start = self.current_span().start;

            // Named argument: `name set value` or `name = value`
            let arg = if let TokenKind::Ident(sym) = self.peek().clone() {
                if matches!(self.peek_nth(1), TokenKind::Set | TokenKind::Assign) {
                    let name = sym;
                    self.advance(); // eat ident
                    self.advance(); // eat set/=
                    let value = self.parse_expr();
                    let end   = self.module.arena.get_expr(value).span().end;
                    Arg { name: Some(name), value, span: Span::new(self.module.file, arg_start, end) }
                } else {
                    let value = self.parse_expr();
                    let end   = self.module.arena.get_expr(value).span().end;
                    Arg { name: None, value, span: Span::new(self.module.file, arg_start, end) }
                }
            } else {
                let value = self.parse_expr();
                let end   = self.module.arena.get_expr(value).span().end;
                Arg { name: None, value, span: Span::new(self.module.file, arg_start, end) }
            };

            args.push(arg);
            if !self.eat(&TokenKind::Comma) { break; }
        }
        let end = self.current_span().end;
        self.expect_tok(&TokenKind::RParen);
        self.module.arena.alloc_expr(Expr::Call {
            callee, args,
            span: Span::new(self.module.file, start, end),
        })
    }

    // ── String interpolation ──────────────────────────────────────────────────

    pub(crate) fn parse_string_interp(&mut self, raw: String, span: Span) -> ExprId {
        if !raw.contains('{') {
            return self.module.arena.alloc_expr(Expr::StrLit { value: raw, span });
        }
        let mut parts = vec![];
        let mut rest  = raw.as_str();

        while let Some(brace) = rest.find('{') {
            if brace > 0 {
                parts.push(InterpPart::Literal(rest[..brace].to_string()));
            }
            rest = &rest[brace + 1..];
            if let Some(close) = rest.find('}') {
                let inner = rest[..close].trim();
                rest = &rest[close + 1..];
                let expr  = self.parse_interp_fragment(inner, span);
                parts.push(InterpPart::Expr(expr));
            } else {
                parts.push(InterpPart::Literal(rest.to_string()));
                break;
            }
        }
        if !rest.is_empty() {
            parts.push(InterpPart::Literal(rest.to_string()));
        }
        // Degenerate: only one literal part after stripping braces
        if parts.len() == 1 {
            if let InterpPart::Literal(s) = &parts[0] {
                return self.module.arena.alloc_expr(Expr::StrLit { value: s.clone(), span });
            }
        }
        self.module.arena.alloc_expr(Expr::StringInterp { parts, span })
    }

    /// Parse a simple `a.b.c` member-access chain from a raw string slice.
    /// Full re-lexing is deferred to a later phase; this handles all cases in test.fdn.
    fn parse_interp_fragment(&mut self, inner: &str, span: Span) -> ExprId {
        let segments: Vec<&str> = inner.split('.').map(str::trim).collect();
        if segments.is_empty() || segments[0].is_empty() {
            return self.error_expr(span);
        }
        let first_str = segments[0];
        let first_sym = self.interner.intern(first_str);
        let mut expr  = match first_str {
            "this"   => self.module.arena.alloc_expr(Expr::This   { span }),
            "parent" => self.module.arena.alloc_expr(Expr::Parent { span }),
            _        => self.module.arena.alloc_expr(Expr::Ident { name: first_sym, span }),
        };
        for seg in &segments[1..] {
            let field = self.interner.intern(seg);
            expr = self.module.arena.alloc_expr(Expr::Field { object: expr, field, span });
        }
        expr
    }

    // ── Binding-power tables ──────────────────────────────────────────────────

    fn infix_bp(&self, kind: &TokenKind) -> Option<(u8, u8)> {
        Some(match kind {
            TokenKind::NullCoalesce                              => (1,  2),
            TokenKind::DotDot | TokenKind::DotDotDot             => (2,  3),  // range, lower than add
            TokenKind::Or                                        => (3,  4),
            TokenKind::And                                       => (5,  6),
            TokenKind::Is | TokenKind::Eq    | TokenKind::NotEq
            | TokenKind::Lt  | TokenKind::LtEq
            | TokenKind::Gt  | TokenKind::GtEq                  => (7,  8),
            TokenKind::Plus  | TokenKind::Minus                  => (9,  10),
            TokenKind::Star  | TokenKind::Slash | TokenKind::Percent => (11, 12),
            TokenKind::StarStar                                       => (13, 14), // right-assoc
            TokenKind::Caret                                          => (13, 13), // bitwise XOR
            TokenKind::Ampersand                                      => (11, 12), // bitwise AND (same tier as mul)
            TokenKind::Pipe                                           => ( 9, 10), // bitwise OR  (same tier as add)
            TokenKind::LtLt | TokenKind::GtGt                        => (11, 12), // shift
            // Postfix (call / member / index)
            TokenKind::LParen | TokenKind::Dot | TokenKind::LBracket => (17, 18),
            _ => return None,
        })
    }

    fn tok_to_binop(kind: &TokenKind) -> Option<BinOp> {
        Some(match kind {
            TokenKind::Plus    => BinOp::Add,
            TokenKind::Minus   => BinOp::Sub,
            TokenKind::Star    => BinOp::Mul,
            TokenKind::Slash   => BinOp::Div,
            TokenKind::Percent => BinOp::Rem,
            TokenKind::StarStar => BinOp::Pow,
            TokenKind::Caret     => BinOp::BitXor,
            TokenKind::Ampersand => BinOp::BitAnd,
            TokenKind::Pipe      => BinOp::BitOr,
            TokenKind::LtLt      => BinOp::Shl,
            TokenKind::GtGt      => BinOp::Shr,
            TokenKind::Eq        => BinOp::Eq,
            TokenKind::Is      => BinOp::Eq,
            TokenKind::NotEq   => BinOp::NotEq,
            TokenKind::Lt      => BinOp::Lt,
            TokenKind::LtEq    => BinOp::LtEq,
            TokenKind::Gt      => BinOp::Gt,
            TokenKind::GtEq    => BinOp::GtEq,
            TokenKind::And     => BinOp::And,
            TokenKind::Or      => BinOp::Or,
            TokenKind::DotDot    => BinOp::Range,
            TokenKind::DotDotDot  => BinOp::RangeInclusive,
            _ => return None,
        })
    }

    fn tok_to_binop_cmp(kind: &TokenKind) -> Option<BinOp> {
        Self::tok_to_binop(kind)
    }
}
