// fidan-parser — error recovery helpers
use crate::parser::{MAX_ERRORS, Parser};
use fidan_ast::{Expr, ExprId, Stmt, StmtId};
use fidan_diagnostics::Diagnostic;
use fidan_lexer::TokenKind;
use fidan_source::Span;

impl<'t> Parser<'t> {
    /// Emit a parse error diagnostic.
    ///
    /// If the parser is already in recovery mode (a previous error was just
    /// emitted and `synchronize()` has not yet fired), the error is silently
    /// suppressed to prevent cascade diagnostics.  `synchronize()` clears the
    /// flag so the next genuine error is always reported.
    ///
    /// Once `MAX_ERRORS` distinct errors have been accumulated the `bail` flag
    /// is set, which causes all major parsing loops to break out immediately.
    pub(crate) fn error(&mut self, message: &str, span: Span) {
        if self.recovering {
            return;
        }
        self.recovering = true;
        self.diags.push(Diagnostic::error(
            fidan_diagnostics::diag_code!("E0000"),
            message,
            span,
        ));
        if self.diags.len() >= MAX_ERRORS {
            self.bail = true;
        }
    }

    /// Allocate an `Expr::Error` placeholder.
    pub(crate) fn error_expr(&mut self, span: Span) -> ExprId {
        self.module.arena.alloc_expr(Expr::Error { span })
    }

    /// Allocate a `Stmt::Error` placeholder.
    pub(crate) fn error_stmt(&mut self, span: Span) -> StmtId {
        self.module.arena.alloc_stmt(Stmt::Error { span })
    }

    /// Skip tokens until a synchronisation point is found.
    ///
    /// Synchronisation points: `}`, statement-starting keywords, `Eof`.
    /// Allows parsing to continue after an error and collect more diagnostics
    /// in a single pass.  Always resets the `recovering` flag so subsequent
    /// genuine errors are reported cleanly.
    ///
    /// **Progress guarantee**: if the very first token was already a sync-point
    /// keyword (other than `}` and `Eof`, which must be left for the parent
    /// block to consume), one extra token is consumed so callers never spin in
    /// place on the same token.
    pub(crate) fn synchronize(&mut self) {
        let pos_before = self.pos;
        loop {
            match self.peek() {
                TokenKind::Eof | TokenKind::RBrace => break,
                TokenKind::Var
                | TokenKind::Action
                | TokenKind::Object
                | TokenKind::If
                | TokenKind::For
                | TokenKind::While
                | TokenKind::Attempt
                | TokenKind::Return
                | TokenKind::Break
                | TokenKind::Continue
                | TokenKind::Panic => {
                    // If we haven't moved yet, consume this keyword too — otherwise the
                    // outer loop would see the same keyword, call error(), call
                    // synchronize() again and spin forever.
                    if self.pos == pos_before {
                        self.advance();
                    }
                    break;
                }
                TokenKind::Newline | TokenKind::Semicolon => {
                    self.advance();
                    break;
                }
                _ => {
                    self.advance();
                }
            }
        }
        self.recovering = false; // re-open error reporting after each sync
    }
}
