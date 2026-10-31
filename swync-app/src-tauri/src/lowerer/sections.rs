//! Arrangement: the family of combinators that chain from a `play`.
//!
//! `.then` is the root of it, and the rest are variations on the one move it
//! makes — lower a section now, at eval time, with `play_start` moved to
//! wherever it should begin. Nothing here is a callback, and the audio thread
//! learns nothing new: it still sees bindings that happen to open later.
//!
//! **A section is an argument that is not evaluated on the way in.** That is
//! the whole reason `call` hands this module raw `Arg`s the way it hands `play`
//! its instrument: a `play` writes its notes at whatever `play_start` says when
//! it runs, so evaluating `.then(playn(...))` before `.then` has moved
//! `play_start` would put those notes at the origin and leave nothing to place.
//! Lowering the argument inside `at_offset` instead is what makes
//! `.then(chorus)` and `.then(playn(...))` the same thing by the same
//! mechanism, rather than two spellings with two implementations. See `place`.
//!
//! The same sentence says why a play *bound to a name* is not a section:
//! `let a = playn(...)` already ran, and already sounds where it was written.
//! The name is a handle for chaining *from* — `a.then(b)` — and never a section
//! to be placed. `place` refuses it rather than lowering nothing.
//!
//! Not every combinator can take a section written out, and the line is drawn
//! by whether it needs to run the section *again*. `then_n` re-lowers its
//! argument once per pass, so it can. `then_each`, `wthen`, `rthen`,
//! `shuffle_then` and `maybe` cannot: their sections arrive inside a list, or
//! are chosen between after the fact, and either way the arm has to be
//! something still runnable when the choice is made. Those keep taking a `fn`,
//! and `function` says so when handed anything else.
//!
//! Three of them break the placing rule, and it is worth knowing which.
//!
//! `.take` and `.stop` reach *backwards*, shortening bindings that are already
//! on the timeline. They are the only things in the language that edit a
//! binding after it was written, which is what lets a bounded pattern decide
//! when an unbounded one gives up.
//!
//! `wthen` (with `rthen` and `maybe`) is the one that genuinely needs the
//! scheduler's help. Everything else settles at eval time; a choice that
//! rerolls cannot, because there is nothing at eval time to reroll *against*.
//! So it writes every arm's bindings, marks them as belonging to one choice,
//! and lets `Patterns::windows` decide which arm sounds each time around. See
//! [`ChoiceGroup`] for why that decision is a hash and not an RNG.

use std::rc::Rc;

use crate::lowerer::lower::Lowerer;
use crate::lowerer::play::{later_end, to_pattern_timed};
use crate::parser::parser::Arg;
use crate::pattern::patterns::{Binding, ChoiceGroup, ChoiceRef};
use crate::swync_graph::environment::{FunctionDef, Value};

pub const THEN: &str = "then";
pub const THEN_AFTER: &str = "then_after";
pub const THEN_N: &str = "then_n";
pub const LOOP: &str = "loop";
pub const THEN_EACH: &str = "then_each";
pub const THEN_FILL: &str = "then_fill";
pub const OVERLAP: &str = "overlap";
pub const WITH: &str = "with";
pub const AT: &str = "at";
pub const SEQ: &str = "seq";
pub const QUANTIZE: &str = "quantize";
pub const WAIT: &str = "wait";
pub const WAITS: &str = "waits";
pub const WAITD: &str = "waitd";
pub const TAKE: &str = "take";
pub const STOP: &str = "stop";
pub const WTHEN: &str = "wthen";
pub const RTHEN: &str = "rthen";
pub const SHUFFLE_THEN: &str = "shuffle_then";
pub const MAYBE: &str = "maybe";

/// Every name this module handles, in one place so `lang` and `call` agree.
pub const SECTION_BUILTINS: &[&str] = &[
    THEN, THEN_AFTER, THEN_N, LOOP, THEN_EACH, THEN_FILL, OVERLAP, WITH, AT,
    SEQ, QUANTIZE, WAIT, WAITS, WAITD, TAKE, STOP, WTHEN, RTHEN, SHUFFLE_THEN,
    MAYBE,
];

/// What a handle covers: where it sits, and which bindings are its own.
struct Section {
    starts_at: f64,
    ends_at: Option<f64>,
    first: usize,
    last: usize,
    /// Where the chain this section belongs to began — see `Value::Play`.
    /// Carried from the receiver by everything that has one, so only `.loop`
    /// ever reads it.
    chain_first: usize,
}

impl Section {
    /// The handle a combinator answers with.
    ///
    /// `template` is set only when the section turned out to be a single
    /// binding, because that is the only case where "the instrument this
    /// section is playing" is a question with one answer — see `then_fill`.
    fn value(&self) -> Value {
        Value::Play {
            starts_at: self.starts_at,
            ends_at: self.ends_at,
            first: self.first,
            last: self.last,
            chain_first: self.chain_first,
            template: (self.last == self.first + 1).then_some(self.first),
        }
    }

    /// A section that begins a chain rather than continuing one: `at` and
    /// `seq`, which take no receiver.
    fn rooted(starts_at: f64, ends_at: Option<f64>, first: usize, last: usize) -> Self {
        Section { starts_at, ends_at, first, last, chain_first: first }
    }

    /// This section, chained onto `prev`.
    fn after(mut self, prev: &Section) -> Self {
        self.chain_first = prev.chain_first;
        self
    }
}

/// A call's parameters, whether the first one arrived by pipe or was written.
///
/// `playn(...).then(x)` and `then(playn(...), x)` are one call spelled two
/// ways, and `lang`'s table describes both with a single parameter list that
/// counts the receiver as parameter 0. Everything here indexes the same way,
/// so a combinator's `place` and `constant` calls can be read straight off its
/// table entry.
struct Params<'a> {
    piped: Option<Value>,
    args: &'a [Arg],
}

impl<'a> Params<'a> {
    /// How many parameters were supplied, counting a piped receiver.
    fn count(&self) -> usize {
        self.args.len() + usize::from(self.piped.is_some())
    }

    /// How many parameters the pipe accounts for: one, or none.
    fn shift(&self) -> usize {
        usize::from(self.piped.is_some())
    }

    /// The expression written for parameter `i`, if it was written at all.
    fn written(&self, i: usize) -> Option<&'a Arg> {
        self.args.get(i.checked_sub(self.shift())?)
    }
}

impl Lowerer {
    pub fn is_section(name: &str) -> bool {
        SECTION_BUILTINS.contains(&name)
    }

    /// Dispatch, with the arguments still unevaluated.
    ///
    /// The combinators that place a section exactly once take the `Arg`s and
    /// lower each part where it belongs — see `place`. Everything else is
    /// handed the values it would have received before, because its arguments
    /// are counts, patterns and lists that mean the same whenever they settle.
    pub fn section_builtin(&mut self, name: &str, args: &[Arg], piped: Option<Value>)
        -> Result<Value, String>
    {
        // Sections take no named arguments, and saying so here keeps the answer
        // the same as when `call` checked it for the whole family at once.
        if args.iter().any(|a| a.name.is_some()) {
            return Err(format!("{name}: named arguments only work on a user `fn`"));
        }
        let p = Params { piped, args };
        match name {
            THEN => self.then(&p),
            THEN_AFTER => self.then_after(&p),
            THEN_N => self.then_n(&p),
            OVERLAP => self.overlap(&p),
            WITH => self.with(&p),
            AT => self.at(&p),
            SEQ => self.seq(&p),
            _ => {
                let mut vals = Vec::with_capacity(p.count());
                if let Some(v) = p.piped.clone() {
                    vals.push(v);
                }
                for arg in args {
                    let v = self.expr(&arg.value)?;
                    vals.push(v);
                }
                self.settled_section(name, &vals)
            }
        }
    }

    /// The combinators whose arguments may be settled on the way in.
    ///
    /// `loop`, `quantize`, the `wait` family, `take` and `stop` take no section
    /// at all. The rest
    /// take one they must be able to run again — per element, per arm, or per
    /// time round — which is exactly what a `play` written out can no longer
    /// be, so they go on wanting a `fn`.
    fn settled_section(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            LOOP => self.loop_(args),
            THEN_EACH => self.then_each(args),
            THEN_FILL => self.then_fill(args),
            QUANTIZE => self.quantize(args),
            WAIT | WAITS | WAITD => self.wait(name, args),
            TAKE => self.take(args),
            STOP => self.stop(args),
            WTHEN => self.wthen(name, args),
            RTHEN => self.wthen(name, args),
            SHUFFLE_THEN => self.shuffle_then(args),
            MAYBE => self.maybe(args),
            _ => Err(format!("{name} is not a section combinator")),
        }
    }

    // ---- the shared moves ----

    /// Run `body` with `play_start` set to `offset`, and report what it wrote.
    ///
    /// This is `.then`'s whole mechanism, factored out: every combinator here
    /// that runs a section is this call with a different `offset`, and the
    /// differences between them are arithmetic on that one number.
    ///
    /// Nested combinators inside `body` are relative to *its* start, so offsets
    /// add up exactly as the nesting reads.
    ///
    /// What the section *is* — a `fn` being inlined or an expression being
    /// lowered — is `body`'s business and makes no difference here, which is
    /// the point: both spellings get their length measured off the bindings
    /// they actually wrote, so neither can drift from the other.
    fn at_offset(
        &mut self,
        who: &str,
        offset: f64,
        body: impl FnOnce(&mut Self) -> Result<Option<Value>, String>,
    ) -> Result<Section, String> {
        if !offset.is_finite() {
            return Err(format!("{who}: cannot start a section at bar {offset}"));
        }
        let outer = self.play_start;
        let first = self.bindings.len();
        self.play_start = offset;
        let result = body(self);
        self.play_start = outer;
        let value = result?;

        // The floor is the offset itself: a section is never over before it
        // began, however little it turned out to write. A binding that never
        // stops makes the whole section never stop, so a further `.then` is
        // refused rather than scheduled at a time that will not arrive.
        let mut ends_at = Some(offset);
        for binding in &self.bindings[first..] {
            ends_at = later_end(ends_at, binding.bars.map(|c| binding.start + c));
        }
        // A section may end later than its last note, and only it knows: that
        // is the whole of `.quantize`, whose padding is a rest nothing was
        // written into. Read off the section's own handle rather than the notes
        // it left behind, or naming a section as a `fn` would quietly shorten
        // it to its last note — and a `.loop` around it would come back early
        // by exactly the rest that was asked for.
        if let Some(Value::Play { ends_at: declared, .. }) = value {
            ends_at = later_end(ends_at, declared);
        }
        Ok(Section::rooted(offset, ends_at, first, self.bindings.len()))
    }

    /// Inline `def` at `offset`.
    fn inline(
        &mut self,
        who: &str,
        def: Rc<FunctionDef>,
        args: Vec<Value>,
        offset: f64,
    ) -> Result<Section, String> {
        self.at_offset(who, offset, |me| me.inline_body(who, def, args))
    }

    /// Inline a section named here, leaving the start where it already is.
    ///
    /// Everything else in this module places a section *somewhere else*, so it
    /// moves `play_start` first. `play_all` is the one combinator that moves
    /// nothing — its parts open where the group does — and this is `place`
    /// with that difference and no other, so the two spellings of a section
    /// stay interchangeable there too. It answers with the handle rather than
    /// a `Section`, which stays private to the combinators that chain.
    pub(super) fn inline_named(&mut self, who: &str, def: Rc<FunctionDef>)
        -> Result<Value, String>
    {
        let start = self.play_start;
        Ok(self.inline(who, def, Vec::new(), start)?.value())
    }

    /// A section that arrived by pipe rather than being written as an argument.
    ///
    /// The same rule `seq` follows, for the same reason: whatever is on the
    /// left settled before this call was reached, so a play written there
    /// already sounds where it was written and there is nothing left to gather
    /// — only a name still names something this call can run.
    pub(super) fn piped_section(&mut self, who: &str, v: Option<&Value>)
        -> Result<Value, String>
    {
        let Some(Value::Function(def)) = v else {
            return Err(format!(
                "{who}: a section piped in from the left must be a `fn` written by name — \
                 `intro.{who}(bassline)`. Written out, it settled before this call could \
                 gather it, so write it among the arguments instead."));
        };
        self.inline_named(who, def.clone())
    }

    /// Place the section written as argument `i`, at `offset`.
    ///
    /// The argument arrives unevaluated, so lowering it *here* — inside the
    /// `play_start` swap — is what puts its notes where the section belongs.
    /// Both spellings then fall out of one match rather than needing separate
    /// machinery:
    ///
    /// - a bare `fn` name evaluates to a function and writes nothing, so it is
    ///   inlined, exactly as it always was;
    /// - a play *written here* has already written its notes at `offset` by the
    ///   time it answers, because that is what `play_start` means, so there is
    ///   nothing left to do.
    ///
    /// The second case is why `.then(playn(...))` is not a fixup of notes
    /// placed at the origin: they are never placed at the origin.
    ///
    /// Which is exactly why the third case is refused. `let a = playn(...)`
    /// sounds where it was written, like every other `play`, and what the name
    /// holds afterwards is a handle to notes already on the timeline — so
    /// `.then(a)` would evaluate to a play that this call did not write, place
    /// nothing, and leave `a` at the origin while reading as though it had been
    /// moved. Lowering it a second time is not on offer either: the notes exist
    /// once, and a name may be used twice. So a handle from elsewhere is named
    /// as the mistake it is, rather than accepted as a section that turns out to
    /// be silent.
    fn place(&mut self, who: &str, what: &str, p: &Params, i: usize, offset: f64)
        -> Result<Section, String>
    {
        // A piped section — `intro.seq(verse)` — was settled before this call
        // could move `play_start`, so it is still only ever a `fn`. Nothing is
        // lost: the written form is the one that wanted placing.
        if i < p.shift() {
            let def = self.function(who, what, p.piped.as_ref())?;
            return self.inline(who, def, Vec::new(), offset);
        }
        let Some(arg) = p.written(i) else {
            return Err(format!("{who}: {what} is missing"));
        };
        let expr = &arg.value;
        // Where this placement's bindings begin — the same index `at_offset`
        // takes, since nothing is written between here and there. A play that
        // answers with anything below it is reaching back to notes written
        // before the section started, which is what tells the two cases apart.
        let mark = self.bindings.len();
        self.at_offset(who, offset, |me| match me.expr(expr)? {
            Value::Function(def) => me.inline_body(who, def, Vec::new()),
            // `chain_first` as well as `first`: a chain hung off an older play,
            // `a.then(playn(...))`, writes its new notes after `a` rather than
            // here, and is reaching back just as plainly.
            //
            // Handed back rather than dropped, for the same reason a `fn`
            // body's is: `.then(playn(...).quantize(4))` asked for a rest at
            // the end, and only the handle knows about it.
            v @ Value::Play { first, chain_first, .. }
                if first >= mark && chain_first >= mark => Ok(Some(v)),
            Value::Play { starts_at, .. } => Err(format!(
                "{who}: {what} is a play that has already been placed — it sounds at bar \
                 {starts_at}, where it was written, and a section is lowered where it is \
                 placed rather than moved there afterwards. Wrap it in a `fn` and name that \
                 here, or write the play out at this call.")),
            _ => Err(format!(
                "{who}: {what} must be either a `fn` written by name, such as `chorus`, \
                 or a play written out, such as `playn(pat, inst, 4)`")),
        })
    }

    /// Lower the section a `trigger` fires, at offset zero, and answer where
    /// its bindings begin.
    ///
    /// `place`'s sibling, and deliberately built out of the same two pieces —
    /// `at_offset` and `inline_body` — so that a `fn` named on a button and the
    /// same `fn` named after a `.then` are lowered by one mechanism and refused
    /// in one set of words. What differs is only the offset and what comes
    /// back: a button has no bar to start at, and the caller wants the slice of
    /// bindings to mark rather than a section to chain from.
    ///
    /// Zero is not a bar here, it is "wherever the press turns out to be" —
    /// see `Binding::button`, which is what re-reads it that way.
    pub(crate) fn place_trigger(&mut self, who: &str, args: &[Arg]) -> Result<usize, String> {
        let Some(arg) = args.first() else {
            return Err(format!("{who}: the section is missing"));
        };
        let expr = &arg.value;
        let mark = self.bindings.len();
        self.at_offset(who, 0.0, |me| match me.expr(expr)? {
            Value::Function(def) => me.inline_body(who, def, Vec::new()),
            // A play written out at the call is as good as a `fn` named, so
            // long as its notes are this section's — the same test `place`
            // makes, and for the same reason.
            v @ Value::Play { first, chain_first, .. }
                if first >= mark && chain_first >= mark => Ok(Some(v)),
            Value::Play { starts_at, .. } => Err(format!(
                "{who}: the section is a play that has already been placed — it sounds at \
                 bar {starts_at}, where it was written, and a button's section is lowered \
                 to be started later rather than moved there afterwards. Wrap it in a `fn` \
                 and name that here.")),
            _ => Err(format!(
                "{who}: the section must be either a `fn` written by name, such as `fill`, \
                 or a play written out, such as `play_once(pat, inst)`")),
        })?;
        Ok(mark)
    }

    /// Parameter `i`, evaluated — for the parts of a combinator that are not
    /// sections and so mean the same whenever they settle.
    fn param(&mut self, p: &Params, i: usize) -> Result<Option<Value>, String> {
        if i < p.shift() {
            return Ok(p.piped.clone());
        }
        match p.written(i) {
            Some(arg) => Ok(Some(self.expr(&arg.value)?)),
            None => Ok(None),
        }
    }

    fn inline_body(
        &mut self,
        who: &str,
        def: Rc<FunctionDef>,
        args: Vec<Value>,
    ) -> Result<Option<Value>, String> {
        let wanted = args.len();
        if def.params.len() != wanted {
            return Err(match wanted {
                0 => format!(
                    "{who}: the section must take no parameters, but it declares {}",
                    def.params.len()),
                _ => format!(
                    "{who}: the section must take {wanted} parameter(s), but it declares {}",
                    def.params.len()),
            });
        }
        // The body's own value, kept rather than dropped: a `fn` ending in
        // `.quantize` or `.take` has said something about where the section
        // ends that its bindings do not record.
        self.apply(who, def, args).map(Some)
    }

    /// The receiver of a chained combinator, when its arguments were settled on
    /// the way in and it therefore arrived at the front of them.
    fn receiver(&self, who: &str, args: &[Value]) -> Result<Section, String> {
        self.receiver_of(who, args.first())
    }

    /// The receiver of a chained combinator, unpacked.
    fn receiver_of(&self, who: &str, v: Option<&Value>) -> Result<Section, String> {
        let Some(Value::Play { starts_at, ends_at, first, last, chain_first, .. }) = v
        else {
            return Err(format!(
                "{who}: the left side must be a play — `playn(...).{who}(...)`"));
        };
        Ok(Section {
            starts_at: *starts_at,
            ends_at: *ends_at,
            first: *first,
            last: *last,
            chain_first: *chain_first,
        })
    }

    /// Where a chain has reached. Refuses the endless case with the reason.
    fn cursor(&self, who: &str, s: &Section) -> Result<f64, String> {
        s.ends_at.ok_or_else(|| format!(
            "{who}: this section never finishes, so nothing could follow it. Bound it \
             with `play_once`, `playn`, `.take(n)` or `.stop()` first."))
    }

    /// A section that must stay runnable, so only a `fn` will do.
    ///
    /// The refusal is worth spelling out, because `.then` accepts a play
    /// written out and these do not. What separates them is that a play has
    /// already happened by the time it is a value, and every caller of this
    /// needs a section it can still run — once per element, once per arm, or
    /// once more each time round.
    fn function<'a>(&self, who: &str, what: &str, v: Option<&'a Value>)
        -> Result<Rc<FunctionDef>, String>
    {
        match v {
            Some(Value::Function(def)) => Ok(def.clone()),
            _ => Err(format!(
                "{who} expects a `fn`: {what} must be a section written by name. Unlike \
                 `{THEN}`, `{who}` runs its section afresh rather than placing it once, so \
                 a play written out is not enough — wrap it in a `fn`.")),
        }
    }

    fn constant(&self, who: &str, what: &str, v: Option<&Value>) -> Result<f64, String> {
        match v {
            Some(Value::Number(n)) if n.is_finite() => Ok(*n),
            _ => Err(format!("{who}: {what} must be a compile-time number")),
        }
    }

    /// Parameter `i` as a compile-time number: the counts and gaps.
    fn constant_param(&mut self, who: &str, what: &str, p: &Params, i: usize)
        -> Result<f64, String>
    {
        let v = self.param(p, i)?;
        self.constant(who, what, v.as_ref())
    }

    /// Parameter `i` as the play a combinator chains from.
    fn receiver_param(&mut self, who: &str, p: &Params, i: usize)
        -> Result<Section, String>
    {
        let v = self.param(p, i)?;
        self.receiver_of(who, v.as_ref())
    }

    /// Shorten every binding of a section so none of it sounds past `cut`.
    ///
    /// The one place a binding is edited after it was written. An unbounded
    /// one is given an end; a bounded one that already stops sooner is left
    /// alone, because a cut is a ceiling and not a length.
    fn cut_at(&mut self, s: &Section, cut: f64) {
        for b in &mut self.bindings[s.first..s.last] {
            let ends = b.bars.map(|c| b.start + c);
            if ends.is_none_or(|e| e > cut) {
                b.bars = Some((cut - b.start).max(0.0));
            }
        }
    }

    // ---- placing a section in time ----

    /// `.then(f)` — run `f`'s bindings once this section has finished.
    ///
    /// The section may be a `fn` named here or a play written out; `place`
    /// lowers either one at the cursor, so the two spell the same music.
    fn then(&mut self, p: &Params) -> Result<Value, String> {
        let s = self.receiver_param(THEN, p, 0)?;
        if p.count() != 2 {
            return Err(format!(
                "{THEN} expects a section to run afterwards: \
                 playn(pat, inst, 4).{THEN}(next)"));
        }
        let at = self.cursor(THEN, &s)?;
        Ok(self.place(THEN, "the section", p, 1, at)?.after(&s).value())
    }

    /// `.then_after(n, f)` — `n` bars of silence, then `f`.
    fn then_after(&mut self, p: &Params) -> Result<Value, String> {
        let s = self.receiver_param(THEN_AFTER, p, 0)?;
        if p.count() != 3 {
            return Err(format!(
                "{THEN_AFTER} expects a gap and a section: \
                 playn(pat, inst, 4).{THEN_AFTER}(2, next)"));
        }
        let gap = self.constant_param(THEN_AFTER, "the gap", p, 1)?;
        if gap < 0.0 {
            return Err(format!(
                "{THEN_AFTER}: a gap cannot be negative — use `.{OVERLAP}` to start early"));
        }
        let at = self.cursor(THEN_AFTER, &s)? + gap;
        Ok(self.place(THEN_AFTER, "the section", p, 2, at)?.after(&s).value())
    }

    /// `.overlap(n, f)` — start `f` `n` bars *before* this section ends.
    ///
    /// The whole of a crossfade, and the reason it is a combinator rather than
    /// a negative gap: the two sections really do sound together for `n`
    /// bars, and the chain carries on from whichever of them ends later.
    fn overlap(&mut self, p: &Params) -> Result<Value, String> {
        let s = self.receiver_param(OVERLAP, p, 0)?;
        if p.count() != 3 {
            return Err(format!(
                "{OVERLAP} expects an overlap and a section: \
                 playn(pat, inst, 4).{OVERLAP}(1, next)"));
        }
        let by = self.constant_param(OVERLAP, "the overlap", p, 1)?;
        if by < 0.0 {
            return Err(format!(
                "{OVERLAP}: an overlap cannot be negative — use `.{THEN_AFTER}` for a gap"));
        }
        let end = self.cursor(OVERLAP, &s)?;
        // Never before the section began: overlapping by more than its whole
        // length would put the newcomer in front of it.
        let at = (end - by).max(s.starts_at);
        let next = self.place(OVERLAP, "the section", p, 2, at)?;
        Ok(Section {
            starts_at: s.starts_at,
            ends_at: later_end(Some(end), next.ends_at),
            first: s.first,
            last: next.last,
            chain_first: s.chain_first,
        }
        .value())
    }

    /// `.with(f)` — run `f` alongside this section, from where it began.
    ///
    /// `play_all` gathers plays that are already concurrent; this *makes* one
    /// concurrent with a section that has already been placed, which is what
    /// lets `A.with(drums).then(B)` read in the order it happens.
    fn with(&mut self, p: &Params) -> Result<Value, String> {
        let s = self.receiver_param(WITH, p, 0)?;
        if p.count() != 2 {
            return Err(format!(
                "{WITH} expects a section to run alongside: \
                 playn(pat, inst, 4).{WITH}(drums)"));
        }
        let alongside = self.place(WITH, "the section", p, 1, s.starts_at)?;
        Ok(Section {
            starts_at: s.starts_at,
            ends_at: later_end(s.ends_at, alongside.ends_at),
            first: s.first,
            last: alongside.last,
            chain_first: s.chain_first,
        }
        .value())
    }

    /// `at(n, f)` — place a section at bar `n`, counted from the origin.
    ///
    /// The escape hatch from chaining: an arrangement you already know the
    /// shape of is often clearer written down than derived one `.then` at a
    /// time.
    fn at(&mut self, p: &Params) -> Result<Value, String> {
        if p.count() != 2 {
            return Err(format!("{AT} expects a bar and a section: {AT}(8, chorus)"));
        }
        let when = self.constant_param(AT, "the bar", p, 0)?;
        if when < 0.0 {
            return Err(format!("{AT}: a bar cannot be negative, got {when}"));
        }
        Ok(self.place(AT, "the section", p, 1, when)?.value())
    }

    /// `seq(a, b, c)` — sections one after another, without the nesting.
    fn seq(&mut self, p: &Params) -> Result<Value, String> {
        if p.count() == 0 {
            return Err(format!("{SEQ} expects at least one section: {SEQ}(intro, verse)"));
        }
        let start = self.play_start;
        let first = self.bindings.len();
        let mut at = start;
        for i in 0..p.count() {
            let s = self.place(SEQ, &format!("section {}", i + 1), p, i, at)?;
            at = s.ends_at.ok_or_else(|| format!(
                "{SEQ}: section {} never finishes, so nothing could follow it", i + 1))?;
        }
        Ok(Section::rooted(start, Some(at), first, self.bindings.len()).value())
    }

    // ---- repeating a section ----

    /// `.then_n(f, n)` — run `f` `n` times, back to back.
    ///
    /// Lowered afresh each time round rather than written once and repeated,
    /// which is what makes a `rand` inside `f` a different number on each pass
    /// — the same rule a voice already follows.
    ///
    /// That holds however the section is spelled: the argument is unevaluated
    /// until `place` reaches it, so `.then_n(playn([c4, rand(1, 5)], lead, 1), 3)`
    /// draws three times, exactly as naming the same play in a `fn` would.
    fn then_n(&mut self, p: &Params) -> Result<Value, String> {
        let s = self.receiver_param(THEN_N, p, 0)?;
        if p.count() != 3 {
            return Err(format!(
                "{THEN_N} expects a section and a count: \
                 playn(pat, inst, 4).{THEN_N}(verse, 3)"));
        }
        let times = self.constant_param(THEN_N, "the count", p, 2)?;
        if times.fract() != 0.0 || times < 1.0 {
            return Err(format!(
                "{THEN_N}: the count must be a whole number of at least 1, got {times}"));
        }
        if times > MAX_REPEATS {
            return Err(format!(
                "{THEN_N}: {times} repeats is more than {MAX_REPEATS} — a typo? Each one is \
                 inlined, so the count is a size and not just a duration."));
        }

        let start = self.cursor(THEN_N, &s)?;
        let first = self.bindings.len();
        let mut at = start;
        for i in 0..times as usize {
            let s = self.place(THEN_N, "the section", p, 1, at)?;
            at = s.ends_at.ok_or_else(|| format!(
                "{THEN_N}: pass {} never finishes, so the next could not follow it", i + 1))?;
        }
        Ok(Section::rooted(start, Some(at), first, self.bindings.len()).after(&s).value())
    }

    /// `.loop(n)` — everything chained so far, `n` times through.
    ///
    /// The counterpart to `then_n`, and the difference is which section is
    /// repeated. `then_n` takes a `fn` and runs it `n` times *after* this one;
    /// `loop` takes no section at all, because the section it repeats is the
    /// chain it is written on — `playn(groove, kit, 3).then_fill(roll).loop(4)`
    /// is four bars of groove-and-fill, and there is nowhere else to say that
    /// without naming the pair as a `fn` first.
    ///
    /// Reaching back over the whole chain is why `chain_first` exists. Every
    /// `.then` narrows `first..last` to what it wrote, so that a `.take` after
    /// it cuts what followed rather than what came before; that is right for
    /// cutting and wrong for repeating, and the two need different answers to
    /// "which section".
    ///
    /// The passes are copies of bindings rather than a counter on one, which
    /// keeps the scheduler out of it — the same choice `then_n` makes, and for
    /// the same reason. It does mean a `rand` inside the chain was spent once,
    /// before `loop` ever saw it: every pass is the same music, where
    /// `then_n`'s are each drawn afresh. Where that matters, `then_n` is the
    /// one that wants a `fn`.
    pub fn loop_(&mut self, args: &[Value]) -> Result<Value, String> {
        let s = self.receiver(LOOP, args)?;
        if args.len() != 2 {
            return Err(format!(
                "{LOOP} expects a count: playn(pat, inst, 4).{LOOP}(2)"));
        }
        let times = self.constant(LOOP, "the count", args.get(1))?;
        if times.fract() != 0.0 || times < 1.0 {
            return Err(format!(
                "{LOOP}: the count must be a whole number of at least 1, got {times}"));
        }
        if times > MAX_REPEATS {
            return Err(format!(
                "{LOOP}: {times} passes is more than {MAX_REPEATS} — a typo? Each one is real \
                 bindings, so the count is a size and not just a duration."));
        }

        let end = self.cursor(LOOP, &s)?;
        let range = s.chain_first..s.last;
        if range.is_empty() {
            return Err(format!("{LOOP}: this chain plays nothing, so there is nothing to loop"));
        }

        // Every pass has to be the same length as the first, so anything still
        // open at the end of one would run into the next. `take` and `stop` are
        // how an endless part is given a length; a choice already repeats on a
        // period of its own, and two repetitions of one binding is one too many.
        for b in &self.bindings[range.clone()] {
            if b.bars.is_none() {
                return Err(format!(
                    "{LOOP}: something in this chain never finishes, so a second pass would \
                     play over the first. Bound it with `play_once`, `playn`, `.{TAKE}(n)` or \
                     `.{STOP}()`."));
            }
            if b.choice.is_some() {
                return Err(format!(
                    "{LOOP}: this chain contains a choice, which already comes back around on \
                     a period of its own. Put the choice inside a `fn` and reach for \
                     `.{THEN_N}` instead."));
            }
        }

        // Where the chain opened, which is not `starts_at`: that is the last
        // link's, and the whole chain is what repeats.
        let start = self.bindings[range.clone()]
            .iter()
            .fold(f64::INFINITY, |a, b| a.min(b.start));
        let period = end - start;
        if !(period > 0.0) {
            return Err(format!(
                "{LOOP}: this chain takes no time, so looping it would not go anywhere"));
        }

        let first = s.chain_first;
        for pass in 1..times as usize {
            for i in range.clone() {
                let mut b = self.bindings[i].clone();
                b.start += period * pass as f64;
                self.bindings.push(b);
            }
        }
        Ok(Section {
            starts_at: start,
            ends_at: Some(start + period * times),
            first,
            last: self.bindings.len(),
            // A loop is a chain in its own right: what follows it chains onto
            // the whole loop, so a second `.loop` repeats all of it again.
            chain_first: first,
        }
        .value())
    }

    /// `.then_each(list, f)` — `f(element)` per element, in sequence.
    ///
    /// Arrangement by list, which is the point: every list function in the
    /// language already builds the shape of a piece, and this is what spends
    /// one. `[1, 2, 4, 8].rev` is a ritardando if `f` reads its argument as a
    /// rate.
    pub fn then_each(&mut self, args: &[Value]) -> Result<Value, String> {
        let s = self.receiver(THEN_EACH, args)?;
        if args.len() != 3 {
            return Err(format!(
                "{THEN_EACH} expects a list and a section: \
                 playn(pat, inst, 4).{THEN_EACH}([1, 2, 4], faster)"));
        }
        let Some(Value::List(items)) = args.get(1) else {
            return Err(format!("{THEN_EACH}: expects a list to walk"));
        };
        if items.is_empty() {
            return Err(format!("{THEN_EACH}: the list is empty, so there is nothing to play"));
        }
        let items = items.clone();
        let def = self.function(THEN_EACH, "the section", args.get(2))?;
        if def.params.len() != 1 {
            return Err(format!(
                "{THEN_EACH}: the section must take exactly one parameter — the element — \
                 but it declares {}", def.params.len()));
        }

        let start = self.cursor(THEN_EACH, &s)?;
        let first = self.bindings.len();
        let mut at = start;
        for (i, item) in items.iter().enumerate() {
            let s = self.inline(
                THEN_EACH, def.clone(), vec![item.value.clone()], at)?;
            at = s.ends_at.ok_or_else(|| format!(
                "{THEN_EACH}: element {} never finishes, so the next could not follow it",
                i + 1))?;
        }
        Ok(Section::rooted(start, Some(at), first, self.bindings.len()).after(&s).value())
    }

    /// `.then_fill(pattern, rate?)` — one pass of `pattern` on this section's
    /// own instrument.
    ///
    /// The difference from `.then` is that there is no `fn` and no second
    /// `play`: a fill is played *by* whoever just played, so the instrument
    /// and every lane come from the binding this chains off. That is also why
    /// it needs a receiver that is one play — a group of them has no single
    /// instrument to be a fill for.
    pub fn then_fill(&mut self, args: &[Value]) -> Result<Value, String> {
        let Some(Value::Play { ends_at, template, chain_first, .. }) = args.first() else {
            return Err(format!(
                "{THEN_FILL}: the left side must be a play — `playn(...).{THEN_FILL}(pat)`"));
        };
        let chain_first = *chain_first;
        if args.len() < 2 || args.len() > 3 {
            return Err(format!(
                "{THEN_FILL} expects a pattern and an optional rate: \
                 playn(groove, drums, 4).{THEN_FILL}([1, 1, 1, 1])"));
        }
        let Some(template) = *template else {
            return Err(format!(
                "{THEN_FILL}: a fill is played by the instrument it follows, so the left side \
                 has to be a single `play` — this one covers several, and there is no one \
                 instrument to fill for. Chain the fill onto the play itself."));
        };
        let at = ends_at.ok_or_else(|| format!(
            "{THEN_FILL}: this section never finishes, so a fill could not follow it. Use \
             `play_once` or `playn`."))?;

        let rate = match args.get(2) {
            None => 1.0,
            Some(v) => {
                let r = self.constant(THEN_FILL, "the rate", Some(v))?;
                if !(r > 0.0) {
                    return Err(format!(
                        "{THEN_FILL}: rate must be positive and finite, got {r}"));
                }
                r
            }
        };

        // A fill is one pass, and a pass is only a bar when the pattern is
        // written in shares — see `to_pattern_timed`.
        let (mut pattern, pass_bars) = to_pattern_timed(args.get(1).expect("checked above"), self.meter)?;
        if rate != 1.0 {
            pattern = crate::pattern::pattern::Pattern::fast(rate, pattern);
        }
        let span = pass_bars / rate;

        // Everything but the pattern is inherited: the fill is the same voice
        // and the same lanes, playing something else for one pass.
        let source = &self.bindings[template];
        let fill = Binding {
            target: source.target.clone(),
            source: pattern.into(),
            lanes: source.lanes.clone(),
            start: at,
            bars: Some(span),
            repeat: None,
            choice: None, button: None,
            // A fill's own speed, not the one it is filling for: the pattern
            // here is new, and this is the rate it was given.
            rate: rate.into(),
        };
        let first = self.bindings.len();
        self.bindings.push(fill);
        Ok(Section {
            starts_at: at,
            ends_at: Some(at + span),
            first,
            last: self.bindings.len(),
            chain_first,
        }
        .value())
    }

    // ---- bounding a section ----

    /// `.quantize(n?)` — round where the chain has reached up to a multiple of
    /// `n` bars, without touching what is already playing.
    ///
    /// The cure for a section whose length is not a whole number. `rate`
    /// divides into the count — `playn(pat, inst, 3, 2)` is 1.5 bars — and
    /// without this every `.then` after such a section is permanently off the
    /// downbeat, with nothing in the language able to recover.
    pub fn quantize(&mut self, args: &[Value]) -> Result<Value, String> {
        let s = self.receiver(QUANTIZE, args)?;
        if args.len() > 2 {
            return Err(format!("{QUANTIZE} expects at most a grid: `.{QUANTIZE}(4)`"));
        }
        let grid = match args.get(1) {
            None => 1.0,
            Some(v) => self.constant(QUANTIZE, "the grid", Some(v))?,
        };
        if !(grid > 0.0) {
            return Err(format!("{QUANTIZE}: the grid must be positive, got {grid}"));
        }
        let end = self.cursor(QUANTIZE, &s)?;
        Ok(Section {
            starts_at: s.starts_at,
            // `ceil` of an exact multiple is itself, so a section already on
            // the grid is left where it is rather than pushed a bar out.
            ends_at: Some((end / grid).ceil() * grid),
            first: s.first,
            last: s.last,
            chain_first: s.chain_first,
        }
        .value())
    }

    /// `.wait(n)`, `.waits(n)`, `.waitd(v)` — hold the chain open for a
    /// stretch of silence before anything is allowed to follow.
    ///
    /// `quantize`'s sibling, and built out of exactly the same move: the
    /// cursor is pushed out and the bindings are left alone, so what is
    /// already playing is untouched and only the *next* section is moved.
    /// Where `quantize` rounds the cursor to a grid, this adds a length
    /// somebody wrote down.
    ///
    /// That is the whole difference from `.then_after(n, f)`, which says the
    /// same thing about one join and can only say it with a section in hand.
    /// A wait is a link in the chain, so it reads in the order it happens —
    /// `verse.wait(2).then(chorus)` — and composes with everything else that
    /// chains: `.loop` repeats the gap along with the notes, and `.then_fill`
    /// still knows whose fill it is, because the receiver's bindings are
    /// carried through untouched.
    ///
    /// The three differ only in what unit the length arrives in, and every
    /// one of them is bars by the time it leaves here.
    fn wait(&mut self, who: &str, args: &[Value]) -> Result<Value, String> {
        let s = self.receiver(who, args)?;
        if args.len() != 2 {
            let (what, example) = Self::wait_shape(who);
            return Err(format!("{who} expects {what}: `{example}`"));
        }
        let bars = self.wait_bars(who, args.get(1))?;
        let end = self.cursor(who, &s)?;
        Ok(Section { ends_at: Some(end + bars), ..s }.value())
    }

    /// The length a wait was written as, in bars.
    fn wait_bars(&mut self, who: &str, v: Option<&Value>) -> Result<f64, String> {
        match who {
            WAIT => {
                let n = self.constant(who, "the length", v)?;
                Self::not_negative(who, n)?;
                Ok(n)
            }
            // Seconds against the tempo this eval was lowered at — see
            // `Lowerer::beat_secs` for why that is the honest reading and not
            // a shortcoming. A bar is the beat times the signature, so the
            // same two seconds is one bar in 4/4 and three quarters of one in
            // 3/4, which is what makes this a *musical* length rather than a
            // wall-clock one.
            WAITS => {
                let secs = self.constant(who, "the length in seconds", v)?;
                Self::not_negative(who, secs)?;
                let bar_secs = self.beat_secs * self.meter.quarters_per_bar();
                if !(bar_secs > 0.0) || !bar_secs.is_finite() {
                    return Err(format!(
                        "{WAITS}: there is no tempo here to measure seconds against — \
                         write the gap in bars with `.{WAIT}(n)` instead"));
                }
                Ok(secs / bar_secs)
            }
            // A written note value, exactly as a pattern spells one, which is
            // why it is `Value::Duration` and not a number: `waitd(2)` would
            // read as two of something and the language has two candidates.
            // A number is pointed at the other two names rather than guessed
            // at.
            WAITD => match v {
                Some(Value::Duration(b)) => Ok(b.to_f64() / self.meter.quarters_per_bar()),
                Some(Value::Number(n)) => Err(format!(
                    "{WAITD}: the length must be a written note value such as `q`, `h` or \
                     `q.dot` — {n} is a number, so write `.{WAIT}({n})` for bars or \
                     `.{WAITS}({n})` for seconds"
                )),
                _ => Err(format!(
                    "{WAITD}: the length must be a written note value such as `q`, `h` or \
                     `q.dot`")),
            },
            _ => Err(format!("{who} is not a wait")),
        }
    }

    /// A wait of nothing is harmless and a wait of less than nothing is not:
    /// it would pull the next section back over the one before it, which is
    /// `.overlap`'s job and says so.
    fn not_negative(who: &str, n: f64) -> Result<(), String> {
        match n < 0.0 {
            true => Err(format!(
                "{who}: a wait cannot be negative, got {n} — use `.{OVERLAP}` to start the \
                 next section early")),
            false => Ok(()),
        }
    }

    /// What a wait takes, and how one reads — the pair the arity message has to
    /// name together, so they are answered together and cannot drift.
    fn wait_shape(who: &str) -> (&'static str, &'static str) {
        match who {
            WAITS => ("a length in seconds", "playn(pat, inst, 4).waits(1.5)"),
            WAITD => ("a written note value", "playn(pat, inst, 4).waitd(h)"),
            _ => ("a length in bars", "playn(pat, inst, 4).wait(2)"),
        }
    }

    /// `.take(n)` — this section, cut to `n` bars.
    ///
    /// What gives a plain `play` an end. Until now an unbounded binding could
    /// never be chained from at all; `playn` only fixes that for a single
    /// play, not for a `play_all` group or a whole nested section.
    pub fn take(&mut self, args: &[Value]) -> Result<Value, String> {
        let s = self.receiver(TAKE, args)?;
        if args.len() != 2 {
            return Err(format!("{TAKE} expects a length in bars: `.{TAKE}(8)`"));
        }
        let n = self.constant(TAKE, "the length", args.get(1))?;
        if !(n > 0.0) {
            return Err(format!("{TAKE}: the length must be positive, got {n}"));
        }
        let cut = s.starts_at + n;
        self.cut_at(&s, cut);
        Ok(Section { ends_at: Some(cut), ..s }.value())
    }

    /// `.stop()` — cut everything still open in this section at the moment its
    /// last *bounded* part finishes.
    ///
    /// One pattern as the trigger to stop the rest: put a `playn` next to a
    /// pair of endless `play`s and the counted one decides when all three give
    /// up. Which is why the cut is the latest bounded end rather than the
    /// section's own `ends_at` — that is `None` here by definition, since an
    /// endless binding is exactly what there is to stop.
    pub fn stop(&mut self, args: &[Value]) -> Result<Value, String> {
        let s = self.receiver(STOP, args)?;
        if args.len() != 1 {
            return Err(format!("{STOP} takes no arguments: `play_all(...).{STOP}()`"));
        }
        let cut = self.bindings[s.first..s.last]
            .iter()
            .filter_map(|b| b.bars.map(|c| b.start + c))
            .fold(None::<f64>, |acc, e| Some(acc.map_or(e, |a| a.max(e))));
        let Some(cut) = cut else {
            return Err(format!(
                "{STOP}: nothing in this section ever finishes, so there is no moment to stop \
                 at. `{STOP}` cuts the endless parts where the counted ones run out — give it \
                 at least one `play_once` or `playn`, or say how long with `.{TAKE}(n)`."));
        };
        self.cut_at(&s, cut);
        Ok(Section { ends_at: Some(cut), ..s }.value())
    }

    // ---- choosing between sections ----

    /// `.wthen([a, b], [0.7, 0.3])` and `.rthen([a, b, c])`.
    ///
    /// Every arm is written to the timeline, all of them starting where this
    /// section ends and all of them marked as arms of one choice. Which one
    /// actually sounds is settled per repetition by the scheduler, so the
    /// answer is different each time around — that is the whole point, and the
    /// reason this cannot be an eval-time draw like `choice` is.
    ///
    /// The block repeats forever, because a choice with nowhere to come back
    /// to would be drawn once and never again. `ends_at` is `None` for the
    /// same reason; bound it with `.take(n)` if something should follow.
    pub fn wthen(&mut self, who: &str, args: &[Value]) -> Result<Value, String> {
        let s = self.receiver(who, args)?;
        let uniform = who == RTHEN;
        let wanted = if uniform { 2 } else { 3 };
        if args.len() != wanted && !(uniform && args.len() == 2) {
            return Err(match uniform {
                true => format!(
                    "{RTHEN} expects a list of sections: `.{RTHEN}([verse, chorus, bridge])`"),
                false => format!(
                    "{WTHEN} expects sections and their weights: \
                     `.{WTHEN}([verse, chorus], [0.7, 0.3])`"),
            });
        }

        let Some(Value::List(arms)) = args.get(1) else {
            return Err(format!("{who}: the sections must be given as a list"));
        };
        if arms.is_empty() {
            return Err(format!("{who}: needs at least one section to choose between"));
        }
        let arms = arms.clone();

        let weights = match uniform {
            true => vec![1.0; arms.len()],
            false => {
                let Some(Value::List(ws)) = args.get(2) else {
                    return Err(format!("{WTHEN}: the weights must be given as a list"));
                };
                if ws.len() != arms.len() {
                    return Err(format!(
                        "{WTHEN}: {} sections but {} weights — every section needs one",
                        arms.len(), ws.len()));
                }
                ws.iter()
                    .map(|i| match i.value {
                        Value::Number(n) if n.is_finite() && n >= 0.0 => Ok(n),
                        Value::Number(n) => Err(format!(
                            "{WTHEN}: a weight cannot be negative, got {n}")),
                        _ => Err(format!("{WTHEN}: weights must be compile-time numbers")),
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
        };
        if weights.iter().sum::<f64>() <= 0.0 {
            return Err(format!(
                "{who}: every weight is zero, so no section could ever be chosen"));
        }

        let at = self.cursor(who, &s)?;
        let group = self.choices.len();
        // Reserved before the arms are lowered so a nested choice inside one
        // of them takes a later index rather than this one.
        let seed = self.choice_seed();
        self.choices.push(ChoiceGroup { weights: weights.clone(), seed });

        let first = self.bindings.len();
        let mut period: f64 = 0.0;
        let mut spans = Vec::with_capacity(arms.len());
        for (i, arm) in arms.iter().enumerate() {
            let def = self.function(who, &format!("section {}", i + 1), Some(&arm.value))?;
            let s = self.inline(who, def, Vec::new(), at)?;
            let end = s.ends_at.ok_or_else(|| format!(
                "{who}: section {} never finishes. Every arm of a choice has to have a \
                 length, because the choice is made again each time round and they all \
                 have to come back to the same place.", i + 1))?;
            period = period.max(end - at);
            spans.push((s.first, s.last, i));
        }
        if !(period > 0.0) {
            return Err(format!("{who}: every section is empty, so there is nothing to choose"));
        }

        // Marked after the fact rather than during, so a nested `wthen` inside
        // an arm keeps its own group and only picks up this one as well.
        for (arm_first, arm_last, arm) in spans {
            for b in &mut self.bindings[arm_first..arm_last] {
                b.repeat = Some(period);
                if b.choice.is_none() {
                    b.choice = Some(ChoiceRef { group, arm });
                }
            }
        }

        Ok(Section::rooted(at, None, first, self.bindings.len()).after(&s).value())
    }

    /// `.maybe(p, f)` — `f`, with probability `p`, each time round.
    ///
    /// A `wthen` whose second arm is silence. It repeats and rerolls for the
    /// same reason `wthen` does: a coin flipped once is just an `if`.
    pub fn maybe(&mut self, args: &[Value]) -> Result<Value, String> {
        let s = self.receiver(MAYBE, args)?;
        if args.len() != 3 {
            return Err(format!(
                "{MAYBE} expects a chance and a section: `.{MAYBE}(0.25, fill)`"));
        }
        let p = self.constant(MAYBE, "the chance", args.get(1))?;
        if !(0.0..=1.0).contains(&p) {
            return Err(format!("{MAYBE}: the chance must be between 0 and 1, got {p}"));
        }
        let def = self.function(MAYBE, "the section", args.get(2))?;

        let at = self.cursor(MAYBE, &s)?;
        let group = self.choices.len();
        // Arm 1 is the silent one: it owns no bindings, so drawing it simply
        // means nothing sounds that time round.
        let seed = self.choice_seed();
        self.choices.push(ChoiceGroup { weights: vec![p, 1.0 - p], seed });

        let first = self.bindings.len();
        let played = self.inline(MAYBE, def, Vec::new(), at)?;
        let end = played.ends_at.ok_or_else(|| format!(
            "{MAYBE}: the section never finishes, so there would be nothing to come back \
             from and no second chance to take"))?;
        let period = end - at;
        if !(period > 0.0) {
            return Err(format!("{MAYBE}: the section is empty, so there is nothing to chance"));
        }
        for b in &mut self.bindings[played.first..played.last] {
            b.repeat = Some(period);
            if b.choice.is_none() {
                b.choice = Some(ChoiceRef { group, arm: 0 });
            }
        }
        Ok(Section::rooted(at, None, first, self.bindings.len()).after(&s).value())
    }

    /// `.shuffle_then([a, b, c])` — all of them, once each, in an order drawn
    /// now.
    ///
    /// The counterpart to `wthen` rather than a variant of it: a weighted
    /// choice may pass a section over for a long time, and this one cannot.
    /// It settles at eval time like `scramble`, so it has a length, and a
    /// `.then` may follow it.
    pub fn shuffle_then(&mut self, args: &[Value]) -> Result<Value, String> {
        use rand::seq::SliceRandom;

        let s = self.receiver(SHUFFLE_THEN, args)?;
        if args.len() != 2 {
            return Err(format!(
                "{SHUFFLE_THEN} expects a list of sections: \
                 `.{SHUFFLE_THEN}([verse, chorus, bridge])`"));
        }
        let Some(Value::List(items)) = args.get(1) else {
            return Err(format!("{SHUFFLE_THEN}: the sections must be given as a list"));
        };
        if items.is_empty() {
            return Err(format!("{SHUFFLE_THEN}: needs at least one section"));
        }

        let mut defs = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            defs.push(self.function(
                SHUFFLE_THEN, &format!("section {}", i + 1), Some(&item.value))?);
        }
        defs.shuffle(&mut self.rng);

        let start = self.cursor(SHUFFLE_THEN, &s)?;
        let first = self.bindings.len();
        let mut at = start;
        for (i, def) in defs.into_iter().enumerate() {
            let s = self.inline(SHUFFLE_THEN, def, Vec::new(), at)?;
            at = s.ends_at.ok_or_else(|| format!(
                "{SHUFFLE_THEN}: section {} never finishes, so the next could not follow it",
                i + 1))?;
        }
        Ok(Section::rooted(start, Some(at), first, self.bindings.len()).after(&s).value())
    }

    /// A seed for one choice, drawn from the eval's own RNG — so re-evaluating
    /// deals a new hand, and `seed` pins the whole arrangement along with
    /// everything else it pins.
    fn choice_seed(&mut self) -> u64 {
        use rand::RngExt;
        self.rng.random::<u64>()
    }
}

/// The most passes `then_n` will inline. Each one is real bindings rather than
/// a loop counter, so an absurd count is a memory problem and not just a long
/// piece of music.
const MAX_REPEATS: f64 = 1024.0;
