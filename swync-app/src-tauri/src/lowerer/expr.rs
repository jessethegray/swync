use std::fmt::format;
use std::rc::Rc;

use crate::swync_graph::environment::{Env, Item, Length, Value};
use crate::swync_graph::ugen_nodes::{NodeId, NodeInput, NodeKind};
use crate::lowerer::lower::Lowerer;
use crate::parser::parser::{CmpOp, Expr, Statement};
use crate::parser::parser::Range;

const MAX_UNROLL: usize = 1024;

impl Lowerer {
    pub fn expr(&mut self, e: &Expr) -> Result<Value, String> {
        match e {
            Expr::Add { lhs, rhs } =>
                self.binop(NodeKind::Add, |a, b| a + b, lhs, rhs),
            
            Expr::Block { stmts , tail } => {
                self.env.push_scope();
                
                let result = (|| {
                    for stmt in stmts {
                        match stmt {
                            Statement::Let { name, value } => {
                                let v = self.expr(value)?;
                                self.env.define(&name.0, v);
                            }
                            Statement::Expr(e) => { self.expr(e)?; }
                        }
                    }
                    self.expr(tail)
                })();

                self.env.pop_scope();
                result
            }
            
            Expr::Call { func, args } => 
                self.call(func, args),
            
            Expr::Chain { lhs , rhs } => {
                // Before the receiver is evaluated, because `Scale` on its own is
                // not a value the way `xs` in `xs.rev` is — it is a type, and
                // evaluating it to hand to `major` as an argument is exactly the
                // reading this is here to prevent. The same reason `play` and
                // `load` intercept in `call_with`: some receivers are read rather
                // than computed.
                if let Some(member) = self.enum_member(lhs, rhs)? {
                    return Ok(member);
                }
                let piped = self.expr(lhs)?;
                match rhs.as_ref() {
                    Expr::Call { func, args } =>
                        self.call_with(func, args, Some(piped)),
                    Expr::Var(func) =>
                        self.call_with(func, &vec![], Some(piped)),
                    _ => Err("right side of chain must be a function call or variable".into())
                }
            }

            Expr::Cmp { op, lhs, rhs } => {
                let a = self.expr(lhs)?;
                let b = self.expr(rhs)?;

                // Two enum members compare as tags: which alternative is this,
                // not what it happens to hold. That is the whole point of the
                // member being opaque — `Section.verse == Section.chorus` has an
                // answer even though neither carries a value, and two members
                // that happened to be given the same number are still two
                // different members.
                if let (Value::Enum { .. }, _) | (_, Value::Enum { .. }) = (&a, &b) {
                    return self.compare_enums(*op, &a, &b);
                }

                let a = self.as_number(a, "comparison")?;
                let b = self.as_number(b, "comparison")?;
                let truth = match op {
                    CmpOp::Lt => a < b,   CmpOp::Le => a <= b,
                    CmpOp::Gt => a > b,   CmpOp::Ge => a >= b,
                    CmpOp::Eq => a == b,  CmpOp::Ne => a != b,
                };
                Ok(Value::Number(if truth { 1.0 } else { 0.0 }))
            }
            
            Expr::Div { lhs, rhs } =>
                self.binop(NodeKind::Div, |a, b| a / b, lhs, rhs),

            Expr::For { var, iter, body, length } => {
                let items = match self.expr(iter)? {
                    Value::List(items) => items,
                    // A lane hands an instrument the plain list, so a `'` here
                    // is somebody carrying the lane's spelling inside — where
                    // there are no steps for it to be distinguished from.
                    Value::Quoted(_) => return Err(format!(
                        "for {}: `'` marks a list as one value for a `play` lane. \
                         Inside a `fn` the list is already one value, so write it \
                         without the quote", var.0)),
                    _ => return Err(format!(
                        "for {}: expected a list or range to iterate over", var.0)),
                };
                if items.is_empty() {
                    return Err(format!("for {}: nothing to iterate over (empty)", var.0));
                }

                let mut out = Vec::with_capacity(items.len());
                for item in items.iter() {
                    self.env.push_scope();
                    self.env.define(&var.0, item.value.clone());
                    // The length is read inside the loop's scope, like the body
                    // it belongs to, so a step may be as long as the element it
                    // was built from: `for i in 1..=4 { f4;i }` is four notes
                    // each longer than the last.
                    let iteration = self.expr(body).and_then(|value| match length {
                        None => Ok(Item::plain(value)),
                        Some(e) => self.length(e).map(|l| Item { value, length: Some(l) }),
                    });
                    self.env.pop_scope();
                    out.push(iteration?);
                }

                // A `;` describes a step of a sequence, so it means something
                // only where the loop is collecting one. The two other things a
                // loop can be are settled below by what the body produced, and
                // neither of them has steps: voices are summed and plays are
                // scheduled, and a length would be silently dropped by both.
                if length.is_some() {
                    if let Some(what) = out.iter().find_map(|item| match item.value {
                        Value::Play { .. } => Some("plays"),
                        _ if signal_node(&item.value).is_some() => Some("audio"),
                        _ => None,
                    }) {
                        return Err(format!(
                            "for {}: a `;` length is how long a step lasts, and this loop \
                             answers with {what} rather than a sequence of steps", var.0));
                    }
                }

                // What the body produced decides what the loop is. A loop over
                // oscillators is voices to be heard at once, so it sums; a loop
                // over values is a list being built, so it collects. Deciding
                // from the values rather than from a keyword is what lets one
                // `for` be both without either having to be spelled specially.
                if out.iter().any(|item| matches!(item.value, Value::Play { .. })) {
                    // Plays are neither summed nor collected: they all happen,
                    // and the loop as a whole finishes when the last does.
                    let mut ends_at = Some(self.play_start);
                    let mut first = usize::MAX;
                    let mut last = 0usize;
                    for item in &out {
                        let Value::Play { ends_at: end, first: f, last: l, .. } = &item.value else {
                            return Err(format!(
                                "for {}: a loop cannot mix plays with other values", var.0));
                        };
                        // One that never stops makes the whole loop never stop,
                        // so nothing may follow it.
                        ends_at = crate::lowerer::play::later_end(ends_at, *end);
                        first = first.min(*f);
                        last = last.max(*l);
                    }
                    return Ok(Value::Play {
                        starts_at: self.play_start,
                        ends_at,
                        first: first.min(last),
                        last,
                        // Like `play_all`: the passes are the loop's own, so
                        // the chain begins here.
                        chain_first: first.min(last),
                        // Like `play_all`: a loop over plays has no one
                        // instrument, unless it went round exactly once.
                        template: (last == first + 1).then_some(first),
                    });
                }

                if out.iter().any(|item| signal_node(&item.value).is_some()) {
                    // Any signal at all makes the loop audio: a number among
                    // them is a constant to be added, which is what `combine`
                    // already does.
                    let mut acc: Option<Value> = None;
                    for item in out {
                        acc = Some(match acc {
                            None => item.value,
                            Some(prev) =>
                                self.combine(NodeKind::Add, |a, b| a + b, prev, item.value)?,
                        });
                    }
                    return Ok(acc.expect("non-empty list yields at least one value"));
                }

                Ok(Value::List(Rc::new(out)))
            }

            Expr::If { cond, then, otherwise } => {
                if self.number(cond, "if condition")? != 0.0 {
                    self.expr(then)
                } else {
                    match otherwise {
                        Some(e) => self.expr(e),
                        None => Ok(Value::Number(0.0)),
                    }
                }
            }

            Expr::Quote { expr } => {
                let v = self.expr(expr)?;
                quoted(&v).map(Value::Quoted)
            }

            Expr::Index { base, index } => {
                let base = self.expr(base)?;
                let items = match as_data(&base) {
                    Value::List(items) => items.clone(),
                    _ => return Err(
                        not_this_member(&base, "cannot index a value that is not a list")
                            .unwrap_or_else(||
                                "cannot index a value that is not a list".to_string())),
                };
                let i = self.number(index, "list index")?;
                if i < 0.0 || i.fract() != 0.0 {
                    return Err(format!("list index must be a whole number >= 0, got {i}"));
                }
                items.get(i as usize).map(|it| it.value.clone()).ok_or_else(|| format!(
                    "list index {i} out of bounds (length {})", items.len()))
            }            
            
            // `value` is evaluated before the scope is pushed, so a binding
            // never sees itself: `let a = a * 2 in ...` reads the outer `a`.
            Expr::Let { name, value, body } => {
                let v = self.expr(value)?;
                self.env.push_scope();
                self.env.define(&name.0, v);
                let result = self.expr(body);
                self.env.pop_scope();
                result
            }

            // A `;` length is folded here, alongside the element it belongs to,
            // and checked for being something a length may be at all. Which of
            // the two kinds it turned out to be is carried rather than resolved:
            // what a length goes on to *mean* is not this layer's business — a
            // pattern reads a share as time and a written value as beats, a lane
            // reads either as a count of notes, and `len` reads neither.
            //
            // A list is also where the octave register is open. It inherits
            // whatever the list around it was in, so a group carries the line's
            // octave into itself, and it is put back on the way out — a note
            // written inside a group describes that group, exactly as a written
            // length does, and letting either escape would make a line's
            // meaning turn on a bracket several tokens back.
            Expr::List(items) => {
                let outer = self.octave;
                let result = (|| {
                    let mut vals = Vec::with_capacity(items.len());
                    for item in items.iter() {
                        self.check_note_ambiguity(&item.value)?;
                        let value = self.expr(&item.value)?;
                        // Read for this step, then set from it: a step that
                        // spells an octave is in that octave itself and hands it
                        // to the steps after it.
                        if let Some(octave) = self.spelled_octave(&item.value) {
                            self.octave = Some(octave);
                        }
                        // The length is outside the register on purpose. `;e` is
                        // an eighth note wherever it appears, and there is no
                        // pitch in the length position for the ambiguity below
                        // to be about.
                        let length = match &item.length {
                            None => None,
                            Some(e) => Some(self.length(e)?),
                        };
                        vals.push(Item { value, length });
                    }
                    Ok(Value::List(Rc::new(vals)))
                })();
                self.octave = outer;
                result
            }
            
            // In `lowerer::matching`, which is long enough to be worth its own
            // file — a pattern is a second little language, read in the
            // opposite direction from everything else here.
            Expr::Match { subject, arms } => self.match_expr(subject, arms),

            Expr::Mul { lhs, rhs } =>
                self.binop(NodeKind::Mul, |a, b| a * b, lhs, rhs),
            
            Expr::Neg { expr } => match self.expr(expr)? {
                Value::Number(n) => Ok(Value::Number(-n)),
                v => {
                    let input = self.as_input(v)?;
                    Ok(Value::Signal(self.push_node(NodeKind::Neg, vec![input])))
                }
            }
            
            Expr::Num(n) => Ok(Value::Number(*n)),

            // `load` takes its path syntactically — see `lowerer::sample` — so
            // a string that reaches here is one written somewhere a string
            // cannot go.
            Expr::Str(s) => Err(format!(
                "a string is only meaningful as the path in `{}(\"...\")`, and \"{s}\" \
                 is somewhere else", crate::samples::LOAD)),

            Expr::Rest => Ok(Value::Rest),

            Expr::Trigger => Ok(Value::Trigger),

            Expr::Range { lo, hi } => {
                let lo = self.number(lo, "range start")?;
                let hi = self.number(hi, "range end")?;
                if lo.fract() != 0.0 || hi.fract() != 0.0 {
                    return Err(format!("range bounds must be whole numbers, got {lo}..={hi}"));
                }
                let count = if hi < lo { 0 } else { (hi - lo + 1.0) as usize };
                if count > MAX_UNROLL {
                    return Err(format!(
                        "range {lo}..={hi} expands to {count} items (limit {MAX_UNROLL})"));
                }
                let mut out = Vec::with_capacity(count);
                let mut i = lo;
                while i <= hi { out.push(Value::Number(i)); i += 1.0; }
                Ok(Value::List(Item::all(out)))
            }

            Expr::Rem { lhs, rhs } => {
                let a = self.number(lhs, "%")?;
                let b = self.number(rhs, "%")?;
                Ok(Value::Number(a % b))
            }
            
            Expr::Sub { lhs, rhs } =>
                self.binop(NodeKind::Sub, |a, b| a - b, lhs, rhs),

            // A name the environment does not know may still be a note: `c4`,
            // `as3`, `af1`. Bindings win, so a user `let` or parameter shadows
            // a note name rather than colliding with it.
            //
            // Written values and the tuplet marker resolve the same way and for
            // the same reason. They cannot collide with note names — those
            // require an octave digit — so the order between them is free, and
            // a parameter named `e` still shadows the eighth as it always did.
            Expr::Var(id) => match self.env.lookup(&id.0) {
                Some(v) => Ok(v),
                None if id.0 == crate::lang::TUPLET => Ok(Value::Tuplet),
                None if crate::lang::duration(&id.0).is_some() => Ok(Value::Duration(
                    crate::lang::duration(&id.0).expect("just matched"),
                )),
                None => match crate::lang::note(&id.0) {
                    crate::lang::NoteName::Note { midi, .. } => Ok(Value::Number(midi)),
                    // An octave-less note takes the one the sequence is in, the
                    // same way a step with no `;` takes the length in force. It
                    // is an error rather than a guess where there is no
                    // sequence to have said one: a note on its own has no
                    // register to carry, and reading `a` as some default octave
                    // would make a typo sound instead of failing.
                    crate::lang::NoteName::PitchClass(offset) => match self.octave {
                        Some(octave) => Ok(Value::Number(
                            crate::lang::in_octave(offset, octave))),
                        None => Err(format!(
                            "`{}` is a note without an octave, which only means \
                             something inside a sequence where an earlier note has \
                             given one: `[{}1;q, {}, {}]`. Write `{}1`, `{}4` — any \
                             octave — to say which one you mean here",
                            id.0, id.0, id.0, id.0, id.0, id.0)),
                    },
                    crate::lang::NoteName::OctaveOutOfRange(octave) => Err(format!(
                        "note {} has octave {octave}, outside {}..={}",
                        id.0,
                        crate::lang::MIN_NOTE_OCTAVE,
                        crate::lang::MAX_NOTE_OCTAVE
                    )),
                    crate::lang::NoteName::NotANote => {
                        Err(format!("unbound name: {}", id.0))
                    }
                },
            }

        }
    }

    fn binop(&mut self, kind: NodeKind, fold: fn(f64, f64) -> f64,
             lhs: &Expr, rhs: &Expr) -> Result<Value, String> {
        let l = self.expr(lhs)?;
        let r = self.expr(rhs)?;

        self.combine(kind, fold, l, r)
    }

    pub fn combine(&mut self, kind: NodeKind, fold: fn(f64, f64) -> f64,
               l: Value, r: Value) -> Result<Value, String> {

        // Arithmetic is a place data is wanted, so a member stands for what it
        // holds before either side is looked at. It has to happen here rather
        // than in the arms below: a member left whole would miss the two folding
        // cases and fall through to the node builder, which would quietly turn
        // `Tuning.a / 2` into a divider in the audio graph instead of 220.
        let (l, r) = (into_data(l), into_data(r));

        match (l, r) {
            (Value::Number(a), Value::Number(b)) => Ok(Value::Number(fold(a, b))),
            // A tie: `h + e` is a half held into an eighth. Only addition — the
            // rest of the arithmetic has no reading on a written value, and
            // subtracting one from another is not something notation can draw.
            (Value::Duration(a), Value::Duration(b)) if kind == NodeKind::Add => {
                Ok(Value::Duration(a.add(b)))
            }
            (Value::Duration(a), Value::Duration(b)) => Err(format!(
                "written note values can only be added, which ties them — `{a} + {b}` \
                 is one held into the other. There is no other arithmetic notation \
                 can draw on them")),
            (l, r) => {
                let inputs = vec![self.as_input(l)?, self.as_input(r)?];
                Ok(Value::Signal(self.push_node(kind, inputs)))
            }
        }
    }

    /// The octave a step spells, when it spells one.
    ///
    /// Only a note written as the whole step sets the register. A step that
    /// computes its pitch — `a1 + 12`, `n.oct(1)` — is left out deliberately:
    /// the octave it lands in is not something the text says, so a following
    /// bare note carrying "it" would be carrying an answer nobody wrote down.
    ///
    /// A bound name is not a note, which is the same precedence `Expr::Var`
    /// resolves by: a `let a = 60` shadows the pitch and so cannot open a
    /// register either.
    fn spelled_octave(&self, e: &Expr) -> Option<i32> {
        let Expr::Var(id) = e else { return None };
        if self.env.lookup(&id.0).is_some() {
            return None;
        }
        match crate::lang::note(&id.0) {
            crate::lang::NoteName::Note { octave, .. } => Some(octave),
            _ => None,
        }
    }

    /// Refuse a step that is both a written value and a note, once an octave is
    /// in force.
    ///
    /// There is exactly one such spelling — `e` is the eighth and also the note
    /// E — but the check is written against the tables rather than against the
    /// letter, so a value added to `DURATIONS` cannot quietly shadow a pitch.
    ///
    /// The duration wins by resolution order, which is the reading nobody means
    /// in `[c4;q, e, g]`: an eighth-long hit with no pitch, wedged between two
    /// notes. Refusing costs one error and never changes what an existing piece
    /// sounds like, which is more than can be said for either of the silent
    /// answers.
    fn check_note_ambiguity(&self, e: &Expr) -> Result<(), String> {
        let Expr::Var(id) = e else { return Ok(()) };
        if self.octave.is_none() || self.env.lookup(&id.0).is_some() {
            return Ok(());
        }
        let (Some(_), crate::lang::NoteName::PitchClass(_)) =
            (crate::lang::duration(&id.0), crate::lang::note(&id.0)) else {
            return Ok(());
        };
        Err(format!(
            "`{name}` is both a written note value and the note {upper}, and this \
             sequence is in an octave, so either could be meant here. Write \
             `{name}4` — any octave — for the note, or `\\;{name}` for a hit of \
             that length",
            name = id.0, upper = id.0.to_uppercase()))
    }

    /// What a `;` gave the step in front of it.
    ///
    /// One reading for both places a length can be written — an element of a
    /// list, and the body of a `for` collecting one — so `[f4;e]` and
    /// `for i in 0..=0 { f4;e }` cannot drift apart in what `e` means there.
    fn length(&mut self, e: &Expr) -> Result<Length, String> {
        match self.expr(e)? {
            Value::Number(n) => {
                if !n.is_finite() || n <= 0.0 {
                    return Err(format!("a `;` length must be a positive number, got {n}"));
                }
                Ok(Length::Ratio(n))
            }
            Value::Duration(b) => Ok(Length::Beats(b)),
            Value::Tuplet => Ok(Length::Tuplet),
            _ => Err("a `;` length needs a compile-time number or a written note value, \
                      got a signal".to_string()),
        }
    }

    pub(crate) fn number(&mut self, e: &Expr, what: &str) -> Result<f64, String> {
        let v = self.expr(e)?;
        self.as_number(v, what)
    }

    /// A value as a number, unwrapping an enum member that holds one.
    ///
    /// Split out from [`number`](Lowerer::number) because two callers already
    /// have the value rather than the expression — a comparison, which has to
    /// look at both sides before it knows whether it is comparing tags at all.
    pub(crate) fn as_number(&mut self, v: Value, what: &str) -> Result<f64, String> {
        if let Some(n) = control_number(&v) {
            return Ok(n);
        }
        if let Value::Number(n) = as_data(&v) {
            return Ok(*n);
        }
        Err(not_this_member(&v, &format!("{what} needs a number"))
            .unwrap_or_else(|| format!(
                "{what} needs a compile-time number, got a signal \
                 (use select(gate, a, b) to choose at audio rate)")))
    }

    /// `Scale.major`, when the left of a dot is an enum rather than a value.
    ///
    /// Answers `None` for every other chain, which is nearly all of them: the
    /// shape this recognises is a bare name bound to an enum, and one dot after
    /// it. An indexed or computed receiver — `enums[0].major` — is deliberately
    /// not it. The receiver has to be readable from the text for the same reason
    /// a note only sets the octave when it is written as the whole step: what
    /// this rule turns on should be visible at the place it applies.
    fn enum_member(&self, lhs: &Expr, rhs: &Expr) -> Result<Option<Value>, String> {
        let Expr::Var(ty) = lhs else { return Ok(None) };
        let Some(Value::EnumType(def)) = self.env.lookup(&ty.0) else { return Ok(None) };

        let (name, args) = match rhs {
            Expr::Call { func, args } => (&func.0, args.as_slice()),
            // `Scale >> major`. The pipe and the dot build the same node, so
            // both spellings reach a member; refusing one would be a rule with
            // nothing behind it.
            Expr::Var(func) => (&func.0, &[][..]),
            _ => return Err(format!(
                "`{}` is an enum, so what follows the dot has to name one of its \
                 members: {}", def.written(), def.member_names())),
        };

        let Some(index) = def.member(name) else {
            return Err(format!(
                "enum `{}` has no member `{name}`. It has {}",
                def.written(), def.member_names()));
        };

        // A member is a constant, so `Scale.major(2)` is not a call with the
        // wrong arity — it is a call on something that was never callable, and
        // saying so is more use than counting arguments.
        if !args.is_empty() {
            return Err(format!(
                "`{}.{name}` is an enum member, which is a value rather than \
                 something to call — write it without the arguments",
                def.written()));
        }

        Ok(Some(Value::Enum { def: def.clone(), member: index }))
    }

    /// Compare two enum members, or say why one side cannot be compared.
    ///
    /// Only `==` and `!=`: members are a set, not a scale. They are written in
    /// an order, but it is the order somebody happened to type them in, and
    /// answering `Section.verse < Section.chorus` from it would be inventing a
    /// fact about the music out of a fact about the file.
    ///
    /// Comparing across two enums is refused rather than answered `false`. It is
    /// a mistake every time — no program means to ask whether a section is a
    /// scale — and `false` is the answer that lets it run forever.
    fn compare_enums(&mut self, op: CmpOp, a: &Value, b: &Value) -> Result<Value, String> {
        let (Value::Enum { def: left, member: i }, Value::Enum { def: right, member: j }) = (a, b)
        else {
            let (member, other) = match a {
                Value::Enum { .. } => (a, b),
                _ => (b, a),
            };
            let Value::Enum { def, member: index } = member else {
                unreachable!("one side is an enum member; this is the check for which")
            };
            return Err(format!(
                "`{}.{}` is an enum member and {} is not, so there is nothing to \
                 compare. An enum member is only ever equal to a member of the \
                 same enum",
                def.written(), def.members[*index].name, describe(other)));
        };

        if left.name != right.name {
            return Err(format!(
                "`{}.{}` and `{}.{}` are members of different enums, so the answer \
                 is no in a way that is never what was meant — nothing is both. \
                 Compare two members of one enum",
                left.written(), left.members[*i].name,
                right.written(), right.members[*j].name));
        }

        let same = i == j;
        let truth = match op {
            CmpOp::Eq => same,
            CmpOp::Ne => !same,
            _ => return Err(format!(
                "enum members can be compared with `==` and `!=` and nothing else. \
                 `{}` has an order because its members had to be written in one, \
                 not because one is less than another",
                left.written())),
        };
        Ok(Value::Number(if truth { 1.0 } else { 0.0 }))
    }

    pub fn as_input(&self, v: Value) -> Result<NodeInput, String> {
        // A member holding a number is one here, which is the auto-unwrap
        // reaching the signal graph: `sin(Tuning.a)` is `sin(440)`. A member
        // holding anything else, or nothing, falls through to that value's own
        // refusal below — a list is no more a signal for being named.
        if let Some(Value::Number(n)) = unwrap_enum(&v) {
            return Ok(NodeInput::Const(*n));
        }
        match v {
            Value::Number(n) => Ok(NodeInput::Const(n)),
            Value::Signal(id) => Ok(NodeInput::Node(id)),
            // The ordinary use of a slider, and the one worth having: the node
            // in the graph, read at audio rate, so the drag is heard without
            // anything being compiled again.
            Value::Control { node, .. } => Ok(NodeInput::Node(node)),
            Value::Function(_) => Err("cannot use a function as a signal".into()),
            Value::List(_) => Err("cannot use a list as a signal (iterate it with `for`)".into()),
            Value::Stack(_) => Err(
                "cannot use a stack as a signal (a stack is layered patterns, not audio — \
                 sum oscillators with `+` to hear them at once)".into()),
            Value::Buffer(_) => Err(
                "cannot use a buffer as a signal — read it at a position with \
                 `sample(buffer, position)`".into()),
            Value::Destination(_) => Err(
                "cannot use a MIDI destination as a signal — `midiout(..)` names gear \
                 to send notes to, and nothing comes back from it".into()),
            Value::Source(_) => Err(
                "cannot use a MIDI source as a signal — `midiin(..)` is notes to play, \
                 not a level. For a knob or a wheel, `cc`, `bend` and `aftertouch` \
                 answer with signals".into()),
            Value::Rest => Err("cannot use a rest as a signal (rests belong in patterns)".into()),
            Value::Trigger => Err("cannot use a trigger as a signal (triggers belong in patterns)".into()),
            Value::Duration(b) => Err(format!(
                "cannot use `{b}` as a signal — a written note value says how long a \
                 step lasts, and only means anything inside a pattern")),
            Value::Tuplet => Err(
                "cannot use `t` as a signal — it marks a group inside a pattern as a \
                 tuplet".into()),
            Value::Play { .. } => Err(
                "cannot use a play as a signal (it schedules notes, it is not audio)".into()),
            Value::Rate(_) => Err(
                "cannot use a rate as a signal — `accel` says how fast a pattern runs, \
                 and is only meaningful as `play`'s rate".into()),
            Value::Quoted(_) => Err(
                "cannot use a quoted list as a signal — `'` marks a list as one value \
                 for a `play` lane, and means nothing anywhere else. Write the list \
                 without the quote".into()),
            Value::EnumType(def) => Err(format!(
                "cannot use enum `{}` as a signal — it is the enum itself rather \
                 than one of its members. Write `{}.{}`",
                def.written(), def.written(),
                def.members.first().map_or("x", |m| m.name.as_str()))),
            // Only the members a number could not be reached through: the
            // unwrap above has already answered for those.
            v @ Value::Enum { .. } => Err(
                not_this_member(&v, "a signal needs a number")
                    .expect("just matched an enum member")),
        }
    }
}

/// What an enum member stands for, where something wants data rather than a tag.
///
/// This is the auto-unwrap, and it is a lookup rather than a conversion: the
/// member keeps its identity, and what comes back is the value it was declared
/// with. Every consumer that wants a number or a list goes through here first,
/// which is what makes `61.scale(Scale.major)` read the offsets without the
/// writer unwrapping them by hand.
///
/// `None` covers both "not a member" and "a member holding nothing", because no
/// caller here tells them apart — either way there is no data to read. What
/// separates them is only ever the wording of a refusal, and that is
/// [`not_this_member`]'s business rather than this one's.
pub(crate) fn unwrap_enum(v: &Value) -> Option<&Value> {
    match v {
        Value::Enum { def, member } => def.members[*member].value.as_ref(),
        _ => None,
    }
}

/// The node a value is in the graph, for the two kinds of value that are one.
///
/// A slider *is* a signal wherever a signal goes — that is the whole of what
/// the variant claims — and the claim is only true if the places that ask "is
/// this audio?" by matching the variant ask this instead. Matching
/// [`Value::Signal`] directly is the mistake this exists to stop: it does not
/// fail to compile, it silently drops a slider out of an output or out of a
/// loop's summed audio, which sounds like nothing at all.
///
/// Not every such site should use it. A pattern refuses a signal and *accepts*
/// a slider as the number it stands at, so `to_step` matches the two variants
/// separately and means it.
pub(crate) fn signal_node(v: &Value) -> Option<NodeId> {
    match v {
        Value::Signal(id) => Some(*id),
        Value::Control { node, .. } => Some(*node),
        _ => None,
    }
}

/// Where a slider is standing, for the places that demand a number and can
/// take nothing else.
///
/// `None` for everything else, so a caller that wants a signal never sees this
/// and a caller that wants a number always does. The reading is **baked**: it
/// is the position at the moment the program compiled, and the slot is marked
/// so that the panel can say as much rather than leaving somebody dragging a
/// control that stopped being connected the moment it was written here.
///
/// Deliberately not folded into [`as_data`], which every arithmetic operator
/// runs both its sides through: `slider("level") * 0.5` has to build a
/// multiply in the graph, and a slider that answered as data there would fold
/// it to a constant and go quiet under the finger.
pub(crate) fn control_number(v: &Value) -> Option<f64> {
    let Value::Control { slot, at, .. } = v else { return None };
    crate::controls::mark_baked(*slot);
    Some(*at)
}

/// A value as data: what an enum member stands for, or the value itself.
///
/// The one function every consumer of a number or a list runs its argument
/// through. Keeping the unwrap in one place is what makes the rule statable —
/// a member is its value wherever data is wanted — rather than a list of the
/// positions somebody remembered to handle.
pub(crate) fn as_data(v: &Value) -> &Value {
    unwrap_enum(v).unwrap_or(v)
}

/// An owned [`as_data`], for the callers holding a value rather than a
/// reference to one.
///
/// A tag comes back unchanged, still a member: there is nothing for it to stand
/// for, and whatever wanted data will refuse it in its own words.
pub(crate) fn into_data(v: Value) -> Value {
    match unwrap_enum(&v) {
        Some(inner) => inner.clone(),
        None => v,
    }
}

/// Why this enum member could not be read as the thing that was wanted, or
/// `None` if it is not an enum member at all.
///
/// Callers fall back to the message they had before enums existed, so a value
/// that was already wrong stays wrong in the words it always used. Only the
/// member case is worth new words, and it needs them: the mention reads fine —
/// `Scale.major` is spelled correctly and names a real member — and what is
/// wrong with it is the declaration, elsewhere in the file. A message that did
/// not point there would leave the reader staring at a correct line.
pub(crate) fn not_this_member(v: &Value, wanted: &str) -> Option<String> {
    let Value::Enum { def, member } = v else { return None };
    let member = &def.members[*member];
    let holds = match &member.value {
        Some(value) => describe(value),
        None => "nothing — it is a tag, standing only for itself",
    };
    Some(format!(
        "{wanted}, and `{}.{}` holds {holds}. An enum member stands for whatever \
         it was declared with, so declare it as one — or write the value here \
         instead of the member",
        def.written(), member.name))
}

/// A short noun for a value, for messages that have to say what something is.
///
/// Exhaustive on purpose: a new variant should have to decide what it is called
/// here rather than inherit a catch-all that reads wrongly about it.
pub(crate) fn describe(v: &Value) -> &'static str {
    match v {
        Value::Number(_) => "a number",
        Value::Signal(_) => "a signal",
        Value::Control { .. } => "a slider",
        Value::Function(_) => "a function",
        Value::List(_) => "a list",
        Value::Stack(_) => "a stack",
        Value::Quoted(_) => "a quoted list",
        Value::Buffer(_) => "a buffer",
        Value::Destination(_) => "a MIDI destination",
        Value::Source(_) => "a MIDI source",
        Value::EnumType(_) => "an enum",
        Value::Enum { .. } => "an enum member",
        Value::Rest => "a rest",
        Value::Trigger => "a trigger",
        Value::Duration(_) => "a written note value",
        Value::Tuplet => "the tuplet marker",
        Value::Rate(_) => "a rate",
        Value::Play { .. } => "a play",
    }
}

/// The numbers behind a `'`, or why this value cannot be one.
///
/// Checked here, where the quote is written, rather than left for the lane to
/// discover: a lane holding lists is read one note at a time on the scheduler
/// thread, which has no way to report a bad element except by halting the
/// pattern. A `;` is refused for the same reason it is refused on a plain
/// argument — inside a value there is no sequence for a length to divide, so
/// one written there would silently do nothing.
pub(crate) fn quoted(v: &Value) -> Result<Rc<Vec<f64>>, String> {
    let items = match v {
        Value::List(items) => items,
        Value::Quoted(_) => return Err(
            "`'` on a list that is already quoted: one quote is what marks it as a \
             value, and a second says nothing more".into()),
        _ => return Err(
            "`'` marks a *list* as one value, so it needs a list after it: \
             `'[2, 3]`".into()),
    };
    items
        .iter()
        .map(|item| {
            if item.length.is_some() {
                return Err("`'` makes a list one value, and a `;` divides a sequence \
                            — inside a quoted list there is no sequence for it to \
                            divide".to_string());
            }
            match item.value {
                Value::Number(n) => Ok(n),
                _ => Err("a quoted list holds numbers: `'[2, 3]`. Whatever else it \
                          held would have to reach an instrument one note at a \
                          time, which only numbers can do".to_string()),
            }
        })
        .collect::<Result<Vec<f64>, String>>()
        .map(Rc::new)
}