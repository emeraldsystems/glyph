use serde::{Deserialize, Serialize};

use crate::span::Span;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenKind {
    // Keywords
    Fn,
    Const,
    Let,
    Mut,
    Move,
    Type,
    Struct,
    Interface,
    Enum,
    Impl,
    Use,
    Import,
    From,
    As,
    Pub,
    If,
    Else,
    For,
    In,
    While,
    Match,
    Break,
    Cont,
    Ret,
    Extern,
    // Identifiers and literals
    Ident,
    Int,
    Float,
    Str,
    Char,
    InterpStr,
    Bool,
    // Punctuation / operators
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Semicolon,
    Dot,
    DotDot,
    Arrow,
    FatArrow,
    ColonColon,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    EqEq,
    Bang,
    BangEq,
    Lt,
    Gt,
    Le,
    Ge,
    Amp,
    AmpAmp,
    PipePipe,
    Question,
    Eof,
    Unknown,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}
