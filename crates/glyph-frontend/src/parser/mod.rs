use glyph_core::{
    ast::{
        BinaryOp, Block, CaptureMode, ConstDef, EnumDef, EnumVariantDef, Expr, ExternFunctionDecl,
        FieldDef, Function, Ident, ImplBlock, Import, ImportItem, ImportKind, ImportPath,
        InlineImpl, InterfaceDef, InterfaceMethod, InterpSegment, Item, Literal, MatchArm,
        MatchPattern, Module, Param, Stmt, StructDef, TypeExpr, UnaryOp,
    },
    diag::Diagnostic,
    span::Span,
    token::{Token, TokenKind},
    types::Mutability,
};

mod expr;
mod imports;
mod items;
mod stmts;
mod types;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ParseOutput {
    pub module: Module,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn parse(tokens: &[Token], source: &str) -> ParseOutput {
    let mut parser = Parser {
        tokens,
        source,
        pos: 0,
        diagnostics: Vec::new(),
        items: Vec::new(),
        pending_items: Vec::new(),
        suppress_struct_literals: false,
    };

    let mut imports = Vec::new();

    // Parse imports first (must come before items)
    while parser.at(TokenKind::Import) || parser.at(TokenKind::From) {
        match parser.parse_import() {
            Some(import) => imports.push(import),
            None => parser.synchronize(),
        }
    }

    // Then parse items with deduplication helper
    while !parser.at(TokenKind::Eof) {
        if !parser.parse_top_level_item() {
            parser.synchronize();
        }
    }

    parser.flush_pending_items();

    ParseOutput {
        module: Module {
            imports,
            items: parser.items,
        },
        diagnostics: parser.diagnostics,
    }
}

pub(super) struct Parser<'a> {
    pub(super) tokens: &'a [Token],
    pub(super) source: &'a str,
    pub(super) pos: usize,
    pub(super) diagnostics: Vec<Diagnostic>,
    pub(super) items: Vec<Item>,
    pub(super) pending_items: Vec<Item>,
    pub(super) suppress_struct_literals: bool,
}

impl<'a> Parser<'a> {
    pub(super) fn at(&self, kind: TokenKind) -> bool {
        match self.peek() {
            Some(t) => t.kind == kind,
            None => kind == TokenKind::Eof,
        }
    }

    pub(super) fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    pub(super) fn flush_pending_items(&mut self) {
        if self.pending_items.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending_items);
        self.items.extend(pending);
    }

    pub(super) fn advance(&mut self) -> Option<&'a Token> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_top_level_item(&mut self) -> bool {
        if self.at(TokenKind::Struct) {
            match self.parse_struct() {
                Some(s) => {
                    self.items.push(Item::Struct(s));
                    self.flush_pending_items();
                    return true;
                }
                None => return false,
            }
        }

        if self.at(TokenKind::Enum) {
            match self.parse_enum() {
                Some(e) => {
                    self.items.push(Item::Enum(e));
                    self.flush_pending_items();
                    return true;
                }
                None => return false,
            }
        }

        if self.at(TokenKind::Interface) {
            match self.parse_interface() {
                Some(iface) => {
                    self.items.push(Item::Interface(iface));
                    self.flush_pending_items();
                    return true;
                }
                None => return false,
            }
        }

        if self.at(TokenKind::Impl) {
            match self.parse_impl_block() {
                Some(block) => {
                    self.items.push(Item::Impl(block));
                    self.flush_pending_items();
                    return true;
                }
                None => return false,
            }
        }

        if self.at(TokenKind::Fn) {
            match self.parse_function() {
                Some(func) => {
                    self.items.push(Item::Function(func));
                    return true;
                }
                None => return false,
            }
        }

        if self.at(TokenKind::Const) {
            match self.parse_const() {
                Some(def) => {
                    self.items.push(Item::Const(def));
                    return true;
                }
                None => return false,
            }
        }

        if self.at(TokenKind::Pub) {
            self.advance();
            if self.at(TokenKind::Struct) {
                match self.parse_struct() {
                    Some(s) => {
                        self.items.push(Item::Struct(s));
                        self.flush_pending_items();
                        return true;
                    }
                    None => return false,
                }
            } else if self.at(TokenKind::Enum) {
                match self.parse_enum() {
                    Some(e) => {
                        self.items.push(Item::Enum(e));
                        self.flush_pending_items();
                        return true;
                    }
                    None => return false,
                }
            } else if self.at(TokenKind::Interface) {
                match self.parse_interface() {
                    Some(iface) => {
                        self.items.push(Item::Interface(iface));
                        self.flush_pending_items();
                        return true;
                    }
                    None => return false,
                }
            } else if self.at(TokenKind::Impl) {
                match self.parse_impl_block() {
                    Some(block) => {
                        self.items.push(Item::Impl(block));
                        self.flush_pending_items();
                        return true;
                    }
                    None => return false,
                }
            } else if self.at(TokenKind::Fn) {
                match self.parse_function() {
                    Some(func) => {
                        self.items.push(Item::Function(func));
                        return true;
                    }
                    None => return false,
                }
            } else if self.at(TokenKind::Const) {
                match self.parse_const() {
                    Some(def) => {
                        self.items.push(Item::Const(def));
                        return true;
                    }
                    None => return false,
                }
            } else if self.at(TokenKind::Extern) {
                match self.parse_extern_function() {
                    Some(extern_fn) => {
                        self.items.push(Item::ExternFunction(extern_fn));
                        return true;
                    }
                    None => return false,
                }
            } else {
                let span = self.peek().map(|t| t.span);
                self.diagnostics
                    .push(Diagnostic::error("expected item after `pub`", span));
                return false;
            }
        }

        if self.at(TokenKind::Extern) {
            match self.parse_extern_function() {
                Some(extern_fn) => {
                    self.items.push(Item::ExternFunction(extern_fn));
                    return true;
                }
                None => return false,
            }
        }

        let span = self.peek().map(|t| t.span);
        self.diagnostics.push(Diagnostic::error(
            "expected `const`, `fn`, `struct`, `enum`, `interface`, `impl`, or `extern`",
            span,
        ));
        false
    }

    pub(super) fn synchronize(&mut self) {
        while let Some(tok) = self.peek() {
            match tok.kind {
                TokenKind::Semicolon | TokenKind::RBrace => {
                    self.advance();
                    return;
                }
                TokenKind::Eof => {
                    self.advance();
                    return;
                }
                _ => {
                    self.pos += 1;
                }
            }
        }
    }

    pub(super) fn consume(&mut self, kind: TokenKind, msg: &str) -> Option<&'a Token> {
        if self.at(kind) {
            return self.advance();
        }
        let span = self.peek().map(|t| t.span);
        self.diagnostics.push(Diagnostic::error(msg, span));
        None
    }

    pub(super) fn slice(&self, tok: &Token) -> String {
        self.source
            .get(tok.span.start as usize..tok.span.end as usize)
            .unwrap_or("")
            .to_string()
    }

    pub(super) fn expr_start(&self, expr: &Expr) -> u32 {
        match expr {
            Expr::Lit(_, sp) => sp.start,
            Expr::InterpString { span, .. } => span.start,
            Expr::Ident(_, sp) => sp.start,
            Expr::Unary { span, .. } => span.start,
            Expr::Binary { span, .. } => span.start,
            Expr::Call { span, .. } => span.start,
            Expr::If { span, .. } => span.start,
            Expr::Block(block) => block.span.start,
            Expr::StructLit { span, .. } => span.start,
            Expr::FieldAccess { span, .. } => span.start,
            Expr::Ref { span, .. } => span.start,
            Expr::While { span, .. } => span.start,
            Expr::For { span, .. } => span.start,
            Expr::ArrayLit { span, .. } => span.start,
            Expr::Index { span, .. } => span.start,
            Expr::MethodCall { span, .. } => span.start,
            Expr::Match { span, .. } => span.start,
            Expr::Tuple { span, .. } => span.start,
            Expr::Try { span, .. } => span.start,
            Expr::Cast { span, .. } => span.start,
            Expr::Closure { span, .. } => span.start,
            Expr::ForIn { span, .. } => span.start,
        }
    }

    pub(super) fn expr_end(&self, expr: &Expr) -> u32 {
        match expr {
            Expr::Lit(_, sp) => sp.end,
            Expr::InterpString { span, .. } => span.end,
            Expr::Ident(_, sp) => sp.end,
            Expr::Unary { span, .. } => span.end,
            Expr::Binary { span, .. } => span.end,
            Expr::Call { span, .. } => span.end,
            Expr::If { span, .. } => span.end,
            Expr::Block(block) => block.span.end,
            Expr::StructLit { span, .. } => span.end,
            Expr::FieldAccess { span, .. } => span.end,
            Expr::Ref { span, .. } => span.end,
            Expr::While { span, .. } => span.end,
            Expr::For { span, .. } => span.end,
            Expr::ArrayLit { span, .. } => span.end,
            Expr::Index { span, .. } => span.end,
            Expr::MethodCall { span, .. } => span.end,
            Expr::Match { span, .. } => span.end,
            Expr::Tuple { span, .. } => span.end,
            Expr::Try { span, .. } => span.end,
            Expr::Cast { span, .. } => span.end,
            Expr::Closure { span, .. } => span.end,
            Expr::ForIn { span, .. } => span.end,
        }
    }

    pub(super) fn literal_from(&self, tok: &Token) -> Literal {
        let text = self.slice(tok);
        match tok.kind {
            // Literals above i64::MAX but within u64 range keep their bit
            // pattern; u64-typed contexts reinterpret them correctly.
            // parse_primary reports anything beyond u64 range.
            TokenKind::Int => Literal::Int(
                text.parse::<i64>()
                    .ok()
                    .or_else(|| text.parse::<u64>().ok().map(|v| v as i64))
                    .unwrap_or(0),
            ),
            TokenKind::Float => Literal::Float(text.parse().unwrap_or(0.0)),
            TokenKind::Str => Literal::Str(parse_string_literal(&text)),
            TokenKind::Char => Literal::Char(parse_char_literal(&text)),
            TokenKind::Bool => Literal::Bool(matches!(text.as_str(), "true")),
            _ => Literal::Int(0),
        }
    }

    pub(super) fn peek_binary_op(&self) -> Option<BinaryOp> {
        match self.peek().map(|t| &t.kind) {
            Some(TokenKind::Plus) => Some(BinaryOp::Add),
            Some(TokenKind::Minus) => Some(BinaryOp::Sub),
            Some(TokenKind::Star) => Some(BinaryOp::Mul),
            Some(TokenKind::Slash) => Some(BinaryOp::Div),
            Some(TokenKind::Percent) => Some(BinaryOp::Mod),
            Some(TokenKind::EqEq) => Some(BinaryOp::Eq),
            Some(TokenKind::BangEq) => Some(BinaryOp::Ne),
            Some(TokenKind::Lt) => Some(BinaryOp::Lt),
            Some(TokenKind::Le) => Some(BinaryOp::Le),
            Some(TokenKind::Gt) => Some(BinaryOp::Gt),
            Some(TokenKind::Ge) => Some(BinaryOp::Ge),
            Some(TokenKind::AmpAmp) => Some(BinaryOp::And),
            Some(TokenKind::PipePipe) => Some(BinaryOp::Or),
            _ => None,
        }
    }
}

pub fn render_type_expr(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Path { segments, .. } => segments.join("::"),
        TypeExpr::App { base, args, .. } => {
            let mut s = render_type_expr(base);
            let rendered_args: Vec<String> = args.iter().map(render_type_expr).collect();
            s.push('<');
            s.push_str(&rendered_args.join(", "));
            s.push('>');
            s
        }
        TypeExpr::Ref {
            mutability, inner, ..
        } => {
            let mut s = String::from("&");
            if matches!(mutability, Mutability::Mutable) {
                s.push_str("mut ");
            }
            s.push_str(&render_type_expr(inner));
            s
        }
        TypeExpr::Array { elem, size, .. } => format!("[{}; {}]", render_type_expr(elem), size),
        TypeExpr::Tuple { elements, .. } => {
            let elem_strs: Vec<String> = elements.iter().map(render_type_expr).collect();
            if elem_strs.len() == 1 {
                format!("({},)", elem_strs[0])
            } else {
                format!("({})", elem_strs.join(", "))
            }
        }
    }
}

pub(super) fn parse_char_literal(raw: &str) -> char {
    let trimmed = raw.trim_matches('\'');
    let mut chars = trimmed.chars();
    match chars.next() {
        Some('\\') => match chars.next() {
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            Some('\\') => '\\',
            Some('\'') => '\'',
            Some(other) => other,
            None => '\0',
        },
        Some(ch) => ch,
        None => '\0',
    }
}

pub(super) fn parse_string_literal(raw: &str) -> String {
    let trimmed = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);
    let mut bytes: Vec<u8> = Vec::new();
    let mut chars = trimmed.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                match next {
                    'n' => bytes.push(b'\n'),
                    't' => bytes.push(b'\t'),
                    'r' => bytes.push(b'\r'),
                    '\\' => bytes.push(b'\\'),
                    '"' => bytes.push(b'"'),
                    'x' => {
                        let h1 = chars.next();
                        let h2 = chars.next();
                        if let (Some(d1), Some(d2)) = (h1, h2) {
                            let hex_str: String = [d1, d2].iter().collect();
                            if let Ok(byte) = u8::from_str_radix(&hex_str, 16) {
                                bytes.push(byte);
                            } else {
                                bytes.push(b'x');
                                bytes.extend_from_slice(d1.encode_utf8(&mut [0; 4]).as_bytes());
                                bytes.extend_from_slice(d2.encode_utf8(&mut [0; 4]).as_bytes());
                            }
                        } else {
                            bytes.push(b'x');
                            if let Some(d1) = h1 {
                                bytes.extend_from_slice(d1.encode_utf8(&mut [0; 4]).as_bytes());
                            }
                        }
                    }
                    other => {
                        bytes.extend_from_slice(other.encode_utf8(&mut [0; 4]).as_bytes());
                    }
                }
            }
        } else {
            bytes.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
        }
    }

    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use glyph_core::token::TokenKind;
    use insta::assert_debug_snapshot;

    #[test]
    fn parses_simple_function() {
        let source = "fn add(a: i, b) { ret a + b }";
        let tokens = vec![
            Token::new(TokenKind::Fn, Span::new(0, 2)),
            Token::new(TokenKind::Ident, Span::new(3, 6)),
            Token::new(TokenKind::LParen, Span::new(6, 7)),
            Token::new(TokenKind::Ident, Span::new(7, 8)),
            Token::new(TokenKind::Colon, Span::new(8, 9)),
            Token::new(TokenKind::Ident, Span::new(10, 11)),
            Token::new(TokenKind::Comma, Span::new(11, 12)),
            Token::new(TokenKind::Ident, Span::new(13, 14)),
            Token::new(TokenKind::RParen, Span::new(14, 15)),
            Token::new(TokenKind::LBrace, Span::new(16, 17)),
            Token::new(TokenKind::Ret, Span::new(18, 21)),
            Token::new(TokenKind::Ident, Span::new(22, 23)),
            Token::new(TokenKind::Plus, Span::new(24, 25)),
            Token::new(TokenKind::Ident, Span::new(26, 27)),
            Token::new(TokenKind::RBrace, Span::new(28, 29)),
            Token::new(TokenKind::Eof, Span::new(29, 29)),
        ];
        let out = parse(&tokens, source);
        assert!(out.diagnostics.is_empty());
        assert_eq!(out.module.items.len(), 1);
        match &out.module.items[0] {
            Item::Function(f) => {
                assert_eq!(f.name.0, "add");
                assert_eq!(f.params.len(), 2);
            }
            other => panic!("expected function, got {:?}", other),
        }
    }

    #[test]
    fn parses_match_expression() {
        let source = r#"
fn main() -> i32 {
  let x = Some(5);
  let y = match x {
    Some(v) => v,
    None => 0,
  };
  ret y
}
"#;
        let lex_out = lex(source);
        let out = parse(&lex_out.tokens, source);
        assert!(
            out.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            out.diagnostics
        );
        assert_eq!(out.module.items.len(), 1);
    }

    #[test]
    fn parses_interpolated_string_with_hole() {
        let source = r#"
fn main() {
  let s = $"hi {x}";
  ret 0
}
"#;
        let lex_out = lex(source);
        let out = parse(&lex_out.tokens, source);
        assert!(
            out.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            out.diagnostics
        );

        match &out.module.items[0] {
            Item::Function(f) => match &f.body.stmts[0] {
                Stmt::Let {
                    value: Some(Expr::InterpString { segments, .. }),
                    ..
                } => {
                    assert_eq!(segments.len(), 2);
                }
                other => panic!("expected interpolated string let, got {:?}", other),
            },
            other => panic!("expected function, got {:?}", other),
        }
    }

    // Regression test for parsing a function call followed by an `if` expression
    // return in the same block. This previously hung in parser state recovery.
    #[test]
    fn parses_if_and_call() {
        let source = "fn f() { let x = foo(1, 2) ret if x { x } else { 0 } }";
        let tokens = lex_stub_tokens();
        let out = parse(&tokens, source);
        assert!(out.diagnostics.is_empty());
        assert_debug_snapshot!("if_call", out.module);
    }

    fn lex_stub_tokens() -> Vec<Token> {
        vec![
            Token::new(TokenKind::Fn, Span::new(0, 2)),
            Token::new(TokenKind::Ident, Span::new(3, 4)),
            Token::new(TokenKind::LParen, Span::new(4, 5)),
            Token::new(TokenKind::RParen, Span::new(5, 6)),
            Token::new(TokenKind::LBrace, Span::new(7, 8)),
            Token::new(TokenKind::Let, Span::new(9, 12)),
            Token::new(TokenKind::Ident, Span::new(13, 14)),
            Token::new(TokenKind::Eq, Span::new(15, 16)),
            Token::new(TokenKind::Ident, Span::new(17, 20)),
            Token::new(TokenKind::LParen, Span::new(20, 21)),
            Token::new(TokenKind::Int, Span::new(21, 22)),
            Token::new(TokenKind::Comma, Span::new(22, 23)),
            Token::new(TokenKind::Int, Span::new(24, 25)),
            Token::new(TokenKind::RParen, Span::new(25, 26)),
            Token::new(TokenKind::Ret, Span::new(27, 30)),
            Token::new(TokenKind::If, Span::new(31, 33)),
            Token::new(TokenKind::Ident, Span::new(34, 35)),
            Token::new(TokenKind::LBrace, Span::new(36, 37)),
            Token::new(TokenKind::Ident, Span::new(38, 39)),
            Token::new(TokenKind::RBrace, Span::new(40, 41)),
            Token::new(TokenKind::Else, Span::new(42, 46)),
            Token::new(TokenKind::LBrace, Span::new(47, 48)),
            Token::new(TokenKind::Int, Span::new(49, 50)),
            Token::new(TokenKind::RBrace, Span::new(51, 52)),
            Token::new(TokenKind::RBrace, Span::new(53, 54)),
            Token::new(TokenKind::Eof, Span::new(54, 54)),
        ]
    }

    #[test]
    fn reports_missing_fn_name() {
        let tokens = vec![
            Token::new(TokenKind::Fn, Span::new(0, 2)),
            Token::new(TokenKind::LParen, Span::new(2, 3)),
            Token::new(TokenKind::Eof, Span::new(3, 3)),
        ];
        let out = parse(&tokens, "fn(");
        assert!(!out.diagnostics.is_empty());
    }

    #[test]
    fn parses_extern_function_decl() {
        let source = r#"extern "C" fn puts(msg: RawPtr<i32>);"#;
        let lexed = lex(source);
        let out = parse(&lexed.tokens, source);
        assert!(
            out.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            out.diagnostics
        );
        assert_eq!(out.module.items.len(), 1);
        match &out.module.items[0] {
            Item::ExternFunction(f) => {
                assert_eq!(f.name.0, "puts");
                assert_eq!(f.abi.as_deref(), Some("C"));
            }
            other => panic!("expected extern function, got {:?}", other),
        }
    }

    #[test]
    fn rejects_extern_function_body() {
        let source = r#"extern "C" fn puts() {}"#;
        let lexed = lex(source);
        let out = parse(&lexed.tokens, source);
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.message.contains("extern functions cannot have a body")),
            "expected body error, got {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn rejects_unsupported_abi() {
        let source = r#"extern "sysv64" fn foo();"#;
        let lexed = lex(source);
        let out = parse(&lexed.tokens, source);
        assert!(
            out.diagnostics
                .iter()
                .any(|d| d.message.contains("unsupported ABI")),
            "expected ABI diagnostic, got {:?}",
            out.diagnostics
        );
    }

    #[test]
    fn extern_and_normal_function_in_same_module() {
        let source = r#"extern "C" fn puts(); fn main() { ret }"#;
        let lexed = lex(source);
        let out = parse(&lexed.tokens, source);
        assert!(out.diagnostics.is_empty(), "diags: {:?}", out.diagnostics);
        assert_eq!(out.module.items.len(), 2);
    }

    #[test]
    fn parses_from_import_statement() {
        let source = r#"from std import println
fn main() { println("hi") }"#;
        let lexed = lex(source);
        let out = parse(&lexed.tokens, source);
        assert!(out.diagnostics.is_empty(), "diags: {:?}", out.diagnostics);
        assert_eq!(out.module.imports.len(), 1);
        match &out.module.imports[0].kind {
            ImportKind::Selective { items } => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].name.0, "println");
            }
            other => panic!("expected selective import, got {:?}", other),
        }
        assert_eq!(out.module.imports[0].path.segments, vec!["std"]);
    }

    #[test]
    fn parses_from_import_with_alias() {
        let source = r#"from std import println as p
fn main() { p("hi") }"#;
        let lexed = lex(source);
        let out = parse(&lexed.tokens, source);
        assert!(out.diagnostics.is_empty(), "diags: {:?}", out.diagnostics);
        match &out.module.imports[0].kind {
            ImportKind::Selective { items } => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].name.0, "println");
                assert_eq!(items[0].alias.as_ref().map(|a| a.0.as_str()), Some("p"));
            }
            other => panic!("expected selective import, got {:?}", other),
        }
    }

    #[test]
    fn parses_string_literal_content() {
        let source = r#"fn main() { let msg = "hi\nthere" }"#;
        let lexed = lex(source);
        let out = parse(&lexed.tokens, source);
        assert!(out.diagnostics.is_empty(), "diags: {:?}", out.diagnostics);
        match &out.module.items[0] {
            Item::Function(f) => {
                if let Stmt::Let {
                    value: Some(expr), ..
                } = &f.body.stmts[0]
                {
                    match expr {
                        Expr::Lit(Literal::Str(s), _) => {
                            assert_eq!(s, "hi\nthere");
                        }
                        other => panic!("expected string literal, got {:?}", other),
                    }
                } else {
                    panic!("expected let binding");
                }
            }
            other => panic!("expected function, got {:?}", other),
        }
    }

    #[test]
    fn parses_unary_not() {
        let source = "fn main() { let x = !true }";
        let lexed = lex(source);
        let out = parse(&lexed.tokens, source);
        assert!(out.diagnostics.is_empty(), "diags: {:?}", out.diagnostics);

        let Item::Function(f) = &out.module.items[0] else {
            panic!("expected function");
        };
        let Stmt::Let {
            value: Some(Expr::Unary { op, expr, .. }),
            ..
        } = &f.body.stmts[0]
        else {
            panic!("expected unary not let binding");
        };

        assert_eq!(*op, UnaryOp::Not);
        assert!(matches!(expr.as_ref(), Expr::Lit(Literal::Bool(true), _)));
    }

    #[test]
    fn binary_operator_precedence_equality_vs_or() {
        let source = "fn main() { let x = ch == 32 || ch == 9 }";
        let lexed = lex(source);
        let out = parse(&lexed.tokens, source);
        assert!(out.diagnostics.is_empty(), "diags: {:?}", out.diagnostics);

        let Item::Function(f) = &out.module.items[0] else {
            panic!("expected function");
        };
        let Stmt::Let {
            value: Some(expr), ..
        } = &f.body.stmts[0]
        else {
            panic!("expected let binding");
        };

        match expr {
            Expr::Binary {
                op: BinaryOp::Or,
                lhs,
                rhs,
                ..
            } => {
                assert!(matches!(
                    lhs.as_ref(),
                    Expr::Binary {
                        op: BinaryOp::Eq,
                        ..
                    }
                ));
                assert!(matches!(
                    rhs.as_ref(),
                    Expr::Binary {
                        op: BinaryOp::Eq,
                        ..
                    }
                ));
            }
            other => panic!("expected top-level ||, got {:?}", other),
        }
    }

    #[test]
    fn parses_const_item() {
        let source = "const SUCCESS: i32 = 0\nfn main() { ret SUCCESS }";
        let lexed = lex(source);
        let out = parse(&lexed.tokens, source);
        assert!(out.diagnostics.is_empty(), "diags: {:?}", out.diagnostics);
        assert_eq!(out.module.items.len(), 2);
        match &out.module.items[0] {
            Item::Const(def) => {
                assert_eq!(def.name.0, "SUCCESS");
                match &def.ty {
                    TypeExpr::Path { segments, .. } => {
                        assert_eq!(segments, &vec!["i32".to_string()])
                    }
                    other => panic!("expected type path, got {:?}", other),
                }
                match &def.value {
                    Expr::Lit(Literal::Int(0), _) => {}
                    other => panic!("expected int literal, got {:?}", other),
                }
            }
            other => panic!("expected const item, got {:?}", other),
        }
    }

    #[test]
    fn parses_hex_escape_in_string_literal() {
        assert_eq!(parse_string_literal(r#""\x1b[31m""#), "\x1b[31m");
    }

    #[test]
    fn parses_multiple_hex_escapes() {
        assert_eq!(
            parse_string_literal(r#""\x1b[31mRed\x1b[0m""#),
            "\x1b[31mRed\x1b[0m"
        );
    }

    #[test]
    fn parses_hex_escape_boundary_values() {
        assert_eq!(parse_string_literal(r#""\x00""#), "\x00");
        assert_eq!(parse_string_literal(r#""\x7f""#), "\x7f");
        // Standalone \xff is invalid UTF-8; treated as raw byte via lossy conversion
        assert_eq!(parse_string_literal(r#""\xff""#), "\u{fffd}");
    }

    #[test]
    fn parses_multibyte_utf8_hex_escapes() {
        // Braille character ⠋ (U+280B) = UTF-8 bytes 0xe2, 0xa0, 0x8b
        let result = parse_string_literal(r#""\xe2\xa0\x8b""#);
        assert_eq!(result, "\u{280b}");
        assert_eq!(result.as_bytes(), &[0xe2, 0xa0, 0x8b]);
    }

    #[test]
    fn hex_escape_invalid_digits_fallback() {
        // Invalid hex digits fall back to literal characters
        assert_eq!(parse_string_literal(r#""\xZZ""#), "xZZ");
    }

    #[test]
    fn parses_hex_escape_in_interpolated_string() {
        let source = r#"
fn main() {
  let s = $"\x1b[31mhello\x1b[0m"
}
"#;
        let lexed = lex(source);
        let out = parse(&lexed.tokens, source);
        assert!(
            out.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            out.diagnostics
        );

        match &out.module.items[0] {
            Item::Function(f) => match &f.body.stmts[0] {
                Stmt::Let {
                    value: Some(Expr::InterpString { segments, .. }),
                    ..
                } => {
                    assert_eq!(segments.len(), 1);
                    match &segments[0] {
                        InterpSegment::Literal(s) => {
                            assert_eq!(s, "\x1b[31mhello\x1b[0m");
                        }
                        other => panic!("expected literal segment, got {:?}", other),
                    }
                }
                other => panic!("expected interpolated string let, got {:?}", other),
            },
            other => panic!("expected function, got {:?}", other),
        }
    }

    #[test]
    fn hex_escape_mixed_with_regular_escapes() {
        assert_eq!(
            parse_string_literal(r#""hello\n\x1b[31mred\x1b[0m\tworld""#),
            "hello\n\x1b[31mred\x1b[0m\tworld"
        );
    }

    #[test]
    fn parses_modulo_expression() {
        let source = "fn main() { let x = 10 % 3 }";
        let lexed = lex(source);
        let out = parse(&lexed.tokens, source);
        assert!(out.diagnostics.is_empty(), "diags: {:?}", out.diagnostics);

        let Item::Function(f) = &out.module.items[0] else {
            panic!("expected function");
        };
        let Stmt::Let {
            value: Some(expr), ..
        } = &f.body.stmts[0]
        else {
            panic!("expected let binding");
        };
        assert!(
            matches!(
                expr,
                Expr::Binary {
                    op: BinaryOp::Mod,
                    ..
                }
            ),
            "expected BinaryOp::Mod, got {:?}",
            expr
        );
    }

    #[test]
    fn modulo_precedence_vs_add() {
        let source = "fn main() { let x = a + b % c }";
        let lexed = lex(source);
        let out = parse(&lexed.tokens, source);
        assert!(out.diagnostics.is_empty(), "diags: {:?}", out.diagnostics);

        let Item::Function(f) = &out.module.items[0] else {
            panic!("expected function");
        };
        let Stmt::Let {
            value: Some(expr), ..
        } = &f.body.stmts[0]
        else {
            panic!("expected let binding");
        };
        // % binds tighter than +, so top-level op should be Add
        match expr {
            Expr::Binary {
                op: BinaryOp::Add,
                rhs,
                ..
            } => {
                assert!(
                    matches!(
                        rhs.as_ref(),
                        Expr::Binary {
                            op: BinaryOp::Mod,
                            ..
                        }
                    ),
                    "expected rhs to be Mod, got {:?}",
                    rhs
                );
            }
            other => panic!("expected top-level Add, got {:?}", other),
        }
    }

    #[test]
    fn const_requires_type_annotation() {
        let source = "const MISSING = 1";
        let lexed = lex(source);
        let out = parse(&lexed.tokens, source);
        assert!(
            out.diagnostics
                .iter()
                .any(|d| { d.message.contains("requires explicit type annotation") })
        );
    }

    fn parse_source(source: &str) -> ParseOutput {
        let lexed = lex(source);
        assert!(
            lexed.diagnostics.is_empty(),
            "unexpected lexer diagnostics: {:?}",
            lexed.diagnostics
        );
        parse(&lexed.tokens, source)
    }

    fn let_value<'a>(function: &'a Function, index: usize) -> &'a Expr {
        match &function.body.stmts[index] {
            Stmt::Let {
                value: Some(value), ..
            } => value,
            other => panic!("expected initialized let statement, got {other:?}"),
        }
    }

    #[test]
    fn parses_arrow_closure_forms_with_precise_spans() {
        let source = "fn main() { let zero = () -> 1; let one = x -> x; let many = move (x: i32, y) -> { x + y } }";
        let out = parse_source(source);
        assert!(out.diagnostics.is_empty(), "diags: {:?}", out.diagnostics);
        let Item::Function(function) = &out.module.items[0] else {
            panic!("expected function");
        };

        let Expr::Closure {
            capture,
            params,
            span,
            ..
        } = let_value(function, 0)
        else {
            panic!("expected zero-parameter closure");
        };
        assert_eq!(*capture, CaptureMode::Inferred);
        assert!(params.is_empty());
        assert_eq!(&source[span.start as usize..span.end as usize], "() -> 1");

        let Expr::Closure { params, span, .. } = let_value(function, 1) else {
            panic!("expected one-parameter closure");
        };
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name.0, "x");
        assert_eq!(&source[span.start as usize..span.end as usize], "x -> x");

        let Expr::Closure {
            capture,
            params,
            body,
            span,
        } = let_value(function, 2)
        else {
            panic!("expected move closure");
        };
        assert_eq!(*capture, CaptureMode::Move);
        assert_eq!(params.len(), 2);
        assert!(params[0].ty.is_some());
        assert_eq!(
            &source[params[0].span.start as usize..params[0].span.end as usize],
            "x: i32"
        );
        assert_eq!(
            &source[span.start as usize..span.end as usize],
            "move (x: i32, y) -> { x + y }"
        );
        assert!(matches!(body.as_ref(), Expr::Block(_)));
    }

    #[test]
    fn arrow_closures_are_low_precedence_and_right_associative() {
        let source = "fn main() { let f = x -> y -> x + y * 2 }";
        let out = parse_source(source);
        assert!(out.diagnostics.is_empty(), "diags: {:?}", out.diagnostics);
        let Item::Function(function) = &out.module.items[0] else {
            panic!("expected function");
        };
        let Expr::Closure { body, .. } = let_value(function, 0) else {
            panic!("expected outer closure");
        };
        let Expr::Closure { body, .. } = body.as_ref() else {
            panic!("expected right-associated inner closure");
        };
        let Expr::Binary {
            op: BinaryOp::Add,
            rhs,
            ..
        } = body.as_ref()
        else {
            panic!("expected addition closure body");
        };
        assert!(matches!(
            rhs.as_ref(),
            Expr::Binary {
                op: BinaryOp::Mul,
                ..
            }
        ));
    }

    #[test]
    fn parses_closures_in_calls_tuples_blocks_if_and_match() {
        let source = r#"
fn main() -> i32 {
  let called = apply(x -> x + 1, () -> { 2 });
  let tupled = (move (x: i32, y: i32) -> x + y, 3);
  let selected = if true { x -> x } else { x -> x + 1 };
  let matched = match value {
    Some(v) => x -> x + v,
    None => () -> 0,
  };
  ret 0
}
"#;
        let out = parse_source(source);
        assert!(out.diagnostics.is_empty(), "diags: {:?}", out.diagnostics);
        let Item::Function(function) = &out.module.items[0] else {
            panic!("expected function");
        };
        assert!(
            function.ret_type.is_some(),
            "function return arrow was lost"
        );

        let Expr::Call { args, .. } = let_value(function, 0) else {
            panic!("expected call");
        };
        assert!(matches!(
            args.as_slice(),
            [Expr::Closure { .. }, Expr::Closure { .. }]
        ));
        assert!(matches!(let_value(function, 1), Expr::Tuple { .. }));

        let Expr::If {
            then_block,
            else_block,
            ..
        } = let_value(function, 2)
        else {
            panic!("expected if expression");
        };
        assert!(matches!(
            then_block.stmts[0],
            Stmt::Expr(Expr::Closure { .. }, _)
        ));
        assert!(matches!(
            else_block.as_ref().unwrap().stmts[0],
            Stmt::Expr(Expr::Closure { .. }, _)
        ));

        let Expr::Match { arms, .. } = let_value(function, 3) else {
            panic!("expected match expression");
        };
        assert!(
            arms.iter()
                .all(|arm| matches!(arm.expr, Expr::Closure { .. }))
        );
    }

    #[test]
    fn malformed_closures_report_specific_errors_and_recover() {
        let source = r#"
fn main() {
  let bad_params = (x + y) -> x;
  let missing_params = move -> 1;
  let missing_body = x ->;
  let recovered = y -> y;
}
"#;
        let out = parse_source(source);
        assert!(
            out.diagnostics
                .iter()
                .any(|diag| diag.message.contains("closure parameter")),
            "expected closure-parameter diagnostic, got {:?}",
            out.diagnostics
        );
        assert!(
            out.diagnostics
                .iter()
                .any(|diag| diag.message.contains("expected closure parameter")),
            "expected missing-parameter diagnostic, got {:?}",
            out.diagnostics
        );
        assert!(
            out.diagnostics
                .iter()
                .any(|diag| diag.message.contains("after closure `->`")),
            "expected missing-body diagnostic, got {:?}",
            out.diagnostics
        );

        let Item::Function(function) = &out.module.items[0] else {
            panic!("expected parser to recover the function");
        };
        assert!(function.body.stmts.iter().any(|stmt| matches!(
            stmt,
            Stmt::Let {
                name: Ident(name),
                value: Some(Expr::Closure { .. }),
                ..
            } if name == "recovered"
        )));
    }

    #[test]
    fn parses_unparenthesized_typed_single_parameter_closure() {
        let source = "fn main() { let typed = value: i32 -> value }";
        let out = parse_source(source);
        assert!(out.diagnostics.is_empty(), "diags: {:?}", out.diagnostics);
        let Item::Function(function) = &out.module.items[0] else {
            panic!("expected function");
        };
        let Expr::Closure { params, span, .. } = let_value(function, 0) else {
            panic!("expected closure");
        };
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name.0, "value");
        assert!(matches!(
            params[0].ty,
            Some(TypeExpr::Path { ref segments, .. }) if segments == &["i32"]
        ));
        assert_eq!(
            &source[params[0].span.start as usize..params[0].span.end as usize],
            "value: i32"
        );
        assert_eq!(
            &source[span.start as usize..span.end as usize],
            "value: i32 -> value"
        );
    }

    #[test]
    fn diagnoses_missing_multi_and_zero_parameter_parentheses_and_recovers() {
        let source = r#"
fn main() {
  let multi = x, y -> x + y;
  let zero = -> 1;
  let recovered = value -> value;
}
"#;
        let out = parse_source(source);
        assert_eq!(
            out.diagnostics
                .iter()
                .filter(|diag| diag
                    .message
                    .contains("closure parameters require parentheses"))
                .count(),
            1,
            "expected one targeted multi-parameter diagnostic: {:?}",
            out.diagnostics
        );
        assert_eq!(
            out.diagnostics
                .iter()
                .filter(|diag| diag.message.contains("requires `()` before `->`"))
                .count(),
            1,
            "expected one targeted zero-parameter diagnostic: {:?}",
            out.diagnostics
        );
        assert_eq!(
            out.diagnostics.len(),
            2,
            "unexpected cascade: {:?}",
            out.diagnostics
        );

        let Item::Function(function) = &out.module.items[0] else {
            panic!("expected parser to recover the function");
        };
        assert!(function.body.stmts.iter().any(|stmt| matches!(
            stmt,
            Stmt::Let {
                name: Ident(name),
                value: Some(Expr::Closure { .. }),
                ..
            } if name == "recovered"
        )));
    }

    #[test]
    fn comma_delimited_argument_before_closure_is_not_misdiagnosed() {
        let source = "fn main() { let result = apply(value, x -> x) }";
        let out = parse_source(source);
        assert!(out.diagnostics.is_empty(), "diags: {:?}", out.diagnostics);
        let Item::Function(function) = &out.module.items[0] else {
            panic!("expected function");
        };
        let Expr::Call { args, .. } = let_value(function, 0) else {
            panic!("expected call");
        };
        assert!(matches!(
            args.as_slice(),
            [Expr::Ident(..), Expr::Closure { .. }]
        ));
    }
}
