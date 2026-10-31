use chumsky::error::{RichPattern, RichReason};
use chumsky::prelude::*;
use chumsky::input::ValueInput;
use logos::Logos;
use crate::diagnostic::{Diagnostic, Stage};
use crate::parser::lex::{insert_terminators, Span, Token};

#[derive(Clone, Debug, PartialEq)]
pub struct Ident(pub String);

#[derive(Clone, Debug, PartialEq)]
pub struct Param { pub name: Ident, pub default: Option<Expr> }

/// One argument at a call site. `name` is `Some` for `cut: 400`, which binds to
/// the parameter of that name rather than by position.
#[derive(Clone, Debug, PartialEq)]
pub struct Arg { pub name: Option<Ident>, pub value: Expr }

impl Arg {
    /// A positional argument, for the many places that synthesize calls.
    pub fn positional(value: Expr) -> Arg {
        Arg { name: None, value }
    }

    /// A named argument: `cut: 400`.
    pub fn named(name: &str, value: Expr) -> Arg {
        Arg { name: Some(Ident(name.to_string())), value }
    }
}

/// One alternative of an `enum`, and the value it stands for.
///
/// The value is optional because the two things enums are for want different
/// halves of this. A tag — `enum Section { verse, chorus }` — is worth nothing
/// but its own identity, and giving it a number would invent an ordering nobody
/// wrote. A named constant — `enum Scale { major = [0, 2, 4, 5, 7, 9, 11] }` —
/// is reached for precisely because of what it holds.
#[derive(Clone, Debug, PartialEq)]
pub struct EnumMember { pub name: Ident, pub value: Option<Expr> }

#[derive(Clone, Debug, PartialEq)]
pub enum SwyncItem {
    Function { name: Ident, params: Vec<Param>, body: Expr },
    Let { name: Ident, value: Expr },
    Call { func: Ident, args: Vec<Arg> },
    /// `enum Scale { major = [0, 2, 4, 5, 7, 9, 11], minor = [...] }`.
    Enum { name: Ident, members: Vec<EnumMember> },
    Expr(Expr),
    /// `use drums::kick` — another file's definitions, named here.
    ///
    /// Nothing downstream ever sees one: `imports::expand` replaces every
    /// `use` with the definitions it names before the program is lowered.
    Use(Use),
}

/// One name brought in by a `use`, and what it is called here: `kick`, or
/// `kick as thump`.
#[derive(Clone, Debug, PartialEq)]
pub struct UseName { pub name: Ident, pub alias: Option<Ident> }

/// What a `use` takes from the path it names.
#[derive(Clone, Debug, PartialEq)]
pub enum UseTree {
    /// `use lib::drums` — whichever of the module or the single item the path
    /// turns out to name; only the filesystem can say which. The `Ident` is an
    /// `as` alias, if one was written.
    Plain(Option<Ident>),
    /// `use lib::drums::{kick, snare as clap}`.
    Names(Vec<UseName>),
    /// `use lib::drums::*` — everything the module defines.
    Glob,
}

/// One `use` item.
#[derive(Clone, Debug, PartialEq)]
pub struct Use {
    /// The path as written, split on `::`.
    pub path: Vec<Ident>,
    pub tree: UseTree,
    /// Byte offset of the `use` keyword, so a module that cannot be found is
    /// reported on the line that asked for it.
    pub at: usize,
}

impl Use {
    /// The path as it was written, for messages.
    pub fn written(&self) -> String {
        self.path.iter().map(|s| s.0.as_str()).collect::<Vec<_>>().join("::")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Add   { lhs: Box<Expr>, rhs: Box<Expr> },
    Block { stmts: Vec<Statement>, tail: Box<Expr> },
    Call  { func: Ident, args: Vec<Arg> },
    Cmp { op: CmpOp, lhs: Box<Expr>, rhs: Box<Expr> },
    Chain { lhs: Box<Expr>, rhs: Box<Expr> },
    Div   { lhs: Box<Expr>, rhs: Box<Expr> },
    /// `for i in 0..=3 { f4;e }`. The `length` is the `;` its body ended with:
    /// what one step of the sequence being collected is worth. It is kept here
    /// rather than on the body because it describes the element the loop makes
    /// of that body, and nothing about the body itself.
    For { var: Ident, iter: Box<Expr>, body: Box<Expr>, length: Option<Box<Expr>> },
    If  { cond: Box<Expr>, then: Box<Expr>, otherwise: Option<Box<Expr>> },
    Index { base: Box<Expr>, index: Box<Expr> },
    Let { name: Ident, value: Box<Expr>, body: Box<Expr> },
    List(Vec<ListItem>),
    /// `match bar % 4 { 0 => kick, 1 | 3 => hat, _ => ` }`.
    ///
    /// Arms are tried in the order they are written and the first that matches
    /// is the one the expression is worth, so an earlier arm shadows a later
    /// one exactly as it reads.
    Match { subject: Box<Expr>, arms: Vec<MatchArm> },
    Mul   { lhs: Box<Expr>, rhs: Box<Expr> },
    Neg { expr: Box<Expr> },
    Num(f64),
    /// `'[2, 3]` — a list marked to be passed whole rather than read step by
    /// step. The same thing `list(2, 3)` builds, and it exists for the one
    /// position where a list is otherwise ambiguous: a `play` lane, where
    /// brackets are the sequence of values the notes read through.
    Quote { expr: Box<Expr> },
    Range { lo: Box<Expr>, hi: Box<Expr> },
    Rem { lhs: Box<Expr>, rhs: Box<Expr> },
    /// A double-quoted string. Only ever the path in `load("break.wav")` —
    /// there is nothing else in the language that takes one, and the lowerer
    /// says so if one turns up anywhere else.
    Str(String),
    /// A silent step in a pattern, written `` ` ``.
    Rest,
    /// A sounding step with no value, written as a single backslash.
    Trigger,
    Sub   { lhs: Box<Expr>, rhs: Box<Expr> },
    Var(Ident),
}


/// One element of a list, with the length a `;` gave it.
///
/// The length is an expression rather than a number because it is read at
/// lowering like every other constant the language folds — `[c4;beats, e4]`
/// should work for the same reason `play`'s `rate` may be a `let`. What it may
/// *mean* depends on how the list is read: time in a pattern, notes in a lane,
/// nothing at all in a list being used as data.
#[derive(Clone, Debug, PartialEq)]
pub struct ListItem {
    pub value: Expr,
    pub length: Option<Box<Expr>>,
}

impl ListItem {
    /// An element with no `;`, which is every element written before lengths
    /// existed and most of them since.
    pub fn plain(value: Expr) -> ListItem {
        ListItem { value, length: None }
    }
}

impl Pattern {
    /// The names this pattern binds, in the order they are written.
    ///
    /// Two passes have to agree about this exactly, which is why there is one
    /// copy of it and it lives beside the type rather than inside either of
    /// them. The lowerer defines what a binding *is*; the rename walk that
    /// expansion runs has to know the same answer, because a name it thought
    /// was a mention would be rewritten to whatever the file imported under
    /// it — turning `match n { q => … }` into a program about somebody else's
    /// `q`.
    pub fn bindings(&self, out: &mut Vec<Ident>) {
        match self {
            Pattern::Name(id) if binds(&id.0) => out.push(id.clone()),
            Pattern::List(items) | Pattern::Or(items) => {
                for item in items {
                    item.bindings(out);
                }
            }
            // A range's ends are read, not written; a `Member` reaches through
            // an enum; a `Name` that did not bind stands for a value.
            Pattern::Wild
            | Pattern::Num(_)
            | Pattern::Name(_)
            | Pattern::Member { .. }
            | Pattern::Range { .. }
            | Pattern::Rest
            | Pattern::Trigger => {}
        }
    }
}

/// Whether a bare name in pattern position binds the subject, or stands for the
/// value it spells.
///
/// The exception is the language's own two tables, and it is there because of
/// what this language is for. `match step { c4 => … }` has one reading anybody
/// means, and under Rust's rule — every bare name binds — it would silently
/// bind `c4` and take that arm for every note there is. Nothing about the text
/// would say so.
///
/// The order is [`Expr::Var`]'s own, minus the environment: the tuplet marker,
/// then a written note value, then a note. A pitch class spelled without an
/// octave — `a`, `ef` — binds, because in a pattern there is no sequence to
/// have said which octave, and refusing would cost the most ordinary binding
/// names in the language.
pub fn binds(name: &str) -> bool {
    if name == crate::lang::TUPLET || crate::lang::duration(name).is_some() {
        return false;
    }
    !matches!(
        crate::lang::note(name),
        crate::lang::NoteName::Note { .. } | crate::lang::NoteName::OctaveOutOfRange(_)
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CmpOp { Lt, Le, Gt, Ge, Eq, Ne }


#[derive(Clone, Debug, PartialEq)]
pub enum Statement {
    Let { name: Ident, value: Box<Expr> },
    Expr(Expr),
}

/// One arm of a `match`: what it answers to, and what it is worth.
///
/// The guard is an ordinary expression read inside the arm's own scope, so it
/// sees whatever the pattern bound — `n if n > 60` is the reason bindings are
/// pushed before the guard is asked rather than after it.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
}

/// What one `match` arm answers to.
///
/// A tree of its own rather than an [`Expr`], because the two are read in
/// opposite directions: an expression is evaluated to produce a value, while a
/// pattern is handed one and asked whether it fits. The overlap in spelling —
/// `440`, `Scale.major`, `[a, b]` — is exactly why they cannot share a type:
/// `a` in an expression is a name being *read*, and in a pattern it is a name
/// being *written*.
#[derive(Clone, Debug, PartialEq)]
pub enum Pattern {
    /// `_` — fits anything and binds nothing.
    Wild,
    /// `440`, `-12`.
    Num(f64),
    /// A bare name. Whether it binds the subject or stands for a note or a
    /// written value is not decided here: the parser has no tables to ask, and
    /// `c4` and `q` are spelled exactly as `n` is. See the lowerer.
    Name(Ident),
    /// `Section.verse` — one member of one enum, matched as a tag.
    ///
    /// A shape of its own rather than the [`Expr::Chain`] the same text builds
    /// in an expression, because only here is the right-hand side certainly a
    /// member: a pattern has no receiver to call a method on.
    Member { ty: Ident, member: Ident },
    /// `0..=3`, inclusive at both ends.
    ///
    /// The ends are expressions rather than patterns, and are read where the
    /// `match` is written: nothing binds in that position, so a name there is a
    /// name being read like any other — `root..=root + 12` means what it says.
    Range { lo: Expr, hi: Expr },
    /// `[a, 0, Scale.major]` — a list of exactly that length, element by
    /// element.
    List(Vec<Pattern>),
    /// A silent step, written `` ` ``.
    Rest,
    /// A sounding step carrying no value, written `\`.
    ///
    /// A bare trigger and nothing else: a written note value is a hit of that
    /// length, and the length is a thing it carries that this does not, so `q`
    /// is matched by its own name rather than swept up here.
    Trigger,
    /// `1 | 3 | 5` — one arm answering to several patterns.
    Or(Vec<Pattern>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Range { Const(i64, i64) }

// One enum per precedence tier, so each fold below can match exhaustively.
// A single shared BinOp would force a catch-all arm in both, which is how
// `Rem` once slipped through the product fold into `unreachable!()`.
#[derive(Clone, Debug)]
pub enum ProductOp {
    Mul, Div, Rem,
}

#[derive(Clone, Debug)]
pub enum SumOp {
    Add, Sub,
}

/// One postfix step. Both bind tighter than any infix operator, and they share
/// a fold so `xs[0].m2h` and `x.scale(s)[1]` both parse.
enum Postfix {
    Index(Expr),
    Method(Ident, Vec<Arg>),
}

/// Labelled, because `select!` has nothing to say about what it wanted: an
/// error where a name belongs would otherwise read "expected something else".
fn ident<'a, I>() -> impl Parser<'a, I, Ident, extra::Err<Rich<'a, Token>>> + Clone
where I: ValueInput<'a, Token = Token, Span = SimpleSpan> {
    select! { Token::Ident(s) => Ident(s) }.labelled("a name")
}

/// The `as` of `use drums as d`.
///
/// A name rather than a token of its own, so it is only a keyword where a
/// keyword is what it looks like: `as` on its own is still an ordinary name,
/// which some file somewhere is bound to be using.
fn kw_as<'a, I>() -> impl Parser<'a, I, (), extra::Err<Rich<'a, Token>>> + Clone
where I: ValueInput<'a, Token = Token, Span = SimpleSpan> {
    select! { Token::Ident(name) if name == "as" => () }.labelled("`as`")
}

/// The first member name written twice, if any.
///
/// Quadratic, and deliberately: an enum is a handful of names, and a `HashSet`
/// here would report *a* duplicate rather than the first one, which is the one
/// the writer's eye is already on.
fn first_duplicate(members: &[EnumMember]) -> Option<&str> {
    members.iter().enumerate().find_map(|(i, m)| {
        members[..i]
            .iter()
            .any(|earlier| earlier.name.0 == m.name.0)
            .then_some(m.name.0.as_str())
    })
}

/// A name, qualified by the module it came from if it needs to be:
/// `kick`, `drums::kick`.
///
/// The qualifier is kept in the `Ident` rather than beside it, because a
/// qualified name is still just a name to everything downstream — `use`
/// resolution rewrites it to the one the definition was filed under, and the
/// lowerer looks it up like any other.
fn name<'a, I>() -> impl Parser<'a, I, Ident, extra::Err<Rich<'a, Token>>> + Clone
where I: ValueInput<'a, Token = Token, Span = SimpleSpan> {
    ident()
        .then(
            just(Token::PathSep)
                .ignore_then(ident())
                .repeated()
                .collect::<Vec<_>>(),
        )
        .map(|(head, rest)| {
            if rest.is_empty() {
                return head;
            }
            let mut joined = head.0;
            for segment in rest {
                joined.push_str("::");
                joined.push_str(&segment.0);
            }
            Ident(joined)
        })
}

/// `{ let a = f * 2\n sin(a) }` — statements, then the expression the block is
/// worth, and the `;` length that expression carried if it had one.
///
/// The length is lifted out here rather than left on the tail because only the
/// block's *value* can have one: a `;` says how long a step lasts, and a
/// statement in the middle of a block is not a step. Every caller but `for`
/// goes through `block`, which refuses it — a `for` collecting a sequence is
/// the one place there is a step for it to describe.
///
/// Takes the expression parser rather than calling `expr` itself so the copy
/// inside `expr` can recurse through the recursive handle, while a function
/// body — which is not inside an expression yet — can build its own.
fn sized_block<'a, I, E>(expr: E)
    -> impl Parser<'a, I, (Expr, Option<Expr>), extra::Err<Rich<'a, Token>>> + Clone
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
    E: Parser<'a, I, Expr, extra::Err<Rich<'a, Token>>> + Clone,
{
    let sep = just(Token::Term).repeated().at_least(1);

    // The length is read onto every statement and refused below on all but the
    // last, so a `;` in the middle of a block is a message rather than a parse
    // error pointing at the token after it.
    let stmt = choice((
        expr.clone()
            .then(just(Token::Semi).ignore_then(expr.clone()).or_not())
            .map(|(e, length)| (Statement::Expr(e), length)),
        just(Token::Let)
            .ignore_then(ident())
            .then_ignore(just(Token::Assign))
            .then(expr)
            .map(|(name, value)| (Statement::Let { name, value: Box::new(value) }, None)),
    ));

    stmt.separated_by(sep)
        .allow_leading()
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(Token::BraceOpen), just(Token::BraceClose))
        .try_map(|mut stmts, span| {
            let Some((Statement::Expr(tail), length)) = stmts.pop() else {
                return Err(Rich::custom(span, "a block must end in an expression"));
            };
            if stmts.iter().any(|(_, length)| length.is_some()) {
                return Err(Rich::custom(span, "a `;` length belongs to the value a block \
                                              answers with, which is its last line"));
            }
            let stmts = stmts.into_iter().map(|(stmt, _)| stmt).collect();
            Ok((Expr::Block { stmts, tail: Box::new(tail) }, length))
        })
}

/// A block in every position but a `for` body, where a `;` length has nothing
/// to attach to.
fn block<'a, I, E>(expr: E) -> impl Parser<'a, I, Expr, extra::Err<Rich<'a, Token>>> + Clone
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
    E: Parser<'a, I, Expr, extra::Err<Rich<'a, Token>>> + Clone,
{
    sized_block(expr).try_map(|(block, length), span| match length {
        None => Ok(block),
        Some(_) => Err(Rich::custom(span, "a `;` length is how long a step of a sequence \
                                          lasts, and this block is not building one — only \
                                          a list and a `for` that collects into one have a \
                                          step for it to describe")),
    })
}

/// Read a program, or say what stopped it.
///
/// Both failures carry a place in the source: the lexer's is the character it
/// could not read, and the parser's is recovered by mapping the token index it
/// reports back through the spans the lexer kept.
pub fn parse(code: String) -> Result<Vec<SwyncItem>, Diagnostic> {
    let mut raw_tokens: Vec<(Token, Span)> = Vec::new();
    for (token, span) in Token::lexer(&code).spanned() {
        match token {
            Ok(token) => raw_tokens.push((token, span)),
            Err(_) => {
                let text = code.get(span.clone()).unwrap_or_default();
                return Err(Diagnostic::at(
                    Stage::Lex,
                    format!("`{text}` is not part of the language"),
                    &code,
                    span.start,
                ));
            }
        }
    }

    let (tokens, spans) = insert_terminators(raw_tokens);

    // Bound rather than returned directly: the parser borrows `tokens`, and as
    // a tail expression it would outlive them.
    let result = parser()
        .parse(&tokens[..])
        .into_result()
        .map_err(|errs| parse_diagnostic(&errs, &spans, &code));

    result.map(|mut items| {
        // A `use` carried the index of its own token out of the parser, which
        // is only a place in the file while the spans are still in scope.
        for item in &mut items {
            if let SwyncItem::Use(u) = item {
                u.at = spans.get(u.at).map_or(0, |s| s.start);
            }
        }
        items
    })
}

/// The first parse error, as something worth reading.
///
/// Only the first: chumsky reports every branch that failed, and after the
/// earliest one they are describing a parse that had already gone wrong.
fn parse_diagnostic(
    errs: &[Rich<'_, Token>],
    spans: &[Span],
    code: &str,
) -> Diagnostic {
    let Some(err) = errs.first() else {
        // `into_result` never gives an empty error vector, but a parse that
        // failed must not report itself as having no reason.
        return Diagnostic::message(Stage::Parse, "could not parse this program");
    };

    // The error's span indexes the token stream, not the source. A span past
    // the end is an error against end-of-input, which belongs after the last
    // token rather than nowhere.
    let offset = spans
        .get(err.span().start)
        .map(|s| s.start)
        .or_else(|| spans.last().map(|s| s.end))
        .unwrap_or(0);

    Diagnostic::at(Stage::Parse, parse_message(err), code, offset)
}

fn parse_message(err: &Rich<'_, Token>) -> String {
    match err.reason() {
        RichReason::Custom(message) => message.clone(),
        RichReason::ExpectedFound { expected, .. } => {
            let found = match err.found() {
                Some(token) => format!("unexpected `{token}`"),
                None => "the file ends here".to_string(),
            };

            // Ranked before sorting so the useful candidates survive the cap:
            // a call missing its `)` can have the parser expecting every infix
            // operator in the language alongside it, and the `)` is the answer.
            let mut ranked: Vec<(u8, String)> = expected.iter().map(pattern_name).collect();
            ranked.sort();
            ranked.dedup();
            let names: Vec<String> = ranked.into_iter().map(|(_, name)| name).collect();

            match expected_list(&names) {
                Some(list) => format!("{found}, expected {list}"),
                None => found,
            }
        }
    }
}

/// One thing the parser would have accepted, as it is written, paired with how
/// much it is worth saying: the thing that closes or separates what you are in
/// the middle of writing tells you more than the operators that could follow it.
fn pattern_name(pattern: &RichPattern<'_, Token>) -> (u8, String) {
    match pattern {
        // The two tokens whose `Display` is a phrase rather than a spelling:
        // backticks around them would be quoting something nobody can type.
        RichPattern::Token(token) if matches!(&**token, Token::Term | Token::NewLine) => {
            (1, token.to_string())
        }
        RichPattern::Token(token) => {
            let rank = match &**token {
                Token::ParensClose | Token::BracketClose | Token::BraceClose => 0,
                Token::Comma | Token::Colon | Token::Assign | Token::Term => 1,
                Token::ParensOpen | Token::BracketOpen | Token::BraceOpen => 2,
                Token::Else | Token::For | Token::Function | Token::If
                | Token::In | Token::Let | Token::Match | Token::Use => 2,
                _ => 3,
            };
            (rank, format!("`{}`", &**token))
        }
        RichPattern::EndOfInput => (1, "the end of the file".to_string()),
        other => (2, other.to_string()),
    }
}

/// "`,`", "`,` or `]`", "`,`, `]` or 3 more".
///
/// Capped because a failure early in an expression can have the parser
/// expecting every operator and every literal at once, and a list that long
/// says less than the first few of it do.
fn expected_list(names: &[String]) -> Option<String> {
    const SHOWN: usize = 4;

    match names.len() {
        0 => None,
        1 => Some(names[0].clone()),
        n if n <= SHOWN => Some(format!(
            "{} or {}",
            names[..n - 1].join(", "),
            names[n - 1],
        )),
        n => Some(format!(
            "{} or {} more",
            names[..SHOWN].join(", "),
            n - SHOWN,
        )),
    }
}

/// One `::`-separated piece of a `use` path: another name, or the braced list
/// that can only come last.
enum Segment {
    Name(Ident),
    Names(Vec<UseName>),
}

/// Assemble a `use` from its pieces, refusing the combinations that have no
/// meaning — a braced list mid-path, or a `*` and an `as` on the same item.
///
/// `at` is a token index here; `parse` maps it back to a byte offset once the
/// whole file has been read, which is the only place the spans still exist.
fn build_use(
    head: Ident,
    segments: Vec<Segment>,
    glob: bool,
    alias: Option<Ident>,
    at: usize,
) -> Result<Use, String> {
    let mut path = vec![head];
    let mut names: Option<Vec<UseName>> = None;

    let last = segments.len().saturating_sub(1);
    for (i, segment) in segments.into_iter().enumerate() {
        match segment {
            Segment::Name(name) => path.push(name),
            Segment::Names(_) if i != last => {
                return Err("a `{…}` list has to be the last part of a `use`".to_string())
            }
            Segment::Names(list) => names = Some(list),
        }
    }

    let tree = match (names, glob, alias) {
        (Some(_), true, _) | (Some(_), _, Some(_)) => {
            return Err("a `{…}` list already names what it takes".to_string())
        }
        (None, true, Some(_)) => {
            return Err("`use …::*` takes every name as it is written, so there \
                        is nothing for `as` to rename".to_string())
        }
        (Some(list), false, None) => UseTree::Names(list),
        (None, true, None) => UseTree::Glob,
        (None, false, alias) => UseTree::Plain(alias),
    };

    Ok(Use { path, tree, at })
}

fn expr<'a, I>() -> impl Parser<'a, I, Expr, extra::Err<Rich<'a, Token>>> + Clone
where I: ValueInput<'a, Token = Token, Span = SimpleSpan> {
    recursive(|expr| {
        let int = select! { Token::Num(n) => Expr::Num(n) }.labelled("a number");
        let string = select! { Token::Str(s) => Expr::Str(s) }.labelled("a string");
        let var = name().map(Expr::Var);
        let paren = expr.clone()
            .delimited_by(just(Token::ParensOpen), just(Token::ParensClose));

        // `cut: 400` is tried before a bare expression. Both start with an
        // Ident, so this relies on `choice` rewinding when the Colon is absent.
        let arg = choice((
            ident()
                .then_ignore(just(Token::Colon))
                .then(expr.clone())
                .map(|(name, value)| Arg { name: Some(name), value }),
            expr.clone().map(Arg::positional),
        ));

        let args = arg
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>();

        // `value;length`, the length optional. Nothing else in the grammar uses
        // `;`, so there is no ambiguity to resolve here — the only reason it
        // binds tighter than `,` is that the separator is what ends an element.
        let list_item = expr.clone()
            .then(just(Token::Semi).ignore_then(expr.clone()).or_not())
            .map(|(value, length)| ListItem { value, length: length.map(Box::new) });

        let list = list_item
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just(Token::BracketOpen), just(Token::BracketClose))
            .map(Expr::List);

        let call = name()
            .then(args.clone().delimited_by(just(Token::ParensOpen), just(Token::ParensClose)))
            .map(|(func, args)| Expr::Call { func, args });

        let block = block(expr.clone());

        let for_expr = just(Token::For)
            .ignore_then(ident())
            .then_ignore(just(Token::In))
            .then(expr.clone())
            .then(sized_block(expr.clone()))
            .map(|((var, iter), (body, length))| Expr::For {
                var,
                iter: Box::new(iter),
                body: Box::new(body),
                length: length.map(Box::new),
            });
        
        let if_expr = recursive(|if_expr| {
            just(Token::If)
                .ignore_then(expr.clone())
                .then(block.clone())
                .then(
                    just(Token::Else)
                        .ignore_then(choice((if_expr.clone(), block.clone())))
                        .or_not()
                )
                .map(|((cond, then), otherwise)| Expr::If {
                    cond: Box::new(cond),
                    then: Box::new(then),
                    otherwise: otherwise.map(Box::new),
                })
        });

        // A pattern, and the arm it heads. Built here rather than beside
        // `expr` for the one thing it needs from this scope: a range's ends and
        // a guard are ordinary expressions, and the recursive handle is the
        // only way to reach them.
        let pattern = recursive(|pattern| {
            // A range end. Only a number or a name, where the expression
            // grammar would take anything: `..=` is already an expression
            // operator, so handing this position to `expr` would have `0..=3`
            // parse as one `Expr::Range` before the pattern ever saw a range.
            let bound = choice((
                just(Token::Sub)
                    .or_not()
                    .then(select! { Token::Num(n) => n })
                    .map(|(neg, n)| Expr::Num(if neg.is_some() { -n } else { n })),
                name().map(Expr::Var),
            ));

            let range = bound
                .clone()
                .then_ignore(just(Token::DotDotEq))
                .then(bound)
                .map(|(lo, hi)| Pattern::Range { lo, hi });

            let member = name()
                .then_ignore(just(Token::Dot))
                .then(ident())
                .map(|(ty, member)| Pattern::Member { ty, member });

            let number = just(Token::Sub)
                .or_not()
                .then(select! { Token::Num(n) => n })
                .map(|(neg, n)| Pattern::Num(if neg.is_some() { -n } else { n }));

            // `_` lexes as an ordinary name, so the wildcard is told from a
            // binding here rather than in the lexer — which keeps `_` usable as
            // a name everywhere else, where it has never meant anything special.
            let bare = ident().map(|id| match id.0.as_str() {
                "_" => Pattern::Wild,
                _ => Pattern::Name(id),
            });

            let list = pattern
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(Token::BracketOpen), just(Token::BracketClose))
                .map(Pattern::List);

            // The two steps that carry no value, spelled as they are in a
            // sequence. Tokens of their own, so there is nothing for them to be
            // ordered against.
            let rest = just(Token::Rest).to(Pattern::Rest);
            let trigger = just(Token::Trigger).to(Pattern::Trigger);

            // `range` before `number` and `member` before `bare`: both pairs
            // start alike, and `choice` commits to the first branch that
            // succeeds — a number-first order would read `0..=3` as the pattern
            // `0` and leave `..=3` for the arrow.
            let atom = choice((list, range, member, number, rest, trigger, bare));

            // A leading `|` is allowed and means nothing, so a long list of
            // alternatives can be written with every one of them in the same
            // column.
            just(Token::Pipe)
                .or_not()
                .ignore_then(
                    atom.separated_by(just(Token::Pipe))
                        .at_least(1)
                        .collect::<Vec<_>>(),
                )
                .map(|mut alts| match alts.len() {
                    1 => alts.pop().expect("just checked there is one"),
                    _ => Pattern::Or(alts),
                })
                // Every combinator here is a type, and they nest: unboxed, the
                // pattern parser inside the expression recursion produced a
                // monomorphized name long enough for the macOS linker to refuse
                // it outright. Boxing costs one indirection per pattern parsed,
                // once, while the program is being read.
                .boxed()
        });

        let arm = pattern
            .then(just(Token::If).ignore_then(expr.clone()).or_not())
            .then_ignore(just(Token::FatArrow))
            .then(expr.clone())
            .map(|((pattern, guard), body)| MatchArm { pattern, guard, body });

        // Arms separate on a comma *or* a line break, and any number of
        // either — the same rule an `enum`'s members follow, for the same
        // reason: the lexer does not track brace depth, so a list written over
        // lines arrives with `Term`s between its entries and a one-line list
        // arrives with commas. See `enum_item`.
        let arm_sep = choice((
            just(Token::Comma).ignored(),
            just(Token::Term).ignored(),
        ))
        .repeated()
        .at_least(1);

        let match_expr = just(Token::Match)
            .ignore_then(expr.clone())
            .then(
                arm.separated_by(arm_sep)
                    .allow_leading()
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(just(Token::BraceOpen), just(Token::BraceClose)),
            )
            .try_map(|(subject, arms), span| match arms.is_empty() {
                // Not an expression at all: there is no arm for it to be worth,
                // and every other reading would be inventing a value.
                true => Err(Rich::custom(span, "a `match` needs at least one arm — \
                                                write `_ => …` for the value it is \
                                                worth when nothing else fits")),
                false => Ok(Expr::Match { subject: Box::new(subject), arms }),
            })
            .boxed();

        let let_expr = just(Token::Let)
            .ignore_then(ident())
            .then_ignore(just(Token::Assign))
            .then(expr.clone())
            .then_ignore(just(Token::In)) 
            .then(expr.clone())
            .map(|((name, value), body)| Expr::Let {
                name, value: Box::new(value), body: Box::new(body),
            });

        // `call` must be tried before `var`: both start with an Ident, and
        // choice commits to the first success — var-first would leave `(...)`
        // unconsumed and split `sin(440)` into two items.
        let rest = just(Token::Rest).to(Expr::Rest);
        let trigger = just(Token::Trigger).to(Expr::Trigger);

        let atom = choice((
            int, string, rest, trigger, list, for_expr, if_expr, match_expr, let_expr,
            block, call, var, paren,
        ));

        // Indexing and method calls share one fold so they interleave freely:
        // `xs[0].m2h`, `x.scale(s)[1]`.
        //
        // `a.f(b)` is desugared here into the same `Chain` node `a >> f(b)`
        // produces — the two operators mean exactly the same thing and differ
        // only in binding. Nothing downstream needs to know `.` exists.
        let method = just(Token::Dot)
            // Qualified, so an imported function chains like any other:
            // `n.m2h.drums::bass()`.
            .ignore_then(name())
            .then(
                args.clone()
                    .delimited_by(just(Token::ParensOpen), just(Token::ParensClose))
                    .or_not(),
            )
            .map(|(func, args)| Postfix::Method(func, args.unwrap_or_default()));

        let index = expr
            .clone()
            .delimited_by(just(Token::BracketOpen), just(Token::BracketClose))
            .map(Postfix::Index);

        let postfix = atom.foldl(
            choice((method, index)).repeated(),
            |base, step| match step {
                Postfix::Index(index) => Expr::Index {
                    base: Box::new(base),
                    index: Box::new(index),
                },
                Postfix::Method(func, args) => Expr::Chain {
                    lhs: Box::new(base),
                    rhs: Box::new(Expr::Call { func, args }),
                },
            },
        );

        // Above the arithmetic layers, so `'` reaches only the value written
        // after it: `'[2, 3]` is one quoted list, never the start of a quoted
        // sum. It sits over `postfix` rather than over the atom so a name and
        // an index can be quoted too — `'divs`, `'sets[0]`.
        let quoted = just(Token::Quote)
            .repeated()
            .foldr(postfix, |_, rhs| Expr::Quote { expr: Box::new(rhs) });

        let unary = just(Token::Sub)
            .repeated()
            .foldr(quoted, |_, rhs| Expr::Neg { expr: Box::new(rhs) });

        let product = unary.clone().foldl(
            choice((
                just(Token::Mul).to(ProductOp::Mul),
                just(Token::Div).to(ProductOp::Div),
                just(Token::Percent).to(ProductOp::Rem),
            ))
            .then(unary)
            .repeated(),
            |lhs, (op, rhs)| match op {
                ProductOp::Mul => Expr::Mul { lhs: Box::new(lhs), rhs: Box::new(rhs) },
                ProductOp::Div => Expr::Div { lhs: Box::new(lhs), rhs: Box::new(rhs) },
                ProductOp::Rem => Expr::Rem { lhs: Box::new(lhs), rhs: Box::new(rhs) },
            },
        );

        let sum = product.clone().foldl(
            choice((
                just(Token::Ad).to(SumOp::Add),
                just(Token::Sub).to(SumOp::Sub),
            ))
            .then(product)
            .repeated(),
            |lhs, (op, rhs)| match op {
                SumOp::Add => Expr::Add { lhs: Box::new(lhs), rhs: Box::new(rhs) },
                SumOp::Sub => Expr::Sub { lhs: Box::new(lhs), rhs: Box::new(rhs) },
            },
        );

        let compare = sum.clone().foldl(
                choice((
                    just(Token::Le).to(CmpOp::Le),
                    just(Token::Lt).to(CmpOp::Lt),
                    just(Token::Ge).to(CmpOp::Ge),
                    just(Token::Gt).to(CmpOp::Gt),
                    just(Token::EqEq).to(CmpOp::Eq),
                    just(Token::Ne).to(CmpOp::Ne),
                ))
                .then(sum.clone())
                .repeated(),
                |lhs, (op, rhs)| Expr::Cmp { op, lhs: Box::new(lhs), rhs: Box::new(rhs) },
        );

        let range = compare.clone()
            .then(just(Token::DotDotEq).ignore_then(compare.clone()).or_not())
            .map(|(lo, hi)| match hi {
                Some(hi) => Expr::Range { lo: Box::new(lo), hi: Box::new(hi) },
                None => lo,
        });

        let chain = range.clone().foldl(
            just(Token::ShiftRight).ignore_then(compare.clone()).repeated(),
            |lhs, rhs| Expr::Chain { lhs: Box::new(lhs), rhs: Box::new(rhs) }
        );

        chain
    })
}

fn parser<'a, I>() -> impl Parser<'a, I, Vec<SwyncItem>, extra::Err<Rich<'a, Token>>>
where I: ValueInput<'a, Token = Token, Span = SimpleSpan> {

    let param = ident()
        .then(
            just(Token::Assign)
                .ignore_then(expr())
                .or_not()
        )
        .map(|(name, default)| Param { name, default });

    let params = param
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(Token::ParensOpen), just(Token::ParensClose));

    // Two spellings of one thing: `fn kick(f) = sin(f)` for a body that fits on
    // the line, and `fn kick(f) { ... }` for one with statements in it. The
    // braced form is the block expression the `=` form would have been handed,
    // written without the `=` that would introduce it.
    let body = choice((
        just(Token::Assign).ignore_then(expr()),
        block(expr()),
    ));

    let function = just(Token::Function)
        .ignore_then(ident())
        .then(params)
        .then(body)
        .map(|((name, params), body)| SwyncItem::Function { name, params, body });

    let let_item = just(Token::Let)
        .ignore_then(ident())
        .then_ignore(just(Token::Assign))
        .then(expr())
        .map(|(name, value)| SwyncItem::Let { name, value });

    // `enum Scale { major = [0, 2, 4], minor }`.
    //
    // Members separate on a comma *or* a line break, and any number of either.
    // The lexer inserts a terminator inside these braces exactly as it does
    // inside a block — brace depth is not tracked — so a member list written
    // over lines arrives with `Term`s between its entries and a one-line list
    // arrives with commas. Accepting both is what lets the same enum be written
    // either way, which is how every other braced construct in the language
    // already reads.
    let enum_member = ident()
        .then(just(Token::Assign).ignore_then(expr()).or_not())
        .map(|(name, value)| EnumMember { name, value });

    let member_sep = choice((
        just(Token::Comma).ignored(),
        just(Token::Term).ignored(),
    ))
    .repeated()
    .at_least(1);

    let enum_item = just(Token::Enum)
        .ignore_then(ident())
        .then(
            enum_member
                .separated_by(member_sep)
                .allow_leading()
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(Token::BraceOpen), just(Token::BraceClose)),
        )
        .try_map(|(name, members), span| {
            // An enum with no members has no value that could ever be written,
            // so it is a declaration that cannot be used — always a half-typed
            // line rather than an intent.
            if members.is_empty() {
                return Err(Rich::custom(span, "an enum needs at least one member: \
                                               `enum Scale { major, minor }`"));
            }
            if let Some(dup) = first_duplicate(&members) {
                return Err(Rich::custom(span, format!(
                    "`{}` is declared twice in enum `{}` — two members of one enum \
                     cannot share a name, since `{}.{}` could then mean either",
                    dup, name.0, name.0, dup)));
            }
            Ok(SwyncItem::Enum { name, members })
        });

    // `use lib::drums`, `use lib::drums as d`, `use lib::drums::kick`,
    // `use lib::drums::{kick, snare as clap}`, `use lib::drums::*`.
    //
    // The path is read as one repetition rather than as "path, then optional
    // suffix" so nothing ever has to be un-consumed: after each `::` the next
    // thing is either another segment or the braced list that ends the item.
    let use_name = ident()
        .then(kw_as().ignore_then(ident()).or_not())
        .map(|(name, alias)| UseName { name, alias });

    let use_names = use_name
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .at_least(1)
        .collect::<Vec<_>>()
        .delimited_by(just(Token::BraceOpen), just(Token::BraceClose));

    let segment = just(Token::PathSep).ignore_then(choice((
        ident().map(Segment::Name),
        use_names.map(Segment::Names),
    )));

    let use_item = just(Token::Use)
        .ignore_then(ident())
        .then(segment.repeated().collect::<Vec<_>>())
        .then(just(Token::PathGlob).or_not())
        .then(kw_as().ignore_then(ident()).or_not())
        .try_map_with(|(((head, segments), glob), alias), e| {
            let span: SimpleSpan = e.span();
            build_use(head, segments, glob.is_some(), alias, span.start)
                .map(SwyncItem::Use)
                .map_err(|message| Rich::custom(span, message))
        });

    let item = choice((
        use_item,
        function,
        enum_item,
        expr().map(SwyncItem::Expr),
        let_item,
    ));

    just(Token::Term)
        .repeated()
        .ignore_then(item)
        .then_ignore(just(Token::Term).repeated())
        .repeated()
        .collect::<Vec<_>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(name: &str) -> Box<Expr> {
        Box::new(Expr::Var(Ident(name.to_string())))
    }

    /// `fn add(a, b) = a + b` — a function whose body adds its two parameters.
    #[test]
    fn parses_function_adding_two_vars() {
        let ast = parse("fn add(a, b) = a + b\n".to_string()).expect("should parse");

        let expected = vec![SwyncItem::Function {
            name: Ident("add".to_string()),
            params: vec![
                Param { name: Ident("a".to_string()), default: None },
                Param { name: Ident("b".to_string()), default: None },
            ],
            body: Expr::Add { lhs: var("a"), rhs: var("b") },
        }];

        assert_eq!(ast, expected);
    }

    /// The same function, braced. The body is the block the `=` form would
    /// have been given explicitly, so it holds one expression and no statements.
    #[test]
    fn parses_a_braced_function_body() {
        let ast = parse("fn add(a, b) { a + b }\n".to_string()).expect("should parse");

        let expected = vec![SwyncItem::Function {
            name: Ident("add".to_string()),
            params: vec![
                Param { name: Ident("a".to_string()), default: None },
                Param { name: Ident("b".to_string()), default: None },
            ],
            body: Expr::Block {
                stmts: Vec::new(),
                tail: Box::new(Expr::Add { lhs: var("a"), rhs: var("b") }),
            },
        }];

        assert_eq!(ast, expected);
    }

    /// The point of the braced form: statements ahead of the value, over lines.
    #[test]
    fn a_braced_body_holds_statements() {
        let ast = parse(
            "fn kick(f) {\n  let a = f * 2\n  sin(a)\n}\n".to_string()
        ).expect("should parse");

        let Some(SwyncItem::Function { body: Expr::Block { stmts, tail }, .. }) = ast.first()
        else {
            panic!("expected a function with a block body, got {ast:?}");
        };
        assert_eq!(stmts.len(), 1, "the `let` should be a statement of the block");
        assert_eq!(**tail, Expr::Call {
            func: Ident("sin".to_string()),
            args: vec![Arg::positional(Expr::Var(Ident("a".to_string())))],
        });
    }

    /// Defaults and the braced body are independent — one must not cost the
    /// other, since both hang off the same `=`.
    #[test]
    fn a_braced_body_still_takes_parameter_defaults() {
        let ast = parse("fn bass(n, cut = 800) { saw(n) }\n".to_string()).expect("should parse");

        let Some(SwyncItem::Function { params, body, .. }) = ast.first() else {
            panic!("expected a function, got {ast:?}");
        };
        assert_eq!(params[1].default, Some(Expr::Num(800.0)));
        assert!(matches!(body, Expr::Block { .. }));
    }

    /// A block is worth its last expression, so there has to be one.
    #[test]
    fn an_empty_braced_body_is_an_error() {
        assert!(parse("fn kick(f) { }\n".to_string()).is_err());
        assert!(parse("fn kick(f) { let a = 1 }\n".to_string()).is_err());
    }

    /// Both forms mean the same thing to everything downstream — which is what
    /// lets the body be a block without anything else knowing.
    #[test]
    fn both_function_forms_reach_the_same_value() {
        let one_line = parse("fn f(a) = a * 2\nsin(f(4))\n".to_string()).expect("should parse");
        let braced = parse("fn f(a) { a * 2 }\nsin(f(4))\n".to_string()).expect("should parse");

        let graph_of = |ast| crate::lowerer::lower::lower_graph(&ast).expect("lower failed");
        assert_eq!(graph_of(one_line).nodes, graph_of(braced).nodes);
    }

    /// The one argument list, parsed both ways at once.
    #[test]
    fn parses_named_and_positional_arguments() {
        let ast = parse("play(kick, cut: 400)\n".to_string()).expect("should parse");

        let expected = vec![SwyncItem::Expr(Expr::Call {
            func: Ident("play".to_string()),
            args: vec![
                Arg::positional(Expr::Var(Ident("kick".to_string()))),
                Arg::named("cut", Expr::Num(400.0)),
            ],
        })];

        assert_eq!(ast, expected);
    }

    /// The quote wraps the value written after it and stops there.
    #[test]
    fn a_quote_marks_the_list_it_precedes() {
        let ast = parse("play(bass, div: '[2, 3])\n".to_string()).expect("should parse");

        let Some(SwyncItem::Expr(Expr::Call { args, .. })) = ast.first() else {
            panic!("expected a call, got {ast:?}");
        };
        assert_eq!(
            args[1].value,
            Expr::Quote { expr: Box::new(Expr::List(vec![
                ListItem::plain(Expr::Num(2.0)),
                ListItem::plain(Expr::Num(3.0)),
            ]))},
        );
    }

    /// It binds tighter than the arithmetic around it: `'[1] * 2` is a quoted
    /// list multiplied, not a quote over the whole product. Nothing sensible
    /// can be done with either, but which one the parser built decides which
    /// error the writer is shown.
    #[test]
    fn a_quote_binds_tighter_than_arithmetic() {
        let ast = parse("'[1] * 2\n".to_string()).expect("should parse");

        let Some(SwyncItem::Expr(Expr::Mul { lhs, .. })) = ast.first() else {
            panic!("expected a product, got {ast:?}");
        };
        assert!(matches!(**lhs, Expr::Quote { .. }), "got {lhs:?}");
    }

    /// A named argument's value is a whole expression, not just a literal —
    /// which is what makes a lane able to be a pattern.
    #[test]
    fn a_named_argument_takes_any_expression() {
        let ast = parse("play(bass, cut: [400, 2000])\n".to_string()).expect("should parse");

        let Some(SwyncItem::Expr(Expr::Call { args, .. })) = ast.first() else {
            panic!("expected a call, got {ast:?}");
        };
        assert_eq!(args[1].name, Some(Ident("cut".to_string())));
        assert_eq!(
            args[1].value,
            Expr::List(vec![
                ListItem::plain(Expr::Num(400.0)),
                ListItem::plain(Expr::Num(2000.0)),
            ]),
        );
    }

    /// `choice` has to rewind after the Ident when no Colon follows, or a bare
    /// variable argument stops parsing.
    #[test]
    fn a_bare_identifier_argument_still_parses() {
        let ast = parse("sin(freq)\n".to_string()).expect("should parse");

        let Some(SwyncItem::Expr(Expr::Call { args, .. })) = ast.first() else {
            panic!("expected a call, got {ast:?}");
        };
        assert_eq!(args, &vec![Arg::positional(Expr::Var(Ident("freq".to_string())))]);
    }

    /// Method sugar shares the argument parser, so names work there too.
    #[test]
    fn method_calls_take_named_arguments() {
        let ast = parse("[220] >> play(bass, cut: 400)\n".to_string()).expect("should parse");

        let Some(SwyncItem::Expr(Expr::Chain { rhs, .. })) = ast.first() else {
            panic!("expected a chain, got {ast:?}");
        };
        let Expr::Call { args, .. } = rhs.as_ref() else {
            panic!("expected a call on the right of the chain");
        };
        assert_eq!(args[1], Arg::named("cut", Expr::Num(400.0)));
    }

    /// The whole point of the diagnostic: the error has to say where it is,
    /// and say it in the source's own spelling rather than the token enum's.
    #[test]
    fn a_parse_error_points_at_the_line_it_is_on() {
        let err = parse("let a = 1\nfn foo(a b) = a\nsin(220)\n".to_string())
            .expect_err("a missing comma should not parse");

        assert_eq!(err.stage, Stage::Parse);
        assert_eq!(err.line, Some(2));
        assert_eq!(err.column, Some(10));
        assert_eq!(err.snippet.as_deref(), Some("fn foo(a b) = a"));
        assert_eq!(err.message, "unexpected `b`, expected `)`, `,` or `=`");
    }

    /// An unclosed bracket holds the statement open, so the error lands on the
    /// line that finally could not continue it — not on the line that opened.
    /// Reported honestly rather than guessed at, since the parser has no idea
    /// which of the two the writer meant.
    #[test]
    fn an_unclosed_call_reports_where_it_gave_up() {
        let err = parse("let a = 1\nsin(440\nlet b = 2\n".to_string())
            .expect_err("an unclosed call should not parse");

        assert_eq!(err.line, Some(3));
        assert!(
            err.message.starts_with("unexpected `let`, expected `)`"),
            "the closing paren should lead the list: {}",
            err.message,
        );
    }

    /// A character the language has no token for stops at that character,
    /// rather than at the start of the file.
    #[test]
    fn an_unknown_character_reports_itself() {
        let err = parse("sin(440)\nlet x = 1 @ 2\n".to_string())
            .expect_err("`@` should not lex");

        assert_eq!(err.stage, Stage::Lex);
        assert_eq!(err.line, Some(2));
        assert_eq!(err.column, Some(11));
        assert!(err.message.contains('@'), "unhelpful message: {}", err.message);
    }

    /// Trailing input must not parse quietly: before this, half a program
    /// could reach the engine with the rest of it dropped on the floor.
    #[test]
    fn a_stray_bracket_is_an_error_rather_than_a_short_program() {
        parse("sin(440)\n]\n".to_string()).expect_err("a stray `]` should not parse");
    }

    // ---- use ----

    fn uses(src: &str) -> Vec<Use> {
        parse(src.to_string())
            .unwrap_or_else(|e| panic!("{src} should parse: {e}"))
            .into_iter()
            .filter_map(|item| match item {
                SwyncItem::Use(u) => Some(u),
                _ => None,
            })
            .collect()
    }

    fn id(name: &str) -> Ident {
        Ident(name.to_string())
    }

    /// The four shapes, and what each of them takes.
    #[test]
    fn parses_every_use_shape() {
        let u = &uses("use drums\n")[0];
        assert_eq!(u.path, vec![id("drums")]);
        assert_eq!(u.tree, UseTree::Plain(None));

        let u = &uses("use lib::drums as d\n")[0];
        assert_eq!(u.path, vec![id("lib"), id("drums")]);
        assert_eq!(u.tree, UseTree::Plain(Some(id("d"))));

        let u = &uses("use lib::drums::{kick, snare as clap}\n")[0];
        assert_eq!(u.path, vec![id("lib"), id("drums")]);
        assert_eq!(
            u.tree,
            UseTree::Names(vec![
                UseName { name: id("kick"), alias: None },
                UseName { name: id("snare"), alias: Some(id("clap")) },
            ])
        );

        let u = &uses("use lib::drums::*\n")[0];
        assert_eq!(u.path, vec![id("lib"), id("drums")]);
        assert_eq!(u.tree, UseTree::Glob);
    }

    /// `::*` is one token so the line it ends is over. Without that, the next
    /// line reads as the right-hand side of a multiplication.
    #[test]
    fn a_glob_ends_its_line() {
        let items = parse("use drums::*\nsin(220)\n".to_string()).expect("should parse");
        assert_eq!(items.len(), 2);
        assert!(matches!(items[1], SwyncItem::Expr(Expr::Call { .. })));
    }

    /// A `use` says where it is, so a module that cannot be found is reported
    /// on the line that asked for it rather than at the top of the file.
    #[test]
    fn a_use_remembers_where_it_was_written() {
        let src = "sin(220)\nuse drums\n";
        let u = &uses(src)[0];
        assert_eq!(&src[u.at..u.at + 3], "use");
    }

    /// Combinations with no meaning are refused where they are written, in the
    /// parser's own words.
    #[test]
    fn nonsense_use_shapes_are_refused() {
        let err = parse("use a::{b}::c\n".to_string()).expect_err("list mid-path");
        assert!(err.message.contains("last part"), "got: {}", err.message);

        let err = parse("use a::* as b\n".to_string()).expect_err("glob and alias");
        assert!(err.message.contains("nothing for `as`"), "got: {}", err.message);
    }

    /// `as` is only a keyword in a `use`. Everywhere else it is a name like any
    /// other, and some file somewhere is already using it as one.
    #[test]
    fn as_is_still_an_ordinary_name() {
        let items = parse("let as = 1\nsin(as)\n".to_string()).expect("should parse");
        assert_eq!(items.len(), 2);
    }

    /// A qualified name is one name, not two: everything downstream looks names
    /// up by string, and `drums::kick` is the string it looks up.
    #[test]
    fn a_qualified_name_is_one_name() {
        let items = parse("play([50], drums::kick)\n".to_string()).expect("should parse");
        let Some(SwyncItem::Expr(Expr::Call { args, .. })) = items.first() else {
            panic!("expected a call, got {items:?}");
        };
        assert_eq!(args[1].value, Expr::Var(id("drums::kick")));
    }

    /// `for i in 0..=3 { f4;e }` — the `;` a `for` body ends with is lifted off
    /// the block and onto the loop, because it belongs to the element the loop
    /// makes rather than to the expression the body is.
    #[test]
    fn a_for_body_may_carry_a_length() {
        let items = parse("for i in 0..=3 { f4;e }\n".to_string()).expect("should parse");
        let Some(SwyncItem::Expr(Expr::For { body, length, .. })) = items.first() else {
            panic!("expected a for, got {items:?}");
        };
        assert_eq!(
            **body,
            Expr::Block { stmts: Vec::new(), tail: var("f4") },
            "the length should be off the block, not inside it");
        assert_eq!(length.as_deref(), Some(&Expr::Var(id("e"))));
    }

    /// Everywhere else a block can be written, there is no step for a length to
    /// be the length of.
    #[test]
    fn a_length_on_an_ordinary_block_is_refused() {
        let err = parse("fn tone(f) { sin(f);q }\n".to_string())
            .expect_err("a `;` in a function body should not parse");
        assert!(err.message.contains("not building one"), "got: {}", err.message);
    }

    /// Only the value a block answers with is a step. A `;` further up is a
    /// length given to something that is never read as one.
    #[test]
    fn a_length_on_a_statement_is_refused() {
        let err = parse("for i in 1..=2 {\n  i;q\n  i\n}\n".to_string())
            .expect_err("a `;` on a statement should not parse");
        assert!(err.message.contains("its last line"), "got: {}", err.message);
    }

    /// A block that ends in a `let` fails with the parser's own words, which
    /// arrive by a different route than the expected/found errors.
    #[test]
    fn a_custom_parser_error_keeps_its_message() {
        let err = parse("{ let a = 1 }\n".to_string())
            .expect_err("a block ending in a let should not parse");

        assert!(
            err.message.contains("must end in an expression"),
            "lost the parser's message: {}",
            err.message,
        );
    }
}
