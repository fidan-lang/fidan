// fidan-parser — error recovery helpers
use crate::parser::Parser;
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
    pub(crate) fn error(&mut self, message: &str, span: Span) {
        if self.recovering {
            return;
        }
        self.recovering = true;
        self.diags.push(Diagnostic::error("P000", message, span));
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
    pub(crate) fn synchronize(&mut self) {
        loop {
            match self.peek() {
                TokenKind::Eof
                | TokenKind::RBrace
                | TokenKind::Var
                | TokenKind::Action
                | TokenKind::Object
                | TokenKind::If
                | TokenKind::For
                | TokenKind::While
                | TokenKind::Attempt
                | TokenKind::Return
                | TokenKind::Break
                | TokenKind::Continue
                | TokenKind::Panic => break,
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
