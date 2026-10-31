//! `match` — one value, several answers, and the one that fits.
//!
//! Decided here rather than in the graph, and that is the whole shape of it:
//! like [`Expr::If`](crate::parser::parser::Expr::If), a `match` is settled
//! while the program is being lowered, so what survives into the audio graph is
//! the arm that won and no trace of the ones that did not. A choice that has to
//! be made at audio rate is `select`, which is a different thing and says so.
//!
//! An arm is read in three passes, and the split is by how much has to be known
//! to ask the question.
//!
//! [`check_pattern`] asks what the text alone settles — that an alternative
//! binds nothing, that a name is not bound twice. [`Lowerer::check_tag_arms`]
//! asks what the *kind* of the subject settles, which is only ever a tag: an
//! enum is the one value here whose alternatives are all known while the
//! program compiles, so it is the only one that can be asked whether the arms
//! cover it and whether each of them could answer at all. Both run over every
//! arm before any is tried, so a mistake in an arm the subject was never going
//! to reach is still a mistake. [`Lowerer::matches`] asks the rest, which needs
//! the value itself: whether this pattern fits *that*.

use std::rc::Rc;

use crate::lowerer::expr::{control_number, describe, signal_node};
use crate::lowerer::lower::Lowerer;
use crate::parser::parser::{binds, Expr, Ident, MatchArm, Pattern};
use crate::swync_graph::environment::{EnumDef, Value};

impl Lowerer {
    pub(crate) fn match_expr(
        &mut self,
        subject: &Expr,
        arms: &[MatchArm],
    ) -> Result<Value, String> {
        // Before the subject is evaluated: these are mistakes in the program
        // rather than facts about the value, and a program that is wrong about
        // its own patterns should say so whichever way the music went.
        for arm in arms {
            check_pattern(&arm.pattern)?;
        }

        let subject = self.subject(subject)?;

        // A tag has a known set of alternatives, so "did you cover them all?"
        // has an answer here and nowhere else in the language. It is asked even
        // though the arm that wins is already known, because the missing member
        // is missing whatever tonight's value happens to be — finding out on
        // the night the bridge is first played is finding out too late.
        if let Value::Enum { def, .. } = &subject {
            let def = def.clone();
            self.check_tag_arms(&def, arms)?;
        }

        for arm in arms {
            let mut bound = Vec::new();
            if !self.matches(&arm.pattern, &subject, &mut bound)? {
                continue;
            }

            // The bindings are in scope for the guard as well as the body. That
            // is the point of a guard — `n if n > 60` has nothing to ask about
            // until `n` means something — and it is why the scope is pushed
            // here rather than after the guard has passed.
            self.env.push_scope();
            for (name, value) in bound {
                self.env.define(&name, value);
            }
            let result = (|| {
                if let Some(guard) = &arm.guard {
                    if self.number(guard, "a `match` guard")? == 0.0 {
                        return Ok(None);
                    }
                }
                self.expr(&arm.body).map(Some)
            })();
            self.env.pop_scope();

            match result? {
                Some(value) => return Ok(value),
                // A guard that said no leaves the arm behind entirely, so a
                // later one may still answer. This is the only way a pattern
                // that fit does not win.
                None => continue,
            }
        }

        Err(no_arm(&subject))
    }

    /// The value being matched on, or why it cannot be one.
    ///
    /// A slider is read as the number it stands at, and a signal is refused —
    /// the same pair of answers `if` gets from `as_number`, and for the same
    /// reason. Arms are chosen once, while the program compiles; a value that
    /// changes after that has not been matched on, it has been sampled.
    fn subject(&mut self, e: &Expr) -> Result<Value, String> {
        let v = self.expr(e)?;
        if let Some(n) = control_number(&v) {
            return Ok(Value::Number(n));
        }
        match signal_node(&v) {
            None => Ok(v),
            Some(_) => Err("a `match` chooses its arm while the program compiles, so \
                            what it matches on has to be known then — and a signal is \
                            not, it is whatever it happens to be a moment from now. \
                            Use select(gate, a, b) to choose at audio rate"
                .to_string()),
        }
    }

    /// Whether one pattern fits one value, collecting what it bound on the way.
    ///
    /// The bindings are collected rather than defined as they are found because
    /// a pattern that fits half way round still does not fit: `[a, 0]` against
    /// `[60, 1]` binds nothing, and an `a` left in scope from the attempt would
    /// be visible to the arm that answers instead.
    fn matches(
        &mut self,
        pattern: &Pattern,
        subject: &Value,
        out: &mut Vec<(String, Value)>,
    ) -> Result<bool, String> {
        match pattern {
            Pattern::Wild => Ok(true),

            Pattern::Name(id) if binds(&id.0) => {
                out.push((id.0.clone(), subject.clone()));
                Ok(true)
            }

            // A note or a written value, spelled here exactly as it is spelled
            // everywhere else in the language. See `parser::binds`.
            Pattern::Name(id) => {
                let literal = self.spelled(id)?;
                same(&literal, subject)
            }

            Pattern::Num(n) => same(&Value::Number(*n), subject),

            // The two steps that carry no value. A bare `\` is a trigger and
            // not a written note value — `q` is a hit *of a length*, and the
            // length is a thing it carries that a `\` does not, so sweeping
            // one up here would match away the difference the pattern was
            // written to see.
            Pattern::Rest => same(&Value::Rest, subject),
            Pattern::Trigger => same(&Value::Trigger, subject),

            Pattern::Member { ty, member } => {
                let index = self.member_index(ty, member)?;
                let Value::Enum { def, member: found } = subject else {
                    return Err(member_against(ty, member, subject));
                };
                // Across two enums the answer is no in a way that is never what
                // was meant — an arm that could not match whatever happened is
                // a line the writer believes does something. The same call
                // `compare_enums` makes about `==`.
                let named = self.enum_def(ty).expect("member_index found it");
                if named.name != def.name {
                    return Err(different_enums(&named, &member.0, def));
                }
                Ok(index == *found)
            }

            Pattern::Range { lo, hi } => {
                if let Value::Enum { def, .. } = subject {
                    return Err(literal_against_enum(def, "a range"));
                }
                let lo = self.number(lo, "the start of a `match` range")?;
                let hi = self.number(hi, "the end of a `match` range")?;
                if hi < lo {
                    return Err(format!(
                        "`{lo}..={hi}` runs backwards, so nothing is inside it and this \
                         arm could never be the one that answers. Write it low to high"));
                }
                match subject {
                    Value::Number(n) => Ok(lo <= *n && *n <= hi),
                    _ => Ok(false),
                }
            }

            Pattern::List(items) => {
                if let Value::Enum { def, .. } = subject {
                    return Err(literal_against_enum(def, "a list"));
                }
                let Value::List(values) = subject else {
                    return Ok(false);
                };
                if values.len() != items.len() {
                    return Ok(false);
                }
                // Left to right, and stopping at the first element that does
                // not fit: what `out` holds after a `false` is discarded by the
                // caller, so a half-matched list leaves nothing behind.
                for (item, value) in items.iter().zip(values.iter()) {
                    if !self.matches(item, &value.value, out)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }

            Pattern::Or(alts) => {
                for alt in alts {
                    if self.matches(alt, subject, out)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }

    /// A name in pattern position that stands for a value rather than binding
    /// one: the tuplet marker, a written note value, or a note.
    ///
    /// The order is [`crate::parser::parser::binds`]'s, because the two have to
    /// agree about which names reach here at all.
    fn spelled(&self, id: &Ident) -> Result<Value, String> {
        if id.0 == crate::lang::TUPLET {
            return Ok(Value::Tuplet);
        }
        if let Some(beats) = crate::lang::duration(&id.0) {
            return Ok(Value::Duration(beats));
        }
        match crate::lang::note(&id.0) {
            crate::lang::NoteName::Note { midi, .. } => Ok(Value::Number(midi)),
            crate::lang::NoteName::OctaveOutOfRange(octave) => Err(format!(
                "`{}` is written like a note but octave {octave} is outside the range \
                 {}..={} that MIDI has notes in",
                id.0, crate::lang::MIN_NOTE_OCTAVE, crate::lang::MAX_NOTE_OCTAVE)),
            // `binds` sent everything else the other way.
            other => unreachable!("a pattern name that binds reached `spelled`: {:?}",
                std::mem::discriminant(&other)),
        }
    }

    /// The enum a `Type.member` pattern names, if the name is one here.
    fn enum_def(&self, ty: &Ident) -> Option<Rc<EnumDef>> {
        match self.env.lookup(&ty.0) {
            Some(Value::EnumType(def)) => Some(def),
            _ => None,
        }
    }

    /// Which member of that enum, or why this pattern names none.
    fn member_index(&self, ty: &Ident, member: &Ident) -> Result<usize, String> {
        let Some(def) = self.enum_def(ty) else {
            return Err(format!(
                "`{}.{}` is a pattern that matches one member of an enum, and `{}` is \
                 not an enum here. A bare name on its own — `{}` — is what binds \
                 whatever is being matched",
                ty.0, member.0, ty.0, member.0));
        };
        def.member(&member.0).ok_or_else(|| format!(
            "enum `{}` has no member `{}`. It has {}",
            def.written(), member.0, def.member_names()))
    }

    /// Every arm of a `match` on a tag, read against the enum it is on.
    ///
    /// One walk asking two things, because a tag is the one value in the
    /// language whose alternatives are all known here — which is what makes
    /// both questions answerable at all, and answerable of the arms that will
    /// never be reached as well as the one that wins.
    ///
    /// The first is that each arm *could* answer: a member of this enum, or a
    /// pattern that fits anything. An arm naming a member of some other enum,
    /// or spelling a number where a tag is, is a line the writer believes does
    /// something and that nothing could ever match.
    ///
    /// The second is that between them the arms leave no member out. Only the
    /// arms that could answer on their own count as cover — a guarded arm is a
    /// *condition* on a member rather than an answer for it, since the guard
    /// may say no and leave the member uncovered again, so
    /// `Section.bridge if fills => …` is not a bridge that has been dealt with.
    fn check_tag_arms(&self, def: &Rc<EnumDef>, arms: &[MatchArm]) -> Result<(), String> {
        let mut covered = vec![false; def.members.len()];
        let mut catch_all = false;

        for arm in arms {
            let answers = arm.guard.is_none();
            for alt in alternatives(&arm.pattern) {
                match alt {
                    Pattern::Wild => catch_all |= answers,
                    Pattern::Name(id) if binds(&id.0) => catch_all |= answers,
                    Pattern::Member { ty, member } => {
                        let index = self.member_index(ty, member)?;
                        let named = self.enum_def(ty).expect("member_index found it");
                        if named.name != def.name {
                            return Err(different_enums(&named, &member.0, def));
                        }
                        if answers {
                            covered[index] = true;
                        }
                    }
                    other => return Err(literal_against_enum(def, describes(other))),
                }
            }
        }

        if catch_all {
            return Ok(());
        }

        let missing: Vec<&str> = def.members.iter().zip(&covered)
            .filter(|(_, done)| !**done)
            .map(|(member, _)| member.name.as_str())
            .collect();

        if missing.is_empty() {
            return Ok(());
        }

        Err(format!(
            "this `match` on `{}` has no arm for {}. An enum is a closed set — every \
             member of it is a thing the music can be — so add the arm, or write \
             `_ => …` for what the ones you have not named are worth",
            def.written(), missing.join(", ")))
    }
}

/// Whether two values of the same kind are the same value.
///
/// Only the kinds a pattern can spell a literal of. Everything else is `false`
/// rather than an error, because a pattern that does not fit is the ordinary
/// outcome — the next arm is what a `match` is for. The exception is a tag,
/// which is refused: an enum member is only ever equal to a member of the same
/// enum, so a literal written against one is an arm that could never answer.
fn same(literal: &Value, subject: &Value) -> Result<bool, String> {
    if let Value::Enum { def, .. } = subject {
        return Err(literal_against_enum(def, describe(literal)));
    }
    Ok(match (literal, subject) {
        (Value::Number(a), Value::Number(b)) => a == b,
        (Value::Duration(a), Value::Duration(b)) => a == b,
        (Value::Tuplet, Value::Tuplet) => true,
        (Value::Rest, Value::Rest) => true,
        (Value::Trigger, Value::Trigger) => true,
        _ => false,
    })
}

/// Across two enums the answer is no in a way that is never what was meant —
/// an arm that could not match whatever happened is a line the writer believes
/// does something. The same call `compare_enums` makes about `==`.
fn different_enums(named: &Rc<EnumDef>, member: &str, on: &Rc<EnumDef>) -> String {
    format!(
        "`{}.{member}` is a member of a different enum from the one being matched, \
         which is `{}`. No value is both, so this arm could never be the one that \
         answers",
        named.written(), on.written())
}

/// A short noun for a pattern, for the message about one written where a tag
/// is. Only the shapes that reach it: a member and a wildcard are the two that
/// belong there.
fn describes(pattern: &Pattern) -> &'static str {
    match pattern {
        Pattern::Num(_) => "a number",
        Pattern::Range { .. } => "a range",
        Pattern::List(_) => "a list",
        Pattern::Name(_) => "a note or a written note value",
        Pattern::Rest => "a rest",
        Pattern::Trigger => "a trigger",
        Pattern::Or(_) => "an alternative",
        Pattern::Wild | Pattern::Member { .. } =>
            unreachable!("both belong on a tag and are handled above"),
    }
}

fn literal_against_enum(def: &Rc<EnumDef>, what: &str) -> String {
    format!(
        "this `match` is on `{}`, which is an enum member, and {what} is not one. A \
         member is only ever equal to a member of the same enum, so write \
         `{}.…` — or `_` for everything you have not named",
        def.written(), def.written())
}

fn member_against(ty: &Ident, member: &Ident, subject: &Value) -> String {
    format!(
        "`{}.{}` matches one member of an enum, and this `match` is on {}. There is \
         nothing to compare: a member is only ever equal to a member of the same enum",
        ty.0, member.0, describe(subject))
}

fn no_arm(subject: &Value) -> String {
    let value = match subject {
        Value::Number(n) => format!("{n}"),
        Value::Enum { def, member } => format!("{}.{}", def.written(), def.members[*member].name),
        other => describe(other).to_string(),
    };
    format!(
        "no arm of this `match` fits {value}. Every value it can be needs an answer, \
         so add the arm that names this one — or `_ => …` for whatever is left over")
}

/// The patterns one arm answers to: the alternatives of an `Or`, or the pattern
/// itself.
fn alternatives(pattern: &Pattern) -> &[Pattern] {
    match pattern {
        Pattern::Or(alts) => alts,
        other => std::slice::from_ref(other),
    }
}

/// What can be known about a pattern from the text alone.
///
/// Two things, and both are about names. An alternative may not bind, because
/// `1 | n` promises the body an `n` that only one of the two ways in can
/// deliver — Rust answers this by making every alternative bind the same names,
/// which is a rule with a lot of grammar behind it and very little music. And a
/// name may not be bound twice in one pattern, because `[a, a]` reads as a
/// claim that the two elements are equal and is not one: it is two bindings,
/// and the second would quietly win.
fn check_pattern(pattern: &Pattern) -> Result<(), String> {
    if let Pattern::Or(alts) = pattern {
        for alt in alts {
            let mut bound = Vec::new();
            alt.bindings(&mut bound);
            if let Some(name) = bound.first() {
                return Err(format!(
                    "`{}` binds inside a `|`, and the arm can only be handed a name \
                     that every alternative gives it. Alternatives are for the values \
                     an arm answers to — `1 | 3 | 5` — so write the name in an arm of \
                     its own, with a guard if it needs one",
                    name.0));
            }
        }
    }

    let mut bound = Vec::new();
    pattern.bindings(&mut bound);
    for (i, name) in bound.iter().enumerate() {
        if bound[..i].iter().any(|earlier| earlier.0 == name.0) {
            return Err(format!(
                "`{}` is bound twice by this pattern. Two bindings of one name is not \
                 a claim that the two places hold the same thing — the second would \
                 simply win — so name them apart and compare them in a guard",
                name.0));
        }
    }

    // Nested patterns are checked as patterns in their own right: an `Or`
    // inside a list is as much an `Or` as one at the top.
    match pattern {
        Pattern::List(items) | Pattern::Or(items) => {
            for item in items {
                check_pattern(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
