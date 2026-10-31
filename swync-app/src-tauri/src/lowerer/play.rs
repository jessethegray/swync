//! `play(pattern, instrument, rate = 1, <lane>: pattern, ...)` — binding a
//! pattern to an instrument.
//!
//! `play` is intercepted before argument evaluation because the instrument
//! must be named syntactically: `Binding` stores a name, and a `Value::Function`
//! has already lost it. Having the definition in hand is also what lets a lane
//! name be checked against the instrument's parameters here, at eval time,
//! rather than failing once per event on the scheduler thread.
//!
//! `play_once` and `playn` are the same binding with an end to it: they differ
//! only in the `bars` the binding carries, which is where the whole of "stop
//! after n passes" lives.
//!
//! `play_all` writes no binding of its own. It is the parallel counterpart to
//! `.then`: several plays that all open together, gathered into the one handle
//! that `.then` can chain from.

use crate::lang::Beats;
use crate::swync_graph::environment::{Item, Length, Source, Value};
use crate::lowerer::expr::control_number;
use crate::lowerer::lower::Lowerer;
use crate::parser::parser::{Arg, Expr, Ident};
use crate::pattern::pattern::{Pattern, Slot, Step, UNIT};
use crate::pattern::patterns::{
    Binding, Lane, LaneValues, SourceOf, Target, MIDI_LANES, RESERVED,
};
use crate::pattern::rate::Rate;
use crate::scheduler::clock::Meter;

/// The one-shot: `play`, stopping after a single pass of the pattern.
pub const PLAY_ONCE: &str = "play_once";
/// The counted one: `playn(pattern, instrument, times, rate = 1)`.
pub const PLAY_N: &str = "playn";
/// A rate that moves: `accel(from, to, bars)`.
pub const ACCEL: &str = "accel";

impl Lowerer {
    /// True when this call should be handled as a `play`.
    pub fn is_play(name: &str) -> bool {
        matches!(name, "play" | PLAY_ONCE | PLAY_N)
    }

    /// True when this call builds a rate rather than a number or a node.
    pub fn is_rate(name: &str) -> bool {
        name == ACCEL
    }

    /// `accel(from, to, bars)` — the only way to write a rate that moves.
    ///
    /// Refused rather than folded: a rate of zero or below is not a slow
    /// pattern but a stopped or reversed one, and a section bounded in passes
    /// would never reach its end to hand on to whatever follows. `Rate::accel`
    /// takes care of the shapes that are merely not curves.
    pub fn rate_builtin(&mut self, args: &[Value]) -> Result<Value, String> {
        let [from, to, bars] = args else {
            return Err(format!("{ACCEL} expects 3 arguments, got {}", args.len()));
        };
        let mut n = [0.0; 3];
        for (slot, (v, what)) in n.iter_mut().zip([
            (from, "from"),
            (to, "to"),
            (bars, "bars"),
        ]) {
            match v {
                Value::Number(x) if x.is_finite() => *slot = *x,
                _ => return Err(format!("{ACCEL}: {what} must be a compile-time number")),
            }
        }
        let [from, to, bars] = n;
        for (r, what) in [(from, "from"), (to, "to")] {
            if r <= 0.0 {
                return Err(format!(
                    "{ACCEL}: {what} must be a positive rate, got {r}"));
            }
        }
        if bars < 0.0 {
            return Err(format!("{ACCEL}: bars cannot be negative, got {bars}"));
        }
        Ok(Value::Rate(Rate::accel(from, to, bars)))
    }

    /// `play(pattern, instrument)` or `play(pattern, instrument, rate)`, plus
    /// any number of trailing named lanes: `play(bass, cut: [400, 2000])`.
    /// When piped (`pat >> play(inst)`) the pattern arrives as `piped` and the
    /// positional arguments shift left by one.
    ///
    /// `name` picks the variant: `play_once` plays one pass, and `playn` takes
    /// its count in the position `rate` would otherwise sit in.
    pub fn play(&mut self, name: &str, args: &[Arg], piped: Option<Value>)
        -> Result<Value, String>
    {
        // Same rule as an ordinary call: interleaving would make it a puzzle
        // which of `play(pat, cut: 400, kick)`'s arguments is the instrument.
        if let Some(first_named) = args.iter().position(|a| a.name.is_some()) {
            if args[first_named..].iter().any(|a| a.name.is_none()) {
                return Err(format!(
                    "{name}: positional arguments must come before named ones"));
            }
        }

        let (positional, named): (Vec<&Arg>, Vec<&Arg>) =
            args.iter().partition(|a| a.name.is_none());
        let positional: Vec<&Expr> = positional.into_iter().map(|a| &a.value).collect();

        let (pattern_value, rest) = match piped {
            Some(p) => (p, positional.as_slice()),
            None => {
                let Some((first, rest)) = positional.split_first() else {
                    return Err(format!("{name} expects a pattern and an instrument"));
                };
                (self.expr(first)?, rest)
            }
        };

        let Some((target_expr, tail)) = rest.split_first() else {
            return Err(format!("{name} expects an instrument name"));
        };

        // Two things can play a pattern, and they are told apart here because
        // this is the last moment the *syntax* is still in hand. An instrument
        // has to be a bare name — `Binding` stores a name and a
        // `Value::Function` has already lost it — while `midiout(..)` is a
        // call, so it is evaluated like any other expression and the two never
        // compete for the same shape.
        let (target, def) = match target_expr {
            Expr::Var(Ident(instrument)) => {
                let Some(Value::Function(def)) = self.env.lookup(instrument) else {
                    return Err(format!("{name}: {instrument} is not a function"));
                };
                (Target::Instrument(instrument.clone()), Some(def))
            }
            _ => match self.expr(target_expr)? {
                Value::Destination(destination) => (Target::Midi(destination), None),
                _ => return Err(format!(
                    "{name}: the instrument must be a plain function name, or a \
                     `midiout(..)` to send the pattern out as MIDI")),
            },
        };
        // Only for the messages below, which name what is being played in
        // order to say what is wrong with it.
        let instrument = &target.label();

        // `playn` takes its count where the others take their rate, so it comes
        // off the front of the tail and everything after it lines up again.
        let (repeats, tail) = match name {
            PLAY_N => {
                let Some((count, tail)) = tail.split_first() else {
                    return Err(format!(
                        "{PLAY_N} expects a number of repeats: {PLAY_N}(pat, {instrument}, 4)"));
                };
                let Value::Number(n) = self.expr(count)? else {
                    return Err(format!("{PLAY_N}: repeats must be a compile-time number"));
                };
                if !(n >= 1.0) || !n.is_finite() {
                    return Err(format!("{PLAY_N}: repeats must be at least 1, got {n}"));
                }
                (Some(n), tail)
            }
            PLAY_ONCE => (Some(1.0), tail),
            _ => (None, tail),
        };

        // A number is one speed throughout; an `accel` is a speed that moves.
        // Both settle here, at eval time — the scheduler is handed a shape it
        // can evaluate itself, never a signal, which it could not read anyway
        // from a lookahead ahead of the audio clock.
        let rate = match tail.first() {
            None => Rate::Fixed(1.0),
            Some(e) => match self.expr(e)? {
                Value::Number(n) => Rate::Fixed(n),
                // A slider is the number it stands at, baked in. The
                // scheduler works a fifth of a second ahead of the audio
                // clock, so a rate it could read while a hand moved would
                // have to be a rate it can see into the future of — which is
                // the same reason a signal is refused below.
                v @ Value::Control { .. } => Rate::Fixed(control_number(&v).unwrap_or_default()),
                Value::Rate(r) => r,
                _ => return Err(format!(
                    "{name}: rate must be a compile-time number or an `accel`")),
            },
        };
        if let Rate::Fixed(r) = rate {
            if !(r > 0.0) || !r.is_finite() {
                return Err(format!("{name}: rate must be positive and finite, got {r}"));
            }
        }
        if tail.len() > 1 {
            // The count `playn` took above is still one of the caller's
            // arguments, so both numbers have to account for it.
            let taken = if name == PLAY_N { 3 } else { 2 };
            return Err(format!(
                "{name} expects at most {} arguments, got {}", taken + 1, tail.len() + taken));
        }

        // The pattern fills the first parameter, so a lane cannot also name it.
        // Every other name must be one the instrument actually declares — the
        // arity check that was missing when a voice took exactly one argument.
        let mut lanes = Vec::with_capacity(named.len());
        for arg in named {
            let lane = arg.name.as_ref().expect("partitioned on name").0.clone();
            match &def {
                // Sending MIDI, where there is no instrument for a lane to
                // reach and so nothing but the fixed set means anything. A
                // name outside it is refused here rather than ignored: a
                // misspelled `vel` that was quietly dropped would be a
                // dynamic nobody could find by reading the program.
                None => {
                    if !MIDI_LANES.iter().any(|(known, _)| *known == lane) {
                        let known: Vec<&str> =
                            MIDI_LANES.iter().map(|(known, _)| *known).collect();
                        return Err(format!(
                            "{name}: {instrument} sends MIDI, which has no parameter \
                             named '{lane}' — only {}", known.join(", ")));
                    }
                }
                Some(def) => match RESERVED.iter().find(|(reserved, _)| *reserved == lane) {
                    // A reserved lane is never passed on, so the instrument must
                    // not be expecting it under that name.
                    Some((_, effect)) => {
                        if def.params.iter().any(|p| p.name.0 == lane) {
                            return Err(format!(
                                "{name}: '{lane}' {effect}, so {instrument} cannot take a \
                                 parameter of that name"));
                        }
                    }
                    None => match def.params.iter().position(|p| p.name.0 == lane) {
                        None => return Err(format!(
                            "{name}: {instrument} has no parameter named '{lane}'")),
                        Some(0) => return Err(format!(
                            "{name}: '{lane}' is {instrument}'s first parameter, which the \
                             pattern itself fills")),
                        Some(_) => {}
                    },
                },
            }
            if lanes.iter().any(|l: &Lane| l.name == lane) {
                return Err(format!("{name}: lane '{lane}' given twice"));
            }

            let value = self.expr(&arg.value)?;

            // A lane of lists is read exactly as a lane of numbers is — the
            // nth note takes the nth value, wrapping — and differs only in
            // what the note is handed. Settled before the pattern conversion
            // below, which would read the same brackets as steps.
            if let Some(lists) = lane_lists(&value) {
                let one_number = RESERVED.iter().chain(if def.is_none() {
                    MIDI_LANES.as_slice()
                } else {
                    [].as_slice()
                });
                if let Some((_, effect)) = one_number.into_iter().find(|(r, _)| *r == lane) {
                    return Err(format!(
                        "{name}: '{lane}' {effect}, which is one number — a quoted \
                         list has nothing to say there"));
                }
                let lists = lists.map_err(|e| format!("{name}: lane '{lane}': {e}"))?;
                lanes.push(Lane { name: lane, values: LaneValues::Lists(lists) });
                continue;
            }

            no_durations(&value).map_err(|b| format!(
                "{name}: lane '{lane}' is written in note values, and `{b}` is a \
                 length of time. A lane is read by note — the nth note takes the \
                 nth value — so how long a step lasts means nothing there; a `;` \
                 in a lane is how many notes the value covers"))?;
            let pattern = to_pattern(&value, self.meter)?;
            whole_lengths(&pattern).map_err(|len| format!(
                "{name}: lane '{lane}' has a length of {len}. A lane is read by note, \
                 not by time — a `;` there is how many notes the value covers, so it \
                 has to be a whole number"))?;
            lanes.push(Lane { name: lane, values: LaneValues::Steps(pattern) });
        }

        // Every parameter the pattern and lanes leave unfilled has to have a
        // default, or the instrument cannot be called at all. A destination
        // has no parameters to leave unfilled — every one of its lanes has a
        // default, because gear that was sent no velocity still has to be
        // sent something.
        for param in def.iter().flat_map(|d| d.params.iter().skip(1)) {
            let filled = lanes.iter().any(|l| l.name == param.name.0);
            if !filled && param.default.is_none() {
                return Err(format!(
                    "{name}: {instrument} needs '{}' — give it a default or pass \
                     `{}: ...` here", param.name.0, param.name.0));
            }
        }

        // A keyboard, rather than notes written down. Everything above this
        // point applied to it unchanged — the target, the lanes, the checks —
        // and everything below it is about laying notes on a grid, which is
        // the one thing a keyboard has no use for.
        if let Value::Source(source) = &pattern_value {
            return self.play_live(name, *source, target, lanes, repeats, rate);
        }

        let (mut pattern, pass_bars) = to_pattern_timed(
            &pattern_value,
            self.meter,
            if repeats.is_some() { Passes::Counted } else { Passes::Endless },
        )?;
        if !rate.is_unit() {
            // Only the pattern. A lane is read by position — the nth note takes
            // the nth value — so it advances with the notes whatever speed they
            // go at, and compressing it too would be compressing it twice.
            pattern = Pattern::Fast(rate, Box::new(pattern));
        }

        // Repeats are passes of the pattern, but the binding is bounded in
        // bars. Two things stand between the two: `rate` has just packed
        // `rate` passes into each bar, and a sequence of written note values
        // was never one bar long to begin with — `[q, q, q]` is three beats,
        // so four passes of it are three bars rather than four.
        //
        // Asking the rate rather than dividing by it is what lets a section
        // that speeds up still have a length: `unphase` is how long it takes to
        // cover that much pattern, which for one speed throughout is the same
        // division as before, and for a curve is the inverse of its integral.
        // Everything an arrangement does with a section rests on this number.
        let bars = repeats.map(|n| rate.unphase(n * pass_bars, 0.0));

        // Anything already sequenced by a `.then` above this call has moved the
        // start; a bare `play` writes at the origin.
        let start = self.play_start;
        let first = self.bindings.len();
        self.bindings.push(Binding {
            target,
            source: pattern.into(),
            lanes,
            start,
            bars,
            // Written once and never chosen between: only `wthen` and `maybe`
            // set these, and they set them on bindings this call already made.
            repeat: None,
            choice: None, button: None,
            // Kept as well as folded in: the pattern above needs it to place
            // its notes, and a voice needs it to know how long a beat is here.
            rate,
        });

        // The handle is what `.then` chains from. It contributes nothing to the
        // output sum, like a bare number.
        Ok(Value::Play {
            starts_at: start,
            ends_at: bars.map(|c| start + c),
            first,
            last: self.bindings.len(),
            // Nothing precedes a `play`, so this is where the chain begins.
            chain_first: first,
            // A single play, so `.then_fill` has exactly one instrument to
            // inherit — the only place a template is ever set.
            template: Some(first),
        })
    }

    /// `play(midiin("keys"), lead)` — a binding whose notes have not happened
    /// yet.
    ///
    /// It is a `Binding` like any other, and deliberately: the target, the
    /// lanes and the handle all mean what they mean for a written pattern, so
    /// `.then` and the rest go on working. What it cannot have is anything
    /// measured in bars, because a keyboard has no length — which is what the
    /// two refusals here are about.
    fn play_live(
        &mut self,
        name: &str,
        source: Source,
        target: Target,
        lanes: Vec<Lane>,
        repeats: Option<f64>,
        rate: Rate,
    ) -> Result<Value, String> {
        // `play_once` and `playn` count passes, and a keyboard has no passes:
        // there is no first note to start one and none to end it. Refused
        // rather than treated as `play`, since what somebody wrote is a
        // section with a length and there is no honest length to give it.
        if repeats.is_some() {
            return Err(format!(
                "{name}: a keyboard has no passes to count — it plays until it is \
                 stopped, so `play` is the only one of these it can take"));
        }
        // Same argument: a rate speeds a pattern up against the bar, and there
        // is no pattern here to speed up. Silently ignoring it would be worse
        // than saying so — the notes would come out at the speed they were
        // played, which is right, while the program says otherwise.
        if !rate.is_unit() {
            return Err(format!(
                "{name}: a keyboard has no rate — the notes arrive when they are \
                 played"));
        }

        let start = self.play_start;
        let first = self.bindings.len();
        self.bindings.push(Binding {
            target,
            source: SourceOf::Live(source),
            lanes,
            start,
            // Never ends on its own. A keyboard is not a section: what stops it
            // is the transport, or the next eval not naming it.
            bars: None,
            repeat: None,
            choice: None, button: None,
            rate: Rate::Fixed(1.0),
        });

        Ok(Value::Play {
            starts_at: start,
            // No end, so nothing can be chained *after* it — `.then` would
            // wait for a moment that never comes. `.with` still works, which
            // is the one that makes sense: playing over a keyboard.
            ends_at: None,
            first,
            last: self.bindings.len(),
            chain_first: first,
            template: Some(first),
        })
    }
}

/// Which of the two rhythms a sequence is written in.
///
/// They are alternatives rather than a spectrum. A share says "twice as long as
/// its neighbour" and needs the whole sequence in view to mean anything; a
/// written value says "one beat" and means it whatever the neighbours do. There
/// is no reading of a sequence holding both that is not a guess, so it is
/// refused.
enum Paradigm {
    /// Slots divide one bar in proportion. Every pattern written before note
    /// values existed, and everything the drawn-pattern panel writes.
    Proportional,
    /// Slots are written note values, and the sequence is as long as they add
    /// up to.
    Metrical,
}

fn paradigm(items: &[Item]) -> Result<Paradigm, String> {
    // The witness is kept rather than a flag, so the error can quote the number
    // that made the sequence ambiguous.
    let mut share: Option<f64> = None;
    let mut written = false;
    for item in items {
        match item.length {
            Some(Length::Ratio(n)) => share = Some(n),
            Some(Length::Beats(_)) | Some(Length::Tuplet) => written = true,
            None => {}
        }
        // A bare `q` is a written value in the step position, so it decides the
        // paradigm exactly as a `;q` does.
        if matches!(item.value, Value::Duration(_)) {
            written = true;
        }
    }
    match (written, share) {
        (true, Some(n)) => Err(format!(
            "a sequence either divides its bar by ratio or counts it in written \
             note values, and this one does both. `;{n}` is a share of whatever \
             else is in the brackets; a written value is a length regardless of \
             them")),
        (true, None) => Ok(Paradigm::Metrical),
        (false, _) => Ok(Paradigm::Proportional),
    }
}

/// One step of a metrical sequence: what sounds, and how long it lasts in beats.
type Part = (Step, Beats);

/// Lower a sequence of written note values, carrying the duration forward.
///
/// `register` is the value in force — set by every explicit `;`, read by every
/// step that gives none. It arrives filled when a tuplet inherits from the
/// sequence around it, and the mutations made here are dropped by that caller
/// rather than returned: a value written inside a tuplet describes that tuplet,
/// and letting it escape would make a line's meaning turn on a bracket several
/// tokens back.
fn metrical_parts(items: &[Item], mut register: Option<Beats>, meter: Meter) -> Result<Vec<Part>, String> {
    let mut parts = Vec::with_capacity(items.len());
    for item in items.iter() {
        // `q` alone is a trigger of that length. A written value carries no
        // pitch, and a trigger is exactly the step that sounds carrying none —
        // so `[q, q, q]` is three quarter-note hits, and a `;` on top of it
        // would be saying the same thing twice.
        let (value, length) = match (&item.value, item.length) {
            (Value::Duration(_), Some(_)) => {
                return Err("a written note value already says how long its step is, \
                            so it cannot take a `;` as well".to_string());
            }
            (Value::Duration(b), None) => (None, Some(Length::Beats(*b))),
            (v, l) => (Some(v), l),
        };

        let group = matches!(value, Some(Value::List(_)) | Some(Value::Stack(_)));

        let (step, beats) = match (length, group) {
            (Some(Length::Tuplet), true) => {
                let (inner, span) = tuplet(value.expect("a group is a value"), register, meter)?;
                (Step::Group(Box::new(inner)), span)
            }
            (Some(Length::Tuplet), false) => {
                return Err("`;t` marks a group as a tuplet, so it needs a bracketed \
                            group in front of it".to_string());
            }
            // The ambiguity the marker exists to settle: three quarters inside a
            // bar could be a bar of their own or a triplet, and nothing about
            // the brackets says which.
            (_, true) => {
                return Err("a group inside a sequence of written note values is a \
                            tuplet, and has to say so — write `;t` after its \
                            closing bracket".to_string());
            }
            (Some(Length::Ratio(n)), _) => {
                return Err(format!(
                    "`;{n}` is a share of the sequence, which cannot be mixed with \
                     written note values"));
            }
            (Some(Length::Beats(b)), _) => {
                register = Some(b);
                (step_of(value, meter)?, b)
            }
            (None, _) => match register {
                Some(b) => (step_of(value, meter)?, b),
                None => {
                    return Err("a sequence of written note values has to say what its \
                                first step is: `[c4;q, e4, g4]` — after that they \
                                carry forward".to_string());
                }
            },
        };
        parts.push((step, beats));
    }
    Ok(parts)
}

/// A step that stated its own value rather than carrying one: the bare `q`
/// case, which sounds and carries nothing.
fn step_of(value: Option<&Value>, meter: Meter) -> Result<Step, String> {
    match value {
        Some(v) => to_step(v, meter),
        None => Ok(Step::Value(1.0)),
    }
}

fn total_beats(parts: &[Part]) -> Beats {
    parts.iter().fold(Beats::whole(0), |sum, (_, b)| sum.add(*b))
}

fn parts_to_steps(parts: Vec<Part>) -> Pattern {
    Pattern::Steps(
        parts.into_iter().map(|(step, b)| Slot::sized(step, b.to_f64())).collect(),
    )
}

/// A `;t` group: the pattern it holds, and the span it is played in.
///
/// The count, the unit and the span all come out of the contents — there is no
/// number on the marker because there is nothing left for one to say. Taking
/// the unit as the gcd rather than as the first value is what admits a tuplet
/// whose opening note fills several of its slots: `[h, q];t` is an ordinary
/// quarter triplet with its first two tied, and reading the base off the `h`
/// would make the count 3/2 and the whole thing nonsense.
///
/// The pattern returned is the bare `Steps`, *not* wrapped in the `Fast` a
/// sequence of this length would carry on its own. A group is given exactly one
/// bar of slot-local time, so only the ratios inside it survive and the span
/// supplies the scale; leaving the `Fast` on would repeat the group a fractional
/// number of times inside its own slot.
fn tuplet(v: &Value, register: Option<Beats>, meter: Meter) -> Result<(Pattern, Beats), String> {
    let Value::List(items) = v else {
        return Err("`;t` marks a bracketed group as a tuplet, and a stack is layers \
                    rather than a sequence — there is nothing in it to count"
            .to_string());
    };
    let parts = metrical_parts(items, register, meter)?;
    if parts.is_empty() {
        return Err("an empty group has nothing for `;t` to make a tuplet of".to_string());
    }

    let total = total_beats(&parts);
    let unit = parts
        .iter()
        .map(|(_, b)| *b)
        .reduce(|a, b| a.gcd(b))
        .expect("just checked the group is not empty");
    let n = total
        .divide_by(unit)
        .expect("the gcd of the parts divides their sum a whole number of times");
    let span = crate::lang::tuplet_span(n);

    // The whole of "what is an acceptable tuplet". A count that is already a
    // power of two would be played in exactly the time it is written, so the
    // contents come to an ordinary duration and the marker is claiming a
    // compression that is not there — `[q, e, e];t` is four eighths, which is a
    // half note however it is bracketed.
    if span == n {
        return Err(format!(
            "this group holds {n} × {unit}, which comes to {total} — an ordinary \
             duration, so `;t` has nothing to compress. A tuplet's contents have to \
             count to something that is not a power of two: three quarters make a \
             triplet, five eighths a quintuplet"));
    }

    Ok((parts_to_steps(parts), unit.times(span)))
}

/// Whether what reads this pattern counts its passes or lets it run.
///
/// It decides exactly one thing, and only for a `stack` whose layers are not
/// all the same length: what a layer does once its own pass is over. There is
/// no answer that is right both times, which is why this has to be asked.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Passes {
    /// `play` — the pattern loops, so every layer loops at its own period and
    /// a short one comes around against a long one. That phasing is what
    /// layers of unequal length are *for*, and is what `stack` has always done.
    Endless,
    /// `play_once`, `playn`, `then_fill` — the binding stops after a count of
    /// passes, and the count has to mean the same thing to every layer. A
    /// stack's pass is as long as its longest layer, so left to loop a shorter
    /// one sounds several times inside a pass that promised one: a chord of an
    /// eighth over a whole note struck its eighth four times under the long
    /// note, which is a cacophony rather than a chord. So a layer is given the
    /// whole of the stack's pass and rests for what it does not fill.
    Counted,
}

/// A layer stretched to rest out the remainder of the stack's pass.
///
/// `layer` runs one pass in `pass` bars and then comes around again, which is
/// what has to stop. `fast(pass, ..)` is that same pass over exactly one bar,
/// so it is a group — a group is given one bar of slot-local time and no more,
/// which is where the looping goes — and the slots either side of it hold the
/// group to `pass` of the `whole` bars and rest the rest. Slowing the pair by
/// `whole` puts it all back in bar time.
fn resting_out(layer: Pattern, pass: f64, whole: f64) -> Pattern {
    if !(whole > pass) || !whole.is_finite() || !(pass > 0.0) {
        return layer;
    }
    let one_pass = Step::Group(Box::new(Pattern::fast(pass, layer)));
    Pattern::fast(1.0 / whole, Pattern::Steps(vec![
        Slot::sized(one_pass, pass),
        Slot::sized(Step::Rest, whole - pass),
    ]))
}

/// Interpret a lowered value rhythmically, and say how long one pass of it
/// takes in bars.
///
/// The pass length is one bar for everything written in shares, which is why
/// nothing needed to ask before written values existed. A metrical sequence is
/// as long as its values add up to, and `playn`, `then` and `then_fill` all
/// count passes while binding in bars — so they need the conversion rather
/// than being able to assume it is one.
pub fn to_pattern_timed(v: &Value, meter: Meter, passes: Passes)
    -> Result<(Pattern, f64), String>
{
    match v {
        Value::List(items) => match paradigm(items)? {
            // A `;` length here is time: the slot takes that share of the
            // bar, as one sustained event. Absent, it is `UNIT` and the
            // sequence divides evenly, exactly as it did before `;` existed.
            Paradigm::Proportional => {
                let steps = items
                    .iter()
                    .map(|item| {
                        let length = match item.length {
                            Some(Length::Ratio(n)) => n,
                            // `paradigm` has already refused the other kinds.
                            _ => UNIT,
                        };
                        Ok(Slot::sized(to_step(&item.value, meter)?, length))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                Ok((Pattern::Steps(steps), 1.0))
            }
            Paradigm::Metrical => {
                let parts = metrical_parts(items, None, meter)?;
                if parts.is_empty() {
                    return Ok((Pattern::Steps(Vec::new()), 1.0));
                }
                let pass = total_beats(&parts).to_f64() / meter.quarters_per_bar();
                let steps = parts_to_steps(parts);
                // A pass that already fills its bar is left alone rather than
                // wrapped in a `Fast` of 1 — which is what makes four quarters
                // in 4/4 lower to precisely what four bare steps have always
                // lowered to. Which passes qualify moves with the signature:
                // three quarters is such a pass in 3/4 and not in 4/4.
                if pass == 1.0 {
                    Ok((steps, 1.0))
                } else {
                    Ok((Pattern::fast(1.0 / pass, steps), pass))
                }
            }
        },
        // Layers, each dividing the bar in its own right. Nothing is done to
        // reconcile their lengths — that they need not agree is the point, and
        // it is how a metrical layer polymeters against a proportional one. A
        // pass of the whole is over when the longest of them is.
        //
        // Which leaves what a shorter layer does for the rest of that pass,
        // and the two answers are both right — see [`Passes`], which is the
        // whole reason this function is asked.
        Value::Stack(layers) => {
            let mut timed = Vec::with_capacity(layers.len());
            let mut pass = 0.0f64;
            for layer in layers.iter() {
                let (p, bars) = to_pattern_timed(layer, meter, passes)?;
                pass = pass.max(bars);
                timed.push((p, bars));
            }
            let whole = if pass > 0.0 { pass } else { 1.0 };
            let out = timed
                .into_iter()
                .map(|(p, bars)| match passes {
                    Passes::Endless => p,
                    Passes::Counted => resting_out(p, bars, whole),
                })
                .collect();
            Ok((Pattern::Stack(out), whole))
        }
        Value::Number(n) => Ok((Pattern::seq([Step::Value(*n)]), 1.0)),
        // A slider is the number it stands at, like everything else a pattern
        // holds — see `to_step`, which says why a pattern is the one place a
        // signal can never reach.
        Value::Control { .. } => Ok((
            Pattern::seq([Step::Value(control_number(v).unwrap_or_default())]),
            1.0,
        )),
        Value::Rest => Ok((Pattern::Silence, 1.0)),
        Value::Trigger => Ok((Pattern::seq([Step::Value(1.0)]), 1.0)),
        // A written value on its own is a single sounding step of that length,
        // for the same reason it is inside a sequence.
        Value::Duration(b) => {
            let pass = b.to_f64() / meter.quarters_per_bar();
            let step = Pattern::seq([Step::Value(1.0)]);
            if pass == 1.0 {
                Ok((step, 1.0))
            } else {
                Ok((Pattern::fast(1.0 / pass, step), pass))
            }
        }
        Value::Tuplet => Err(
            "`t` marks a group as a tuplet — `[[c4;q, e4, g4];t]` — and is not a step \
             in its own right".to_string()),
        Value::Signal(_) => {
            Err("a pattern cannot contain a signal (patterns are events, not audio)".to_string())
        }
        Value::Buffer(_) => Err(
            "a pattern cannot contain a buffer (a pattern says when, an instrument \
             says what)".to_string()),
        Value::Function(_) => Err("a pattern cannot contain a function".to_string()),
        Value::Play { .. } => Err("a pattern cannot contain a play".to_string()),
        // A note sounds at one value, so there is nowhere in a pattern for a
        // list to go — which is exactly why the quote is only read in a lane.
        Value::Quoted(_) => Err(
            "a pattern cannot contain a quoted list — `'` marks a list as one value \
             for a `play` lane, and a note carries a single value. For several notes \
             at once, write a `stack`".to_string()),
        Value::Rate(_) => Err(
            "a pattern cannot contain a rate — `accel` says how fast a pattern runs, \
             so it belongs in `play`'s rate rather than among its steps".to_string()),
        Value::Destination(_) => Err(
            "a pattern cannot contain a MIDI destination — `midiout(..)` says where the \
             notes go, so it belongs where the instrument goes rather than among the \
             steps".to_string()),
        // A source *is* a pattern's worth of notes, so it is refused only as a
        // step *inside* one: `play(midiin("keys"), lead)` is where it goes,
        // and `[midiin("keys"), 60]` is a keyboard nested in a rhythm, which
        // is not a thing that could be played.
        Value::Source(_) => Err(
            "a pattern cannot contain a MIDI source — `midiin(..)` is the whole of what \
             is played, so it goes where the pattern goes rather than among its \
             steps".to_string()),
        Value::EnumType(def) => Err(format!(
            "`{}` is an enum rather than a step. Name one of its members — \
             `{}.{}` — if one of them is what should sound here",
            def.written(), def.written(),
            def.members.first().map_or("x", |m| m.name.as_str()))),
        Value::Enum { .. } => Err(enum_not_a_step(v)),
    }
}

/// Interpret a lowered value rhythmically, where how long a pass takes does not
/// matter — a lane, a fill's shape, or a check that something is a pattern at all.
pub fn to_pattern(v: &Value, meter: Meter) -> Result<Pattern, String> {
    to_pattern_timed(v, meter, Passes::Endless).map(|(pattern, _)| pattern)
}

/// A lane's values, if this one carries quoted lists rather than numbers.
///
/// `None` means "not that kind of lane", and the caller goes on to read the
/// value as a pattern of numbers exactly as it always has. A single `'[2, 3]`
/// is the one-element case: a lane wraps, so one value is every note's.
///
/// A lane that holds both is refused rather than resolved. The instrument walks
/// what it is given, so a lane that were sometimes a list and sometimes a
/// number would build for one note and fail on the next — and a voice that
/// fails to build halts its whole pattern, one note into a bar, which is the
/// worst place to learn about it.
fn lane_lists(v: &Value) -> Option<Result<Vec<Option<Vec<f64>>>, String>> {
    match v {
        Value::Quoted(nums) => Some(Ok(vec![Some(nums.to_vec())])),
        Value::List(items) => {
            if !items.iter().any(|i| matches!(i.value, Value::Quoted(_))) {
                return None;
            }
            Some(items.iter().try_fold(Vec::new(), |mut out, item| {
                // A `;` here means what it means in any lane — how many notes
                // this value covers — so it is counted out into the sequence
                // rather than kept as a length there would be no reader for.
                let held = match item.length {
                    None => 1,
                    Some(Length::Ratio(n)) if n.fract() == 0.0 && n >= 1.0 => n as usize,
                    Some(Length::Ratio(n)) => return Err(format!(
                        "a `;` is how many notes a value covers, so it has to be a \
                         whole number of them, and this one is {n}")),
                    Some(_) => return Err(
                        "a `;` is how many notes a value covers, and a written note \
                         value is a length of time — which is not a count of \
                         notes".to_string()),
                };
                let value = match &item.value {
                    Value::Quoted(nums) => Some(nums.to_vec()),
                    // Exactly what a resting number does: the parameter is not
                    // passed, so the instrument's own default stands.
                    Value::Rest => None,
                    _ => return Err(
                        "every value in a lane of quoted lists has to be one — a lane \
                         hands the whole of its value to the instrument, and a number \
                         among the lists would be a different shape on those \
                         notes".to_string()),
                };
                out.extend(std::iter::repeat(value).take(held));
                Ok(out)
            }))
        }
        _ => None,
    }
}

/// Refuse a written note value anywhere in a pattern being used as a lane.
///
/// The same misunderstanding `whole_lengths` catches, one step earlier and one
/// step larger. A lane is indexed by which note is asking, so a `;` there is a
/// count of notes; a written value is a length of time, which is not a thing a
/// position can be. It would otherwise slip through as a plain number of beats —
/// `q` is 1, and reads as "one note" — and quietly mean something else.
///
/// Returns the offending value, so the caller can name what it found.
fn no_durations(v: &Value) -> Result<(), Beats> {
    match v {
        Value::Duration(b) => Err(*b),
        Value::List(items) => items.iter().try_for_each(|item| {
            if let Some(Length::Beats(b)) = item.length {
                return Err(b);
            }
            no_durations(&item.value)
        }),
        Value::Stack(layers) => layers.iter().try_for_each(no_durations),
        _ => Ok(()),
    }
}

/// Refuse a fractional `;` anywhere in a pattern being used as a lane.
///
/// Lengths mean two different things in the two positions, and only one of them
/// admits a fraction. A pattern divides time, where `;1.5` is a dotted note; a
/// lane is indexed by which note is asking, where one and a half notes is not a
/// place. Rejecting it is better than rounding, because the rounding is silent
/// and the mistake is a misunderstanding worth naming.
///
/// Returns the offending length, so the caller can say which number it meant.
fn whole_lengths(p: &Pattern) -> Result<(), f64> {
    match p {
        Pattern::Silence => Ok(()),
        Pattern::Steps(slots) => slots.iter().try_for_each(|slot| {
            if slot.length.fract() != 0.0 {
                return Err(slot.length);
            }
            match &slot.step {
                Step::Group(inner) => whole_lengths(inner),
                _ => Ok(()),
            }
        }),
        Pattern::Stack(ps) => ps.iter().try_for_each(whole_lengths),
        Pattern::Fast(_, inner) => whole_lengths(inner),
    }
}

/// Why an enum member cannot be a step or a lane value.
///
/// Refused rather than unwrapped, even for a member holding a number, and this
/// is the one place in the language where a member does not stand for what it
/// holds. A pattern is not read here: it is handed to the scheduler thread,
/// which builds a voice per note against a clock that has already been set
/// running, and what crosses has to be plain data. Unwrapping at this boundary
/// would work, and would mean an enum whose meaning quietly depended on which
/// side of a thread it was read on — so both sides refuse it, and the message
/// says what to write instead.
fn enum_not_a_step(v: &Value) -> String {
    let Value::Enum { def, member } = v else {
        unreachable!("only called for an enum member")
    };
    let member = &def.members[*member];
    let instead = match &member.value {
        Some(Value::Number(n)) => format!("Write `{n}` here"),
        _ => "Write the value here".to_string(),
    };
    format!(
        "a pattern cannot contain `{}.{}`. An enum member stands for what it holds \
         everywhere a value is read at lowering, but a pattern is read a note at a \
         time on the scheduler's own thread, where only plain numbers reach. \
         {instead}, or bind it with a `let` outside the pattern",
        def.written(), member.name)
}

fn to_step(v: &Value, meter: Meter) -> Result<Step, String> {
    match v {
        Value::Number(n) => Ok(Step::Value(*n)),
        // Where the slider stood when this compiled, baked in — a pattern is
        // read a note at a time on the scheduler's thread, a fifth of a second
        // ahead of the audio clock, so a step is a number or it is nothing.
        // The panel marks it as one whose drag is heard at the next run.
        Value::Control { .. } => Ok(Step::Value(control_number(v).unwrap_or_default())),
        Value::Destination(_) => Err(
            "a MIDI destination is not a step — `midiout(..)` goes where the \
             instrument goes, not in the pattern".into()),
        Value::Source(_) => Err(
            "a MIDI source is not a step — `midiin(..)` is the whole pattern, not one \
             note of one".into()),
        Value::Rest => Ok(Step::Rest),
        // A trigger sounds but carries nothing; instruments that take an
        // argument see 1.
        Value::Trigger => Ok(Step::Value(1.0)),
        // Both fill the slot with a whole pattern: a list divides it in turn, a
        // stack layers over the whole of it. That is how a chord is written at
        // one step of a longer line.
        //
        // Reached only from a sequence of shares — a metrical sequence handles
        // its own groups, which are tuplets and say so. So an inner pattern that
        // is not one bar long is a metrical group in a share sequence, and the
        // two have no way to agree: the slot offers a share of a bar, which
        // knows nothing of beats, while the group is a length that ignores the
        // slot. It is refused rather than squeezed, because a group is given
        // exactly one bar of slot-local time and a half-bar pass would sound
        // its contents twice inside its own slot.
        Value::List(_) | Value::Stack(_) => {
            let (inner, pass) = to_pattern_timed(v, meter, Passes::Endless)?;
            if pass != 1.0 {
                return Err(
                    "a group written in note values cannot sit inside a sequence of \
                     shares — the sequence would give it a share of the bar, which \
                     says nothing about how long a beat is. Write the whole sequence \
                     in note values, where a group is a tuplet and marks itself `;t`"
                        .to_string());
            }
            Ok(Step::Group(Box::new(inner)))
        }
        // A written value reaching here is one in a sequence of shares, where
        // it has no length to be — `metrical_parts` turns the ones it owns into
        // triggers before ever calling this.
        Value::Duration(b) => Err(format!(
            "`{b}` is how long a step lasts, not what sounds. In a sequence of \
             shares there is nothing for it to say — write `\\;<share>` for a hit \
             of a given length, or write the whole sequence in note values")),
        Value::Tuplet => Err(
            "`t` marks a group as a tuplet — `[[c4;q, e4, g4];t]` — and is not a step \
             in its own right".to_string()),
        Value::Signal(_) => {
            Err("a pattern cannot contain a signal (patterns are events, not audio)".to_string())
        }
        Value::Buffer(_) => Err(
            "a pattern cannot contain a buffer (a pattern says when, an instrument \
             says what)".to_string()),
        Value::Function(_) => Err("a pattern cannot contain a function".to_string()),
        Value::Play { .. } => Err("a pattern cannot contain a play".to_string()),
        // A note sounds at one value, so there is nowhere in a pattern for a
        // list to go — which is exactly why the quote is only read in a lane.
        Value::Quoted(_) => Err(
            "a pattern cannot contain a quoted list — `'` marks a list as one value \
             for a `play` lane, and a note carries a single value. For several notes \
             at once, write a `stack`".to_string()),
        Value::Rate(_) => Err(
            "a pattern cannot contain a rate — `accel` says how fast a pattern runs, \
             so it belongs in `play`'s rate rather than among its steps".to_string()),
        Value::EnumType(def) => Err(format!(
            "`{}` is an enum rather than a step. Name one of its members — \
             `{}.{}` — if one of them is what should sound here",
            def.written(), def.written(),
            def.members.first().map_or("x", |m| m.name.as_str()))),
        Value::Enum { .. } => Err(enum_not_a_step(v)),
    }
}

/// When two things running at once are both over. One that never stops makes
/// the pair never stop, so nothing may be scheduled after it.
pub fn later_end(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        _ => None,
    }
}

/// `play_all(a, b, ...)` — several plays as one section.
pub const PLAY_ALL: &str = "play_all";

impl Lowerer {
    pub fn is_play_all(name: &str) -> bool {
        name == PLAY_ALL
    }

    /// Gather sections that run together into a single handle.
    ///
    /// There is nothing to *schedule* here. Every argument is lowered at the
    /// current `play_start`, so by the time this counts them they are already
    /// concurrent. What is missing is a name for the group, and that is what
    /// this returns: one `Play` that ends when the last of them does, so
    /// `.then` can follow the whole section rather than having to pick one of
    /// its parts to chain from.
    ///
    /// A section is written the same two ways here as in `seq` and `.then` — a
    /// `fn` named, or a play written out — which is why the arguments arrive
    /// raw. A named one writes nothing when it is evaluated, so it is inlined
    /// where it stands; the start it is inlined at is simply the one the group
    /// already sits at, since gathering plays is the one arrangement move that
    /// puts nothing anywhere new.
    pub fn play_all(&mut self, args: &[Arg], piped: Option<Value>) -> Result<Value, String> {
        if args.iter().any(|a| a.name.is_some()) {
            return Err(format!("{PLAY_ALL}: named arguments only work on a user `fn`"));
        }

        let mut parts: Vec<Value> = Vec::with_capacity(args.len() + 1);
        if piped.is_some() {
            parts.push(self.piped_section(PLAY_ALL, piped.as_ref())?);
        }
        for arg in args {
            // Lowered in written order, so a `rand` in one of these sections
            // draws where it reads as drawing.
            let part = match self.expr(&arg.value)? {
                Value::Function(def) => self.inline_named(PLAY_ALL, def)?,
                v => v,
            };
            parts.push(part);
        }

        if parts.is_empty() {
            return Err(format!(
                "{PLAY_ALL} expects at least one section: \
                 {PLAY_ALL}(verse, play_once(hits, bass))"));
        }

        // The floor is where the section itself opened: a group is never over
        // before it began, whatever its parts turn out to be.
        let mut ends_at = Some(self.play_start);
        let mut first = usize::MAX;
        let mut last = 0usize;
        for (i, part) in parts.iter().enumerate() {
            let Value::Play { ends_at: end, first: f, last: l, .. } = part else {
                return Err(format!(
                    "{PLAY_ALL}: argument {} is not a section — every argument must be \
                     either a `fn` written by name, such as `chorus`, or a play written \
                     out, such as `playn(pat, inst, 4)`", i + 1));
            };
            ends_at = later_end(ends_at, *end);
            // Contiguous, because the arguments were lowered left to right and
            // each one pushed its bindings as it went.
            first = first.min(*f);
            last = last.max(*l);
        }

        Ok(Value::Play {
            starts_at: self.play_start,
            ends_at,
            first: first.min(last),
            last,
            // A group is its own beginning: the plays it gathers were written
            // as its arguments, so there is no chain in front of it.
            chain_first: first.min(last),
            // Gathering several plays is exactly what makes "the instrument"
            // ambiguous, so a group is never a template for a fill — unless it
            // gathered only one, where nothing was made ambiguous at all.
            template: (last == first + 1).then_some(first),
        })
    }
}
