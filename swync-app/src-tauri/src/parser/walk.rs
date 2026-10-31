//! Every call a program writes, wherever it is written.
//!
//! A syntax walk rather than a lowering pass, and the difference is the whole
//! reason this exists: **an instrument's body is not lowered until the
//! scheduler builds a voice from it**, one note at a time, on a thread with a
//! deadline. Anything that has to be settled before the music starts —
//! decoding a file, giving a slider a slot and telling the panel about it —
//! cannot wait for that. So it is read off the text instead, where a `fn`
//! nobody has played yet is as visible as a line at the top of the file.
//!
//! Two callers want the same walk for that same reason: [`crate::samples`],
//! which asks which files a program names, and [`crate::controls`], which asks
//! which sliders it declares. They shared nothing but a bug risk while the
//! walk was written twice — see the note on [`walk`] about exhaustiveness,
//! which is only worth anything if there is one copy of it to be exhaustive.

use crate::parser::parser::{Arg, Expr, Ident, Pattern, Statement, SwyncItem};

/// Visit every call in a program, in the order they are written.
///
/// The visitor is handed the name being called and the arguments as written —
/// unevaluated, because at this point nothing has been evaluated and that is
/// the point. A caller that needs a literal reads one off the syntax; a caller
/// that needs a computed value has come to the wrong pass.
///
/// Nested calls come through as well as the call containing them, since a
/// `load` or a `slider` is as real inside an argument as anywhere else.
pub(crate) fn calls_in(items: &[SwyncItem], visit: &mut impl FnMut(&Ident, &[Arg])) {
    for item in items {
        match item {
            SwyncItem::Function { body, .. } => walk(body, visit),
            SwyncItem::Let { value, .. } => walk(value, visit),
            SwyncItem::Expr(e) => walk(e, visit),
            SwyncItem::Call { func, args } => {
                visit(func, args);
                for arg in args {
                    walk(&arg.value, visit);
                }
            }
            // A member's value is an ordinary expression, so an enum can name a
            // file: `enum Kit { kick = load("kick.wav") }` is a buffer under a
            // name, and the decode has to be queued from here like any other.
            SwyncItem::Enum { members, .. } => {
                for member in members {
                    if let Some(value) = &member.value {
                        walk(value, visit);
                    }
                }
            }
            // Expanded away before this runs; nothing of it survives to lower.
            SwyncItem::Use(_) => {}
        }
    }
}

/// Visit one expression and everything under it.
///
/// The arms are exhaustive on purpose: a new `Expr` variant that can hold a
/// subexpression must be added here too, and a wildcard would let one be
/// forgotten — which would not fail to compile. It would fail to find a file
/// at runtime, on the audio thread, one note at a time; or leave a slider the
/// program plainly declares missing from the panel.
fn walk(e: &Expr, visit: &mut impl FnMut(&Ident, &[Arg])) {
    match e {
        Expr::Call { func, args } => {
            visit(func, args);
            for arg in args {
                walk(&arg.value, visit);
            }
        }

        Expr::Add { lhs, rhs }
        | Expr::Sub { lhs, rhs }
        | Expr::Mul { lhs, rhs }
        | Expr::Div { lhs, rhs }
        | Expr::Rem { lhs, rhs }
        | Expr::Chain { lhs, rhs }
        | Expr::Cmp { lhs, rhs, .. } => {
            walk(lhs, visit);
            walk(rhs, visit);
        }

        Expr::Block { stmts, tail } => {
            for stmt in stmts {
                match stmt {
                    Statement::Let { value, .. } => walk(value, visit),
                    Statement::Expr(e) => walk(e, visit),
                }
            }
            walk(tail, visit);
        }

        Expr::For { iter, body, length, .. } => {
            walk(iter, visit);
            walk(body, visit);
            // The same rule as a list element's `;`: a call is as findable in
            // the length as in the step it measures.
            if let Some(length) = length {
                walk(length, visit);
            }
        }

        Expr::If { cond, then, otherwise } => {
            walk(cond, visit);
            walk(then, visit);
            if let Some(e) = otherwise {
                walk(e, visit);
            }
        }

        // A range's ends are the only expressions a pattern holds, and a call
        // is as findable in one as anywhere else: `0..=bars()` names a
        // function, and a `load` could just as well be written there.
        Expr::Match { subject, arms } => {
            walk(subject, visit);
            for arm in arms {
                walk_pattern(&arm.pattern, visit);
                if let Some(guard) = &arm.guard {
                    walk(guard, visit);
                }
                walk(&arm.body, visit);
            }
        }

        Expr::Index { base, index } => {
            walk(base, visit);
            walk(index, visit);
        }

        Expr::Let { value, body, .. } => {
            walk(value, visit);
            walk(body, visit);
        }

        Expr::List(items) => {
            // Both halves of an element: a call is as findable in the length a
            // `;` gave a step as it is in the step itself.
            for item in items {
                walk(&item.value, visit);
                if let Some(length) = &item.length {
                    walk(length, visit);
                }
            }
        }

        Expr::Range { lo, hi } => {
            walk(lo, visit);
            walk(hi, visit);
        }

        Expr::Neg { expr } => walk(expr, visit),
        Expr::Quote { expr } => walk(expr, visit),

        // Leaves.
        Expr::Num(_) | Expr::Str(_) | Expr::Rest | Expr::Trigger | Expr::Var(_) => {}
    }
}

/// The expressions inside one pattern, which are its ranges' ends and nothing
/// else.
///
/// Exhaustive for the reason [`walk`] is: a pattern that grows a place to write
/// an expression has to be added here, and a wildcard would hide it.
fn walk_pattern(p: &Pattern, visit: &mut impl FnMut(&Ident, &[Arg])) {
    match p {
        Pattern::Range { lo, hi } => {
            walk(lo, visit);
            walk(hi, visit);
        }
        Pattern::List(items) | Pattern::Or(items) => {
            for item in items {
                walk_pattern(item, visit);
            }
        }
        // Leaves. A `Member`'s two names are reached through the enum rather
        // than called, and a `Name` is being written rather than read.
        Pattern::Wild
        | Pattern::Num(_)
        | Pattern::Name(_)
        | Pattern::Member { .. }
        | Pattern::Rest
        | Pattern::Trigger => {}
    }
}
