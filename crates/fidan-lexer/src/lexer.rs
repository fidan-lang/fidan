use crate::{
    interner::SymbolInterner,
    synonyms::lookup_keyword,
    token::{Token, TokenKind},
};
use fidan_source::{FileId, SourceFile, Span};
use std::sync::Arc;

/// The Fidan lexer.
///
/// Consumes a `SourceFile` and produces a flat `Vec<Token>`.
/// All synonym resolution happens here; the parser only sees canonical tokens.
pub struct Lexer<'src> {
    src: &'src str,
    file_id: FileId,
    pos: u32, // current byte position (always on a char boundary)
    interner: Arc<SymbolInterner>,
    /// The last non-whitespace token kind emitted (needed for auto-newline insertion).
    last_kind: Option<TokenKind>,
}

impl<'src> Lexer<'src> {
    pub fn new(file: &'src SourceFile, interner: Arc<SymbolInterner>) -> Self {
        Self {
            src: &file.src,
            file_id: file.id,
            pos: 0,
            interner,
            last_kind: None,
        }
    }

    /// Tokenise the entire source and return a `Vec<Token>`.
    /// The last token is always `Eof`.
    pub fn tokenise(mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token();
            let is_eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }
        tokens
    }

    // ── Core advancement ─────────────────────────────────────────────────────

    fn peek(&self) -> Option<char> {
        self.src[self.pos as usize..].chars().next()
    }

    fn peek2(&self) -> Option<char> {
        let mut chars = self.src[self.pos as usize..].chars();
        chars.next();
        chars.next()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8() as u32;
        Some(c)
    }

    fn eat_if(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.pos += expected.len_utf8() as u32;
            true
        } else {
            false
        }
    }

    fn span_from(&self, start: u32) -> Span {
        Span::new(self.file_id, start, self.pos)
    }

    // ── Whitespace / comments ────────────────────────────────────────────────

    /// Skip whitespace (but NOT newlines — those may need to become Newline tokens).
    /// Returns `true` if a newline was encountered while skipping.
    fn skip_non_newline_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' || !c.is_whitespace() {
                break;
            }
            self.advance();
        }
    }

    fn skip_line_comment(&mut self) {
        // Already consumed `#`; skip to end of line.
        while let Some(c) = self.advance() {
            if c == '\n' {
                // Put the newline back for auto-insertion logic.
                self.pos -= 1;
                break;
            }
        }
    }

    fn skip_block_comment(&mut self) {
        // Already consumed `#/`; track nesting depth.
        let mut depth: u32 = 1;
        while let Some(c) = self.advance() {
            if c == '#' && self.peek() == Some('/') {
                self.advance();
                depth += 1;
            } else if c == '/' && self.peek() == Some('#') {
                self.advance();
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
        }
    }

    // ── Token lexing ─────────────────────────────────────────────────────────

    fn next_token(&mut self) -> Token {
        loop {
            self.skip_non_newline_whitespace();

            let start = self.pos;

            match self.peek() {
                // ── End of file ──────────────────────────────────────────
                None => {
                    // Auto-insert newline before Eof if needed.
                    if self
                        .last_kind
                        .as_ref()
                        .map_or(false, |k| k.terminates_statement())
                    {
                        let span = self.span_from(start);
                        self.last_kind = Some(TokenKind::Newline);
                        return Token::new(TokenKind::Newline, span);
                    }
                    return Token::new(TokenKind::Eof, self.span_from(start));
                }

                // ── Newline ───────────────────────────────────────────────
                Some('\n') => {
                    self.advance();
                    let span = self.span_from(start);
                    // Emit a `Newline` token only if the last real token warrants it.
                    if self
                        .last_kind
                        .as_ref()
                        .map_or(false, |k| k.terminates_statement())
                    {
                        self.last_kind = Some(TokenKind::Newline);
                        return Token::new(TokenKind::Newline, span);
                    }
                    // Otherwise swallow the newline and keep going.
                    continue;
                }

                // ── Comments ─────────────────────────────────────────────
                Some('#') => {
                    self.advance(); // consume `#`
                    if self.peek() == Some('/') {
                        self.advance(); // consume `/`
                        self.skip_block_comment();
                    } else {
                        self.skip_line_comment();
                    }
                    continue;
                }

                // ── String literals ───────────────────────────────────────
                Some('"') => return self.lex_string(start),

                // ── Numbers ───────────────────────────────────────────────
                Some(c) if c.is_ascii_digit() => return self.lex_number(start),

                // ── Identifiers / keywords ────────────────────────────────
                Some(c) if c.is_alphabetic() || c == '_' => return self.lex_ident(start),

                // ── Operators & punctuation ───────────────────────────────
                Some(_) => return self.lex_punct(start),
            }
        }
    }

    fn lex_string(&mut self, start: u32) -> Token {
        self.advance(); // opening `"`
        let mut s = String::new();
        loop {
            match self.advance() {
                None | Some('"') => break,
                Some('\\') => {
                    // Basic escape sequences
                    match self.advance() {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some('r') => s.push('\r'),
                        Some('"') => s.push('"'),
                        Some('\\') => s.push('\\'),
                        Some('{') => s.push('{'),
                        Some(c) => {
                            s.push('\\');
                            s.push(c);
                        }
                        None => break,
                    }
                }
                Some(c) => s.push(c),
            }
        }
        let kind = TokenKind::LitString(s);
        self.emit(kind, start)
    }

    fn lex_number(&mut self, start: u32) -> Token {
        while self
            .peek()
            .map_or(false, |c| c.is_ascii_digit() || c == '_')
        {
            self.advance();
        }
        let is_float =
            self.peek() == Some('.') && self.peek2().map_or(false, |c| c.is_ascii_digit());
        if is_float {
            self.advance(); // `.`
            while self
                .peek()
                .map_or(false, |c| c.is_ascii_digit() || c == '_')
            {
                self.advance();
            }
            // Optional exponent: e/E [+-] digits
            if matches!(self.peek(), Some('e') | Some('E')) {
                self.advance();
                if matches!(self.peek(), Some('+') | Some('-')) {
                    self.advance();
                }
                while self.peek().map_or(false, |c| c.is_ascii_digit()) {
                    self.advance();
                }
            }
            let raw = &self.src[start as usize..self.pos as usize];
            let val: f64 = raw.replace('_', "").parse().unwrap_or(0.0);
            return self.emit(TokenKind::LitFloat(val), start);
        }
        let raw = &self.src[start as usize..self.pos as usize];
        let val: i64 = raw.replace('_', "").parse().unwrap_or(0);
        self.emit(TokenKind::LitInteger(val), start)
    }

    fn lex_ident(&mut self, start: u32) -> Token {
        while self
            .peek()
            .map_or(false, |c| c.is_alphanumeric() || c == '_')
        {
            self.advance();
        }
        let word = &self.src[start as usize..self.pos as usize];

        let kind = if let Some(kw) = lookup_keyword(word) {
            kw
        } else {
            let sym = self.interner.intern(word);
            TokenKind::Ident(sym)
        };
        self.emit(kind, start)
    }

    fn lex_punct(&mut self, start: u32) -> Token {
        let c = self.advance().unwrap();
        let kind = match c {
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            '{' => TokenKind::LBrace,
            '}' => TokenKind::RBrace,
            '[' => TokenKind::LBracket,
            ']' => TokenKind::RBracket,
            ',' => TokenKind::Comma,
            '.' => {
                if self.eat_if('.') {
                    TokenKind::DotDot
                } else {
                    TokenKind::Dot
                }
            }
            ':' => {
                if self.eat_if(':') {
                    TokenKind::DoubleColon
                } else {
                    TokenKind::Colon
                }
            }
            ';' => TokenKind::Semicolon,
            '@' => TokenKind::At,
            '?' => {
                if self.eat_if('?') {
                    TokenKind::NullCoalesce
                } else {
                    TokenKind::Unknown('?')
                }
            }
            '+' => {
                if self.eat_if('=') {
                    TokenKind::PlusEq
                } else {
                    TokenKind::Plus
                }
            }
            '-' => {
                if self.eat_if('>') {
                    TokenKind::Arrow
                } else if self.eat_if('=') {
                    TokenKind::MinusEq
                } else {
                    TokenKind::Minus
                }
            }
            '*' => {
                if self.eat_if('*') {
                    TokenKind::StarStar
                } else if self.eat_if('=') {
                    TokenKind::StarEq
                } else {
                    TokenKind::Star
                }
            }
            '/' => {
                if self.eat_if('=') {
                    TokenKind::SlashEq
                } else {
                    TokenKind::Slash
                }
            }
            '%' => {
                if self.eat_if('=') {
                    TokenKind::PercentEq
                } else {
                    TokenKind::Percent
                }
            }
            '^' => TokenKind::Caret,
            '=' => {
                if self.eat_if('>') {
                    TokenKind::FatArrow
                } else if self.eat_if('=') {
                    TokenKind::Eq
                } else {
                    TokenKind::Assign
                }
            }
            '!' => {
                if self.eat_if('=') {
                    TokenKind::NotEq
                } else {
                    TokenKind::Not
                }
            }
            '<' => {
                if self.eat_if('=') {
                    TokenKind::LtEq
                } else {
                    TokenKind::Lt
                }
            }
            '>' => {
                if self.eat_if('=') {
                    TokenKind::GtEq
                } else {
                    TokenKind::Gt
                }
            }
            other => TokenKind::Unknown(other),
        };
        self.emit(kind, start)
    }

    // ── Helper: emit a token and update `last_kind` ───────────────────────────

    fn emit(&mut self, kind: TokenKind, start: u32) -> Token {
        let span = self.span_from(start);
        // Track last non-newline, non-error kind for auto-newline insertion.
        if !matches!(kind, TokenKind::Newline | TokenKind::Unknown(_)) {
            self.last_kind = Some(kind.clone());
        }
        Token::new(kind, span)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_source::{FileId, SourceFile};

    fn lex(src: &str) -> Vec<TokenKind> {
        let file = SourceFile::new(FileId(0), "<test>", src);
        let interner = Arc::new(SymbolInterner::new());
        Lexer::new(&file, interner)
            .tokenise()
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn test_integers() {
        let tokens = lex("42 0 1_000");
        assert_eq!(tokens[0], TokenKind::LitInteger(42));
        assert_eq!(tokens[1], TokenKind::LitInteger(0));
        assert_eq!(tokens[2], TokenKind::LitInteger(1000));
    }

    #[test]
    fn test_float() {
        let tokens = lex("3.14");
        assert!(matches!(tokens[0], TokenKind::LitFloat(_)));
    }

    #[test]
    fn test_string() {
        let tokens = lex("\"hello world\"");
        assert_eq!(tokens[0], TokenKind::LitString("hello world".to_string()));
    }

    #[test]
    fn test_keywords() {
        let tokens = lex("var action object");
        assert_eq!(tokens[0], TokenKind::Var);
        assert_eq!(tokens[1], TokenKind::Action);
        assert_eq!(tokens[2], TokenKind::Object);
    }

    #[test]
    fn test_synonyms() {
        // Control-flow synonyms
        let tokens = lex("else stop");
        assert_eq!(tokens[0], TokenKind::Otherwise); // else = otherwise
        assert_eq!(tokens[1], TokenKind::Break); // stop = break

        // Inline separator synonym
        let tokens = lex("separate");
        assert_eq!(tokens[0], TokenKind::Semicolon); // separate = ;

        // Operator / call synonyms
        let tokens = lex("also try throw mod pow");
        assert_eq!(tokens[0], TokenKind::Comma); // also = ,
        assert_eq!(tokens[1], TokenKind::Attempt); // try = attempt
        assert_eq!(tokens[2], TokenKind::Panic); // throw = panic
        assert_eq!(tokens[3], TokenKind::Percent); // mod = %
        assert_eq!(tokens[4], TokenKind::StarStar); // pow = **
    }

    #[test]
    fn test_auto_newline() {
        // Newline after an identifier terminates the statement.
        let tokens = lex("x\ny");
        assert!(tokens.iter().any(|t| *t == TokenKind::Newline));
    }

    #[test]
    fn test_no_newline_after_operator() {
        // `+` is a continuation token; a newline after it must NOT terminate the statement.
        // Verify: no [Plus, Newline] pair exists (the Newline before Eof from `y` is fine).
        let tokens = lex("x +\ny");
        let newline_after_plus = tokens
            .windows(2)
            .any(|w| w[0] == TokenKind::Plus && w[1] == TokenKind::Newline);
        assert!(!newline_after_plus);
    }

    #[test]
    fn test_line_comment() {
        let tokens = lex("x # this is a comment\ny");
        // Should have x, Newline, y, Eof — comment is gone.
        assert!(tokens.iter().any(|t| *t == TokenKind::Newline));
        assert!(!tokens.iter().any(|t| matches!(t, TokenKind::Unknown(_))));
    }

    #[test]
    fn test_null_coalesce() {
        let tokens = lex("x ?? y");
        assert_eq!(tokens[1], TokenKind::NullCoalesce);
    }

    #[test]
    fn test_nothing() {
        let tokens = lex("nothing");
        assert_eq!(tokens[0], TokenKind::Nothing);
    }
}
