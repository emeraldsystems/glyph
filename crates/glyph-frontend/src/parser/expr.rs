use super::*;

impl<'a> Parser<'a> {
    pub(super) fn parse_expr(&mut self) -> Option<Expr> {
        self.parse_closure_expr()
    }

    /// Parse an arrow closure. Arrow is the lowest-precedence expression
    /// operator and is right-associative, so `x -> y -> x` nests in the body.
    pub(super) fn parse_closure_expr(&mut self) -> Option<Expr> {
        if self.at(TokenKind::Arrow) {
            let arrow = self.advance().unwrap();
            self.diagnostics.push(Diagnostic::error(
                "zero-parameter closure requires `()` before `->`",
                Some(arrow.span),
            ));
            if !self.at(TokenKind::Eof)
                && !self.at(TokenKind::Semicolon)
                && !self.at(TokenKind::RBrace)
            {
                let _ = self.parse_expr();
            }
            return None;
        }

        let explicit_move = self.at(TokenKind::Move);
        if !explicit_move && !self.at_closure_header() {
            return self.parse_match_expr();
        }

        let move_tok = if explicit_move {
            Some(self.advance().unwrap())
        } else {
            None
        };
        let start = move_tok
            .map(|tok| tok.span.start)
            .or_else(|| self.peek().map(|tok| tok.span.start))?;

        let params = if self.at(TokenKind::Ident) {
            let name_tok = self.advance().unwrap();
            let ty = if self.at(TokenKind::Colon) {
                self.advance();
                Some(self.parse_type_expr()?)
            } else {
                None
            };
            let end = ty
                .as_ref()
                .map(|ty| ty.span().end)
                .unwrap_or(name_tok.span.end);
            vec![Param {
                name: Ident(self.slice(name_tok)),
                ty,
                span: Span::new(name_tok.span.start, end),
            }]
        } else if self.at(TokenKind::LParen) {
            self.parse_closure_params()?
        } else {
            self.diagnostics.push(Diagnostic::error(
                "expected closure parameter or parenthesized parameter list",
                self.peek().map(|tok| tok.span),
            ));
            return None;
        };

        self.consume(TokenKind::Arrow, "expected `->` after closure parameters")?;
        if self.at(TokenKind::Eof)
            || self.at(TokenKind::Comma)
            || self.at(TokenKind::Semicolon)
            || self.at(TokenKind::RParen)
            || self.at(TokenKind::RBrace)
        {
            self.diagnostics.push(Diagnostic::error(
                "expected expression or block after closure `->`",
                self.peek().map(|tok| tok.span),
            ));
            return None;
        }
        let body = self.parse_expr()?;
        let span = Span::new(start, self.expr_end(&body));
        Some(Expr::Closure {
            capture: if explicit_move {
                CaptureMode::Move
            } else {
                CaptureMode::Inferred
            },
            params,
            body: Box::new(body),
            span,
        })
    }

    /// Detect a closure header without speculatively emitting diagnostics.
    /// Any balanced `( ... )` immediately followed by `->` is a closure
    /// attempt; invalid contents then receive a parameter-specific diagnostic.
    pub(super) fn at_closure_header(&self) -> bool {
        if self.at(TokenKind::Ident) {
            if self
                .tokens
                .get(self.pos + 1)
                .is_some_and(|tok| tok.kind == TokenKind::Arrow)
            {
                return true;
            }
            if !self
                .tokens
                .get(self.pos + 1)
                .is_some_and(|tok| tok.kind == TokenKind::Colon)
            {
                return false;
            }

            let mut paren_depth = 0usize;
            let mut bracket_depth = 0usize;
            let mut angle_depth = 0usize;
            for tok in self.tokens.iter().skip(self.pos + 2) {
                match tok.kind {
                    TokenKind::LParen => paren_depth += 1,
                    TokenKind::RParen if paren_depth > 0 => paren_depth -= 1,
                    TokenKind::LBracket => bracket_depth += 1,
                    TokenKind::RBracket if bracket_depth > 0 => bracket_depth -= 1,
                    TokenKind::Lt => angle_depth += 1,
                    TokenKind::Gt if angle_depth > 0 => angle_depth -= 1,
                    TokenKind::Arrow
                        if paren_depth == 0 && bracket_depth == 0 && angle_depth == 0 =>
                    {
                        return true;
                    }
                    TokenKind::Comma
                    | TokenKind::Semicolon
                    | TokenKind::RBrace
                    | TokenKind::Eof
                        if paren_depth == 0 && bracket_depth == 0 && angle_depth == 0 =>
                    {
                        return false;
                    }
                    _ => {}
                }
            }
            return false;
        }
        if !self.at(TokenKind::LParen) {
            return false;
        }

        let mut depth = 0usize;
        for (index, tok) in self.tokens.iter().enumerate().skip(self.pos) {
            match tok.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return self
                            .tokens
                            .get(index + 1)
                            .is_some_and(|next| next.kind == TokenKind::Arrow);
                    }
                }
                TokenKind::Eof => return false,
                _ => {}
            }
        }
        false
    }

    pub(super) fn parse_closure_params(&mut self) -> Option<Vec<Param>> {
        self.consume(TokenKind::LParen, "expected `(` before closure parameters")?;
        let mut params = Vec::new();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let name_tok = self.consume(
                TokenKind::Ident,
                "expected parameter name in closure parameter list",
            )?;
            let ty = if self.at(TokenKind::Colon) {
                self.advance();
                Some(self.parse_type_expr()?)
            } else {
                None
            };
            let end = ty
                .as_ref()
                .map(|ty| ty.span().end)
                .unwrap_or(name_tok.span.end);
            params.push(Param {
                name: Ident(self.slice(name_tok)),
                ty,
                span: Span::new(name_tok.span.start, end),
            });

            if self.at(TokenKind::Comma) {
                self.advance();
                if self.at(TokenKind::RParen) {
                    break;
                }
            } else if !self.at(TokenKind::RParen) {
                self.diagnostics.push(Diagnostic::error(
                    "expected `,` or `)` after closure parameter",
                    self.peek().map(|tok| tok.span),
                ));
                return None;
            }
        }
        self.consume(TokenKind::RParen, "expected `)` after closure parameters")?;
        Some(params)
    }

    pub(super) fn parse_match_expr(&mut self) -> Option<Expr> {
        if self.at(TokenKind::Match) {
            let match_tok = self.advance().unwrap();
            let prev = self.suppress_struct_literals;
            self.suppress_struct_literals = true;
            let scrutinee = self.parse_binary_expr();
            self.suppress_struct_literals = prev;
            let scrutinee = scrutinee?;
            self.consume(TokenKind::LBrace, "expected `{` after match scrutinee")?;

            let mut arms = Vec::new();
            while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                let pattern = self.parse_match_pattern()?;
                if self.at(TokenKind::FatArrow) {
                    self.advance();
                } else if self.at(TokenKind::Eq) {
                    // Be tolerant if lexer produced '=' followed by '>'
                    self.advance();
                    self.consume(TokenKind::Gt, "expected `>` after `=` in match arm")?;
                } else {
                    self.consume(TokenKind::FatArrow, "expected `=>` after match pattern")?;
                }
                let expr = self.parse_expr()?;
                let mut end = self.expr_end(&expr);
                if self.at(TokenKind::Comma) {
                    let comma = self.advance().unwrap();
                    end = comma.span.end;
                }
                arms.push(MatchArm {
                    pattern,
                    span: Span::new(self.expr_start(&expr), end),
                    expr,
                });
            }

            let end_tok = self.consume(TokenKind::RBrace, "expected `}` to close match")?;
            return Some(Expr::Match {
                scrutinee: Box::new(scrutinee),
                arms,
                span: Span::new(match_tok.span.start, end_tok.span.end),
            });
        }

        self.parse_binary_expr()
    }

    pub(super) fn parse_match_pattern(&mut self) -> Option<MatchPattern> {
        if self.at(TokenKind::Ident) {
            let tok = self.peek().unwrap();
            if self.slice(tok) == "_" {
                self.advance();
                return Some(MatchPattern::Wildcard);
            }
        }

        let variant_tok = self.consume(TokenKind::Ident, "expected variant name")?;
        let mut name = Ident(self.slice(variant_tok));
        let mut qualifier = None;
        while self.at(TokenKind::ColonColon) {
            self.advance();
            let seg_tok = self.consume(
                TokenKind::Ident,
                "expected identifier after `::` in match pattern",
            )?;
            // Preserve the segment immediately preceding the final one as the
            // pattern's qualifier (the enum name in `Enum::Variant`), rather
            // than silently discarding it (GLYPH-13).
            qualifier = Some(name);
            name = Ident(self.slice(seg_tok));
        }
        let mut binding = None;
        if self.at(TokenKind::LParen) {
            self.advance();
            if let Some(b) = self.consume(TokenKind::Ident, "expected binding name") {
                binding = Some(Ident(self.slice(b)));
            }
            self.consume(TokenKind::RParen, "expected `)` after binding")?;
        }

        Some(MatchPattern::Variant {
            qualifier,
            name,
            binding,
        })
    }

    pub(super) fn parse_binary_expr(&mut self) -> Option<Expr> {
        self.parse_binary_expr_prec(0)
    }

    pub(super) fn parse_binary_expr_prec(&mut self, min_prec: u8) -> Option<Expr> {
        let mut lhs = self.parse_call_or_primary()?;

        // `expr as Type` binds tighter than any binary operator.
        while self.at(TokenKind::As) {
            self.advance();
            let target = self.parse_type_expr()?;
            let span = Span::new(self.expr_start(&lhs), target.span().end);
            lhs = Expr::Cast {
                expr: Box::new(lhs),
                target,
                span,
            };
        }

        loop {
            let Some(op) = self.peek_binary_op() else {
                break;
            };
            let prec = self.binary_precedence(&op);
            if prec < min_prec {
                break;
            }

            self.advance();
            // All binary operators are left-associative.
            let rhs = self.parse_binary_expr_prec(prec.saturating_add(1))?;

            let span = Span::new(self.expr_start(&lhs), self.expr_end(&rhs));
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }

        Some(lhs)
    }

    pub(super) fn binary_precedence(&self, op: &BinaryOp) -> u8 {
        match op {
            // Lowest precedence
            BinaryOp::Or => 1,
            BinaryOp::And => 2,
            BinaryOp::Eq | BinaryOp::Ne => 3,
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => 4,
            BinaryOp::Add | BinaryOp::Sub => 5,
            BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => 6,
        }
    }

    pub(super) fn parse_call_or_primary(&mut self) -> Option<Expr> {
        if self.at(TokenKind::Match) {
            return self.parse_match_expr();
        }
        // Unary not prefix (!expr)
        if self.at(TokenKind::Bang) {
            let start_tok = self.peek().unwrap();
            let start = start_tok.span.start;
            self.advance();
            let expr = self.parse_call_or_primary()?;
            let span = Span::new(start, self.expr_end(&expr));
            return Some(Expr::Unary {
                op: UnaryOp::Not,
                expr: Box::new(expr),
                span,
            });
        }
        // Unary negation prefix (-expr)
        if self.at(TokenKind::Minus) {
            let start_tok = self.peek().unwrap();
            let start = start_tok.span.start;
            self.advance();
            let expr = self.parse_call_or_primary()?;
            let span = Span::new(start, self.expr_end(&expr));
            return Some(Expr::Unary {
                op: UnaryOp::Neg,
                expr: Box::new(expr),
                span,
            });
        }
        // Check for reference prefix (&expr or &mut expr)
        if self.at(TokenKind::Amp) {
            let start_tok = self.peek().unwrap();
            let start = start_tok.span.start;
            self.advance();

            // Check for mut keyword
            let mutability = if self.at(TokenKind::Mut) {
                self.advance();
                glyph_core::types::Mutability::Mutable
            } else {
                glyph_core::types::Mutability::Immutable
            };

            // Parse the expression being referenced
            let expr = self.parse_call_or_primary()?;
            let span = Span::new(start, self.expr_end(&expr));

            return Some(Expr::Ref {
                expr: Box::new(expr),
                mutability,
                span,
            });
        }

        let mut expr = self.parse_primary()?;
        loop {
            if self.at(TokenKind::LParen) {
                let start = self.expr_start(&expr);
                self.advance();
                let mut args = Vec::new();
                while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                    if let Some(arg) = self.parse_expr() {
                        args.push(arg);
                    }
                    if self.at(TokenKind::Comma) {
                        self.advance();
                    } else {
                        break;
                    }
                }
                let end =
                    if let Some(rp) = self.consume(TokenKind::RParen, "expected `)` after args") {
                        rp.span.end
                    } else {
                        self.expr_end(args.last().unwrap_or(&expr))
                    };
                expr = Expr::Call {
                    callee: Box::new(expr),
                    args,
                    span: Span::new(start, end),
                };
                continue;
            } else if self.at(TokenKind::Dot) {
                // Field access or method call - need lookahead
                self.advance(); // consume dot

                // Check for numeric index (tuple element access like .0, .1)
                let (name_tok, name) = if self.at(TokenKind::Int) {
                    let idx_tok = self.advance().unwrap();
                    let idx_str = self.slice(idx_tok);
                    (idx_tok, Ident(idx_str.to_string()))
                } else {
                    let ident_tok =
                        self.consume(TokenKind::Ident, "expected field or method name after `.`")?;
                    (ident_tok, Ident(self.slice(ident_tok)))
                };

                // Lookahead: is there a `(` after the identifier?
                if self.at(TokenKind::LParen) {
                    // METHOD CALL: obj.method(args)
                    self.advance(); // consume (

                    let mut args = Vec::new();
                    while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                        if let Some(arg) = self.parse_expr() {
                            args.push(arg);
                        }
                        if self.at(TokenKind::Comma) {
                            self.advance();
                        } else {
                            break;
                        }
                    }

                    let end = if let Some(rp) =
                        self.consume(TokenKind::RParen, "expected `)` after method arguments")
                    {
                        rp.span.end
                    } else {
                        args.last()
                            .map(|a| self.expr_end(a))
                            .unwrap_or(name_tok.span.end)
                    };

                    let start = self.expr_start(&expr);
                    expr = Expr::MethodCall {
                        receiver: Box::new(expr),
                        method: name,
                        args,
                        span: Span::new(start, end),
                    };
                } else {
                    // FIELD ACCESS: obj.field
                    let span = Span::new(self.expr_start(&expr), name_tok.span.end);
                    expr = Expr::FieldAccess {
                        base: Box::new(expr),
                        field: name,
                        span,
                    };
                }
                continue;
            } else if self.at(TokenKind::LBracket) {
                // Array indexing
                self.advance(); // consume [
                let index_expr = self.parse_expr()?;
                let end = if let Some(rb) = self.consume(TokenKind::RBracket, "expected `]`") {
                    rb.span.end
                } else {
                    self.expr_end(&index_expr)
                };
                let span = Span::new(self.expr_start(&expr), end);
                expr = Expr::Index {
                    base: Box::new(expr),
                    index: Box::new(index_expr),
                    span,
                };
                continue;
            } else if self.at(TokenKind::Question) {
                // Try operator: expr?
                let q_tok = self.advance().unwrap();
                let span = Span::new(self.expr_start(&expr), q_tok.span.end);
                expr = Expr::Try {
                    expr: Box::new(expr),
                    span,
                };
                continue;
            }
            break;
        }
        Some(expr)
    }

    pub(super) fn parse_primary(&mut self) -> Option<Expr> {
        let tok = self.advance()?;
        match tok.kind {
            TokenKind::Ident => {
                // Parse full path (supports `module::Type`)
                let ident_expr = self.parse_path_ident(&tok);
                if !self.suppress_struct_literals && self.at(TokenKind::LBrace) {
                    if let Expr::Ident(name, span) = ident_expr {
                        self.parse_struct_lit(name, span.start)
                    } else {
                        None
                    }
                } else {
                    Some(ident_expr)
                }
            }
            TokenKind::Int => {
                let text = self.slice(tok);
                if text.parse::<i64>().is_err() && text.parse::<u64>().is_err() {
                    self.diagnostics.push(Diagnostic::error(
                        format!("integer literal '{}' is out of range (max u64)", text),
                        Some(tok.span),
                    ));
                }
                Some(Expr::Lit(self.literal_from(tok), tok.span))
            }
            TokenKind::Float => Some(Expr::Lit(self.literal_from(tok), tok.span)),
            TokenKind::Str => Some(Expr::Lit(self.literal_from(tok), tok.span)),
            TokenKind::Char => Some(Expr::Lit(self.literal_from(tok), tok.span)),
            TokenKind::InterpStr => self.parse_interpolated_string(tok),
            TokenKind::Bool => Some(Expr::Lit(self.literal_from(tok), tok.span)),
            TokenKind::LParen => {
                let start = tok.span.start;

                // Empty tuple: ()
                if self.at(TokenKind::RParen) {
                    let rp = self.advance().unwrap();
                    return Some(Expr::Tuple {
                        elements: vec![],
                        span: Span::new(start, rp.span.end),
                    });
                }

                let first = self.parse_expr()?;

                // Check if it's a tuple (has comma) or just parenthesized
                if self.at(TokenKind::Comma) {
                    let mut elements = vec![first];
                    while self.at(TokenKind::Comma) {
                        self.advance();
                        if self.at(TokenKind::RParen) {
                            break;
                        }
                        elements.push(self.parse_expr()?);
                    }
                    let rp = self.consume(TokenKind::RParen, "expected `)`")?;
                    Some(Expr::Tuple {
                        elements,
                        span: Span::new(start, rp.span.end),
                    })
                } else {
                    // Parenthesized expression
                    self.consume(TokenKind::RParen, "expected `)`")?;
                    Some(first)
                }
            }
            TokenKind::If => self.parse_if(tok.span.start),
            TokenKind::While => self.parse_while(tok.span.start),
            TokenKind::For => self.parse_for(tok.span.start),
            TokenKind::LBracket => {
                let start = tok.span.start;
                let mut elements = Vec::new();

                while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
                    if let Some(elem) = self.parse_expr() {
                        elements.push(elem);
                    } else {
                        break;
                    }

                    if self.at(TokenKind::Comma) {
                        self.advance();
                    } else {
                        break;
                    }
                }

                let end = if let Some(rb) = self.consume(TokenKind::RBracket, "expected `]`") {
                    rb.span.end
                } else {
                    tok.span.end
                };

                Some(Expr::ArrayLit {
                    elements,
                    span: Span::new(start, end),
                })
            }
            TokenKind::LBrace => {
                self.pos -= 1;
                self.parse_block().map(Expr::Block)
            }
            _ => {
                self.diagnostics.push(Diagnostic::error(
                    "unexpected token in expression",
                    Some(tok.span),
                ));
                None
            }
        }
    }

    pub(super) fn parse_interpolated_string(&mut self, tok: &Token) -> Option<Expr> {
        let raw = self.slice(tok);
        if raw.len() < 3 {
            self.diagnostics.push(Diagnostic::error(
                "invalid interpolated string",
                Some(tok.span),
            ));
            return None;
        }

        let mut segments = Vec::new();
        let content = &raw[2..raw.len().saturating_sub(1)]; // strip $" and closing "
        let bytes = content.as_bytes();
        let mut i = 0usize;
        let mut literal: Vec<u8> = Vec::new();

        while i < bytes.len() {
            if bytes[i] == b'{' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                    literal.push(b'{');
                    i += 2;
                    continue;
                }

                // Flush current literal segment
                if !literal.is_empty() {
                    let s = String::from_utf8_lossy(&literal).into_owned();
                    literal.clear();
                    segments.push(InterpSegment::Literal(s));
                }

                let hole_start = i;
                i += 1;
                let mut depth = 1;
                while i < bytes.len() && depth > 0 {
                    match bytes[i] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }

                if depth != 0 {
                    self.diagnostics.push(Diagnostic::error(
                        "unterminated interpolation hole",
                        Some(tok.span),
                    ));
                    return None;
                }

                let hole_end = i;
                let hole_src = content[hole_start + 1..hole_end].trim();
                let offset = tok.span.start + 2 + hole_start as u32;
                if let Some(expr) = self.parse_inline_expr(hole_src, offset) {
                    segments.push(InterpSegment::Expr(expr));
                }
                i += 1; // skip closing brace
                continue;
            }

            if bytes[i] == b'}' && i + 1 < bytes.len() && bytes[i + 1] == b'}' {
                literal.push(b'}');
                i += 2;
                continue;
            }

            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                if bytes[i + 1] == b'x' && i + 3 < bytes.len() {
                    let d1 = bytes[i + 2] as char;
                    let d2 = bytes[i + 3] as char;
                    let hex_str: String = [d1, d2].iter().collect();
                    if let Ok(byte) = u8::from_str_radix(&hex_str, 16) {
                        literal.push(byte);
                        i += 4;
                        continue;
                    }
                }
                let escaped = match bytes[i + 1] {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'r' => b'\r',
                    b'\\' => b'\\',
                    b'"' => b'"',
                    other => other,
                };
                literal.push(escaped);
                i += 2;
                continue;
            }

            literal.push(bytes[i]);
            i += 1;
        }

        if !literal.is_empty() {
            segments.push(InterpSegment::Literal(
                String::from_utf8_lossy(&literal).into_owned(),
            ));
        }

        Some(Expr::InterpString {
            segments,
            span: tok.span,
        })
    }

    pub(super) fn parse_inline_expr(&mut self, src: &str, span_offset: u32) -> Option<Expr> {
        let lex_out = crate::lexer::lex(src);
        for diag in lex_out.diagnostics {
            let span = diag
                .span
                .map(|s| Span::new(s.start + span_offset, s.end + span_offset));
            self.diagnostics.push(Diagnostic::error(diag.message, span));
        }

        let mut parser = Parser {
            tokens: &lex_out.tokens,
            source: src,
            pos: 0,
            diagnostics: Vec::new(),
            items: Vec::new(),
            pending_items: Vec::new(),
            suppress_struct_literals: false,
        };

        let expr = parser.parse_expr();
        for diag in parser.diagnostics {
            let span = diag
                .span
                .map(|s| Span::new(s.start + span_offset, s.end + span_offset));
            self.diagnostics.push(Diagnostic::error(diag.message, span));
        }
        expr
    }

    pub(super) fn parse_path_ident(&mut self, first: &Token) -> Expr {
        let mut name = self.slice(first).to_string();
        let mut end = first.span.end;
        while self.at(TokenKind::ColonColon) {
            self.advance();
            if let Some(seg) = self.consume(TokenKind::Ident, "expected identifier after `::`") {
                name.push_str("::");
                name.push_str(&self.slice(seg));
                end = seg.span.end;
            } else {
                break;
            }
        }
        Expr::Ident(Ident(name), Span::new(first.span.start, end))
    }

    pub(super) fn parse_struct_lit(&mut self, name: Ident, start: u32) -> Option<Expr> {
        self.consume(TokenKind::LBrace, "expected `{` for struct literal")?;

        let mut fields = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let field_name_tok = self.consume(TokenKind::Ident, "expected field name")?;
            self.consume(TokenKind::Colon, "expected `:` after field name")?;
            let value = self.parse_expr()?;

            fields.push((Ident(self.slice(field_name_tok)), value));

            if self.at(TokenKind::Comma) {
                self.advance();
            } else if !self.at(TokenKind::RBrace) {
                break;
            }
        }

        let end = self.consume(TokenKind::RBrace, "expected `}` to end struct literal")?;

        Some(Expr::StructLit {
            name,
            fields,
            span: Span::new(start, end.span.end),
        })
    }
}
