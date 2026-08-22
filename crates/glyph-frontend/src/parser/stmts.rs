use super::*;

impl<'a> Parser<'a> {
    pub(super) fn parse_block(&mut self) -> Option<Block> {
        let lb = self.consume(TokenKind::LBrace, "expected `{` to start block")?;
        let start = lb.span.start;
        let mut stmts = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            if let Some(stmt) = self.parse_stmt() {
                stmts.push(stmt);
            } else {
                self.synchronize();
            }
            if self.at(TokenKind::Semicolon) {
                self.advance();
            }
        }
        let end = if let Some(rb) = self.consume(TokenKind::RBrace, "expected `}` to end block") {
            rb.span.end
        } else {
            lb.span.end
        };
        Some(Block {
            span: Span::new(start, end),
            stmts,
        })
    }

    pub(super) fn parse_stmt(&mut self) -> Option<Stmt> {
        if self.at(TokenKind::Let) {
            let let_tok = self.advance().unwrap();

            let mutable = if self.at(TokenKind::Mut) {
                self.advance();
                true
            } else {
                false
            };

            let name_tok = self.consume(TokenKind::Ident, "expected identifier after `let`")?;
            let mut ty = None;
            if self.at(TokenKind::Colon) {
                self.advance();
                ty = self.parse_type_expr();
            }
            let mut value = None;
            if self.at(TokenKind::Eq) {
                self.advance();
                value = self.parse_expr();
                if value.is_some() {
                    self.recover_unparenthesized_multi_closure();
                }
            }
            let end = value
                .as_ref()
                .map(|v| self.expr_end(v))
                .unwrap_or(name_tok.span.end);
            let span = Span::new(let_tok.span.start, end);
            return Some(Stmt::Let {
                name: Ident(self.slice(name_tok)),
                mutable,
                ty,
                value,
                span,
            });
        }

        if self.at(TokenKind::Ret) {
            let ret_tok = self.advance().unwrap();
            let expr = if self.at(TokenKind::RBrace) || self.at(TokenKind::Semicolon) {
                None
            } else {
                self.parse_expr()
            };
            if expr.is_some() {
                self.recover_unparenthesized_multi_closure();
            }
            let end = expr
                .as_ref()
                .map(|e| self.expr_end(e))
                .unwrap_or(ret_tok.span.end);
            return Some(Stmt::Ret(expr, Span::new(ret_tok.span.start, end)));
        }

        if self.at(TokenKind::Break) {
            let break_tok = self.advance().unwrap();
            return Some(Stmt::Break(break_tok.span));
        }

        if self.at(TokenKind::Cont) {
            let cont_tok = self.advance().unwrap();
            return Some(Stmt::Continue(cont_tok.span));
        }

        // Try to parse as assignment or expression statement
        let expr = self.parse_expr()?;

        // Check if this is an assignment (target = value)
        if self.at(TokenKind::Eq) {
            let start = self.expr_start(&expr);
            self.advance(); // consume =

            let value = self.parse_expr()?;
            self.recover_unparenthesized_multi_closure();
            let end = self.expr_end(&value);
            let span = Span::new(start, end);

            return Some(Stmt::Assign {
                target: expr,
                value,
                span,
            });
        }

        // Otherwise, it's an expression statement
        self.recover_unparenthesized_multi_closure();
        let span = Span::new(self.expr_start(&expr), self.expr_end(&expr));
        Some(Stmt::Expr(expr, span))
    }

    /// Recognize the invalid statement-level spelling `x, y -> body` after
    /// `x` has already parsed as an expression. This deliberately runs only at
    /// statement boundaries: a comma inside `call(x, y -> y)` separates valid
    /// arguments and must remain owned by the call parser.
    pub(super) fn recover_unparenthesized_multi_closure(&mut self) -> bool {
        if !self.at(TokenKind::Comma) {
            return false;
        }

        let mut index = self.pos;
        let mut parameter_count = 1usize;
        while self
            .tokens
            .get(index)
            .is_some_and(|token| token.kind == TokenKind::Comma)
        {
            index += 1;
            if !self
                .tokens
                .get(index)
                .is_some_and(|token| token.kind == TokenKind::Ident)
            {
                return false;
            }
            parameter_count += 1;
            index += 1;
        }

        if parameter_count < 2
            || !self
                .tokens
                .get(index)
                .is_some_and(|token| token.kind == TokenKind::Arrow)
        {
            return false;
        }

        let start = self.tokens[self.pos].span.start;
        let arrow = &self.tokens[index];
        self.diagnostics.push(Diagnostic::error(
            "multiple closure parameters require parentheses, for example `(x, y) -> ...`",
            Some(Span::new(start, arrow.span.end)),
        ));
        self.pos = index + 1;

        if !self.at(TokenKind::Eof) && !self.at(TokenKind::Semicolon) && !self.at(TokenKind::RBrace)
        {
            let _ = self.parse_expr();
        }
        true
    }

    pub(super) fn parse_if(&mut self, start: u32) -> Option<Expr> {
        let prev = self.suppress_struct_literals;
        self.suppress_struct_literals = true;
        let cond = self.parse_expr()?;
        self.suppress_struct_literals = prev;
        let then_block = self.parse_block()?;
        let else_block = if self.at(TokenKind::Else) {
            self.advance();
            if self.at(TokenKind::If) {
                let if_tok = self.advance().unwrap();
                let nested_start = if_tok.span.start;
                let nested = self.parse_if(nested_start)?;
                Some(Block {
                    span: Span::new(self.expr_start(&nested), self.expr_end(&nested)),
                    stmts: vec![Stmt::Expr(
                        nested.clone(),
                        Span::new(self.expr_start(&nested), self.expr_end(&nested)),
                    )],
                })
            } else {
                self.parse_block()
            }
        } else {
            None
        };
        let end = else_block
            .as_ref()
            .map(|b| b.span.end)
            .unwrap_or(then_block.span.end);
        Some(Expr::If {
            cond: Box::new(cond),
            then_block,
            else_block,
            span: Span::new(start, end),
        })
    }

    pub(super) fn parse_while(&mut self, start: u32) -> Option<Expr> {
        let prev = self.suppress_struct_literals;
        self.suppress_struct_literals = true;
        let cond = self.parse_expr()?;
        self.suppress_struct_literals = prev;
        let body = self.parse_block()?;
        let end = body.span.end;
        Some(Expr::While {
            cond: Box::new(cond),
            body,
            span: Span::new(start, end),
        })
    }

    pub(super) fn parse_for(&mut self, start: u32) -> Option<Expr> {
        let var_tok = self.consume(TokenKind::Ident, "expected variable name in for loop")?;
        let var = Ident(self.slice(var_tok));

        self.consume(TokenKind::In, "expected `in` after loop variable")?;

        // Parse the first expression - could be a range start or an iterable.
        // Suppress struct literals so `for x in names { ... }` doesn't try to
        // parse `names { ... }` as a struct literal.
        let prev = self.suppress_struct_literals;
        self.suppress_struct_literals = true;
        let first_expr = self.parse_expr()?;
        self.suppress_struct_literals = prev;

        if self.at(TokenKind::DotDot) {
            // Range loop: for x in start..end { ... }
            self.advance(); // consume ..
            let range_end = self.parse_expr()?;
            let body = self.parse_block()?;
            let end = body.span.end;
            Some(Expr::For {
                var,
                start: Box::new(first_expr),
                end: Box::new(range_end),
                body,
                span: Span::new(start, end),
            })
        } else {
            // Collection loop: for x in collection { ... }
            let body = self.parse_block()?;
            let end = body.span.end;
            Some(Expr::ForIn {
                var,
                iter: Box::new(first_expr),
                body,
                span: Span::new(start, end),
            })
        }
    }
}
