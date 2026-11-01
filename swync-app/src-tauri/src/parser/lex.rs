use logos::Logos;
use std::fmt;

/// Where a token came from, as a byte range into the source text.
///
/// Carried alongside the token stream rather than inside `Token`, which is
/// compared by value all through the parser — a span would make every one of
/// those comparisons position-sensitive.
pub type Span = std::ops::Range<usize>;

#[derive(Logos, Debug, PartialEq, Clone)]
#[logos(skip r"[ \t]+")]

#[logos(skip(r"//[^\n]*", allow_greedy = true))]
pub enum Token {
    #[token("+")]
    Ad,

    #[token("=")]
    Assign,

    #[token("{")]
    BraceOpen,

    #[token("}")]
    BraceClose,

    #[token("[")]
    BracketOpen,

    #[token("]")]
    BracketClose,

    #[token(":")]
    Colon,

    #[token(",")]
    Comma,

    #[token("/")]
    Div,

    /// Method sugar: `60.m2h`. One character, so `..=` wins by longest match
    /// and `.5` still lexes as a number.
    #[token(".")]
    Dot,

    #[token("..=")]
    DotDotEq,

    #[token("else")]
    Else,

    /// `enum Scale { major = [0, 2, 4, 5, 7, 9, 11] }`.
    ///
    /// A token rather than a contextual keyword, unlike `as`: `as` is only ever
    /// a keyword inside a `use`, where the parser already knows what it is
    /// reading, while `enum` has to be recognisable at the top level with
    /// nothing in front of it to say so.
    #[token("enum")]
    Enum,

    #[token("==")]
    EqEq,

    /// The arrow of a `match` arm: `Section.verse => kick`. Two characters, so
    /// it wins over `=` by longest match and an assignment still lexes.
    #[token("=>")]
    FatArrow,

    #[token("for")]
    For,

    #[token("fn")]
    Function,
    
    #[token(">=")]
    Ge,

    #[token(">")]
    Gt,
    
    #[regex("[a-zA-Z_][a-zA-Z0-9_]*", |lex| lex.slice().to_string())]
    Ident(String),

    #[token("if")]
    If,

    #[token("in")]
    In,

    #[token("<=")]
    Le,
    
    #[token("let")]
    Let,

    #[token("<")]
    Lt,

    #[token("match")]
    Match,

    #[token("*")]
    Mul,

    #[token("!=")]
    Ne,

    #[token("\n")]
    NewLine,

    #[token("null")]
    Null,

    #[regex(r"(\d+(\.\d+)?|\.\d+)([eE][+-]?\d+)?", |lex| lex.slice().parse::<f64>().ok())]
    Num(f64),

    #[token("(")]
    ParensOpen,

    #[token(")")]
    ParensClose,

    /// The separator in a module path: `drums::kick`. Two characters, so it
    /// wins over `:` by longest match and a named argument still lexes.
    #[token("::")]
    PathSep,

    /// `use drums::*` — every name a module defines. Lexed whole rather than as
    /// `::` then `*` so a line ending in one is recognisably over: a trailing
    /// `*` otherwise reads as a multiplication waiting for its right side.
    #[token("::*")]
    PathGlob,

    /// One `match` arm answering to several patterns: `1 | 3 => hat`.
    ///
    /// The language has no other use for the character — there is no bitwise
    /// or, and nothing infix reads as one — so it is only ever this.
    #[token("|")]
    Pipe,

    /// Marks a list as one value rather than a sequence to read through:
    /// `play(riff, bzzz, div: '[2, 3])`. Short for `list(2, 3)`.
    ///
    /// The Lisp spelling, for the Lisp meaning — everywhere else brackets
    /// around a lane are the steps of a pattern, and the quote is what says
    /// this pair is data instead. A character with no infix reading was the
    /// point of choosing it: `*` would have lexed just as well, but `a *[1, 2]`
    /// is a multiplication the parser has no way to tell from a quoted list.
    #[token("'")]
    Quote,

    /// A rest inside a pattern: `[220, `, 330, `]`.
    #[token("`")]
    Rest,

    /// A sounding step carrying no data, written as a single backslash.
    #[token("\\")]
    Trigger,

    #[token("%")]
    Percent,

    /// How long a step is, inside a pattern: `[220;2, 330, 440]`.
    ///
    /// The number is a *share* of the sequence rather than a duration — the
    /// list still fills exactly one cycle, and the lengths divide it between
    /// them. Absent, a step's length is one, which is the even division every
    /// pattern had before this existed.
    #[token(";")]
    Semi,

    /// A double-quoted string: the path in `load("break.wav")`, and nothing
    /// else so far. No escapes — a path with a `"` in it cannot be written,
    /// which is a trade for a rule that fits on one line and a lexer that
    /// cannot run off the end of a file looking for a closing quote.
    #[regex(r#""[^"\n]*""#, |lex| {
        let s = lex.slice();
        s[1..s.len() - 1].to_string()
    })]
    Str(String),

    #[token(">>")]
    ShiftRight,

    #[token("-")]
    Sub,

    #[token("use")]
    Use,

    Term,
}

/// A token as it is written, so parse errors can quote the source rather than
/// the enum: "expected `,` or `]`" instead of "expected Comma or BracketClose".
///
/// chumsky's `Rich` errors are only printable through this impl — without it
/// the parser's own message is the `Debug` spelling of a whole error vector.
impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Token::Ad => "+",
            Token::Assign => "=",
            Token::BraceOpen => "{",
            Token::BraceClose => "}",
            Token::BracketOpen => "[",
            Token::BracketClose => "]",
            Token::Colon => ":",
            Token::Comma => ",",
            Token::Div => "/",
            Token::Dot => ".",
            Token::DotDotEq => "..=",
            Token::Else => "else",
            Token::Enum => "enum",
            Token::EqEq => "==",
            Token::FatArrow => "=>",
            Token::For => "for",
            Token::Function => "fn",
            Token::Ge => ">=",
            Token::Gt => ">",
            Token::Ident(name) => return write!(f, "{name}"),
            Token::If => "if",
            Token::In => "in",
            Token::Le => "<=",
            Token::Let => "let",
            Token::Lt => "<",
            Token::Match => "match",
            Token::Mul => "*",
            Token::Ne => "!=",
            Token::NewLine => "a line break",
            Token::Null => "null",
            Token::Num(n) => return write!(f, "{n}"),
            Token::ParensOpen => "(",
            Token::ParensClose => ")",
            Token::PathSep => "::",
            Token::PathGlob => "::*",
            Token::Pipe => "|",
            Token::Quote => "'",
            Token::Rest => "`",
            Token::Trigger => "\\",
            Token::Percent => "%",
            Token::Semi => ";",
            Token::Str(s) => return write!(f, "\"{s}\""),
            Token::ShiftRight => ">>",
            Token::Sub => "-",
            Token::Use => "use",
            // Never written down: the lexer inserts it at the end of a line
            // that stands on its own, so name it the way a reader would.
            Token::Term => "the end of the line",
        };
        write!(f, "{s}")
    }
}

/// Insert statement terminators at the line breaks that end a statement, and
/// report where every surviving token came from.
///
/// The spans are what lets a parse error — which chumsky reports as an index
/// into the returned token stream — be turned back into a place in the source.
/// They are returned as a parallel vector rather than paired with the tokens
/// because the parser takes a `&[Token]` slice.
pub fn insert_terminators(raw: Vec<(Token, Span)>) -> (Vec<Token>, Vec<Span>) {

    fn can_end(t: &Token) -> bool {
        matches!(t,
            Token::Ident(_) | Token::Num(_) | Token::Rest | Token::Trigger |
            Token::BraceClose | Token::BracketClose | Token::ParensClose |
            // Only ever the last token of a `use`, and nothing can follow it.
            Token::PathGlob
        )
    }

    fn cont_next(t: &Token) -> bool {
        matches!(t,
            // BracketOpen is deliberately absent: a line starting with `[`
            // begins a new statement (a pattern), it does not index the
            // previous line's value.
            Token::Ad | Token::Assign | Token::BraceOpen |
            Token::Colon |
            Token::Comma | Token::Div | Token::Dot | Token::DotDotEq |
            Token::Else | Token::EqEq | Token::FatArrow | Token::Ge | Token::Gt |
            Token::Le | Token::Lt | Token::Mul | Token::Ne |
            Token::PathSep | Token::PathGlob |
            // A `match` arm broken over lines: the arrow, or the `|` of an
            // alternative given a column of its own.
            Token::Pipe |
            Token::Percent | Token::ShiftRight | Token::Sub
        )
    }

    fn convert(t: Token) -> Token {
        match t {
            Token::NewLine => unreachable!("new line handled separately"),
            _ => t,
        }
    }

    let mut out = Vec::with_capacity(raw.len());
    let mut spans: Vec<Span> = Vec::with_capacity(raw.len());
    let mut prev : Option<Token> = None;

    let mut depth: i32 = 0;
    let mut i = 0;

    while i < raw.len() {
        let (tok, span) = &raw[i];
        match tok {
            Token::ParensOpen | Token::BracketOpen => depth += 1,
            Token::ParensClose | Token::BracketClose => depth -= 1,
            _ => {}
        }

        if *tok == Token::NewLine {
            // The inserted terminator stands where the line break was, so an
            // error against it points at the end of the line it terminates.
            let span = span.clone();
            i += 1;
            if depth > 0 { continue; }

            let ends = prev.as_ref().map_or(false, can_end);
            if !ends { continue; }

            let mut j = i;
            while j < raw.len() && raw[j].0 == Token::NewLine { j += 1; }

            if let Some((nxt, _)) = raw.get(j) {
                if cont_next(nxt) { continue; }
            }

            out.push(Token::Term);
            spans.push(span);
            prev = Some(Token::NewLine);
            continue;
        }

        out.push(convert(tok.clone()));
        spans.push(span.clone());
        prev = Some(tok.clone());
        i += 1;
    }

    (out, spans)
}