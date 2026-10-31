//! Compile-time arithmetic on numbers.
//!
//! These emit no nodes: they fold during lowering, exactly like the list
//! builtins. Written to read through the dot operator — `60.m2h`,
//! `x.clamp(0, 1)` — though `m2h(60)` and `60 >> m2h` are the same call.
//!
//! Three of them — `oct`, `semi` and `scale` — take a list of notes as well as
//! one note, because a riff is as natural a subject for them as a pitch is.
//! That is a property of those three rather than of this file: `60.round` is
//! arithmetic that happens to land on a note, and a list there is still a
//! mistake. See [`pitch_op`] and [`over_notes`].
//!
//! Signals are rejected rather than silently handled. A per-sample conversion
//! would need its own graph node, and every use so far is a constant or a
//! pattern value, which is already a number by the time a voice is lowered.
//!
//! Inputs that would fold to NaN or infinity are errors rather than results.
//! A poisoned constant propagates silently into the graph and surfaces as
//! silence much later, which is miserable to trace back.

use std::rc::Rc;

use crate::lang::ListBuiltin;
use crate::swync_graph::environment::{Item, Value};
use crate::lowerer::expr::{as_data, not_this_member, control_number};
use crate::lowerer::lists::check_arity;
use crate::lowerer::lower::Lowerer;
/// `bpm` is the transport's own conversion, so the two can never disagree.
use crate::scheduler::clock::cps_from_bpm;

/// MIDI note 69 is A4.
const A4_MIDI: f64 = 69.0;
const A4_HZ: f64 = 440.0;
const SEMITONES_PER_OCTAVE: f64 = 12.0;
const CENTS_PER_OCTAVE: f64 = 1200.0;

/// Shared with `lowerer::random`, which draws numbers into the same fold and so
/// has to reject a signal in the same words.
pub(crate) fn number(func: &str, what: &str, v: &Value) -> Result<f64, String> {
    // A slider is a number here, baked at the position it was standing —
    // these fold during lowering and there is no node for a changing value to
    // be part of. See `expr::control_number`.
    if let Some(n) = control_number(v) {
        return Ok(n);
    }
    match as_data(v) {
        Value::Number(n) => Ok(*n),
        _ => Err(not_this_member(v, &format!("{func}: {what} must be a number"))
            .unwrap_or_else(|| format!(
                "{func}: {what} must be a compile-time number (it folds during lowering)"
            ))),
    }
}

pub(crate) fn positive(func: &str, what: &str, n: f64) -> Result<f64, String> {
    if n <= 0.0 {
        return Err(format!("{func}: {what} must be above zero, got {n}"));
    }
    Ok(n)
}

pub(crate) fn range(func: &str, lo: f64, hi: f64) -> Result<(), String> {
    if !(hi > lo) {
        return Err(format!("{func}: the range {lo}..{hi} is empty"));
    }
    Ok(())
}

pub(crate) fn scale_tones(func: &str, v: &Value) -> Result<Vec<f64>, String> {
    let wanted = format!("{func}: the scale must be a list of semitone offsets");
    let Value::List(items) = as_data(v) else {
        return Err(not_this_member(v, &wanted).unwrap_or(wanted));
    };
    if items.is_empty() {
        return Err(format!("{func}: the scale is empty"));
    }
    items
        .iter()
        .map(|item| number(func, "every scale degree", &item.value))
        .collect()
}

impl Lowerer {
    pub fn math_builtin(&mut self, func: &str, args: &[Value]) -> Result<Option<Value>, String> {
        let Some(spec) = crate::lang::math_builtin(func) else {
            return Ok(None);
        };
        check_arity(func, args, Some(spec))?;

        // The pitch builtins take a whole list as readily as a single note.
        // Transposing a riff is the same thought as transposing a note, and a
        // riff is written as a list — so the list is the subject here rather
        // than a type error to be routed around with `map`. Taken before the
        // fold below, which is where a list would otherwise be refused; a list
        // handed to anything else falls through to it and is refused there, as
        // it always was.
        if let Value::List(items) = as_data(&args[0]) {
            if let Some(op) = pitch_op(func, spec, args)? {
                return over_notes(func, spec.params[0], items, &op).map(Some);
            }
        }

        // Every math builtin takes its subject first, which is what makes the
        // dot spelling read: `60.oct(-1)` is `oct(60, -1)`.
        let x = number(func, spec.params[0], &args[0])?;
        let arg = |i: usize| number(func, spec.params[i], &args[i]);

        let out = match func {
            // --- conversions ---
            "m2h" => A4_HZ * ((x - A4_MIDI) / SEMITONES_PER_OCTAVE).exp2(),
            "h2m" => {
                let hz = positive(func, "the frequency", x)?;
                A4_MIDI + SEMITONES_PER_OCTAVE * (hz / A4_HZ).log2()
            }
            "db" => 10.0f64.powf(x / 20.0),
            "amp" => 20.0 * positive(func, "the amplitude", x)?.log10(),
            "cents" => positive(func, "the frequency", x)? * (arg(1)? / CENTS_PER_OCTAVE).exp2(),
            "bpm" => cps_from_bpm(positive(func, "the tempo", x)?, self.meter),

            // --- pitch arithmetic ---
            "oct" => x + SEMITONES_PER_OCTAVE * arg(1)?,
            "semi" => x + arg(1)?,
            "scale" => snap_to_scale(x, &scale_tones(func, &args[1])?),

            // --- ranges and shaping ---
            "clamp" => {
                let (lo, hi) = (arg(1)?, arg(2)?);
                range(func, lo, hi)?;
                x.clamp(lo, hi)
            }
            "norm" => {
                let (lo, hi) = (arg(1)?, arg(2)?);
                lo + x * (hi - lo)
            }
            "wrap" => {
                let (lo, hi) = (arg(1)?, arg(2)?);
                range(func, lo, hi)?;
                lo + (x - lo).rem_euclid(hi - lo)
            }
            "round" => x.round(),
            "floor" => x.floor(),
            "ceil" => x.ceil(),
            "abs" => x.abs(),
            "pow" => x.powf(arg(1)?),
            "sqrt" => {
                if x < 0.0 {
                    return Err(format!("{func}: cannot take the root of {x}"));
                }
                x.sqrt()
            }
            "log2" => positive(func, "x", x)?.log2(),

            // Unreachable while the table and this match agree, which
            // `every_math_builtin_is_implemented` checks.
            _ => return Ok(None),
        };

        if !out.is_finite() {
            return Err(format!("{func} produced {out}, which cannot be used as a value"));
        }
        Ok(Some(Value::Number(out)))
    }
}

/// What one of the pitch builtins does to a single note, or `None` if `func`
/// is not one of them — which is how the list branch above knows to fall
/// through.
///
/// Read once per call rather than once per note. The interval or the scale is
/// the same for every note in the list, so re-reading it per element would be
/// work for nothing — and worse, it would report a bad second argument once per
/// note instead of once, against the argument where the mistake actually is.
fn pitch_op(func: &str, spec: &ListBuiltin, args: &[Value]) -> Result<Option<PitchOp>, String> {
    // The second argument is reached only inside a matched arm, and that is
    // load-bearing: most names in this table take one argument, so `params[1]`
    // and `args[1]` do not exist for them. For these three `check_arity` has
    // already insisted on both.
    let op: PitchOp = match func {
        "oct" => {
            let by = SEMITONES_PER_OCTAVE * number(func, spec.params[1], &args[1])?;
            Box::new(move |note| note + by)
        }
        "semi" => {
            let by = number(func, spec.params[1], &args[1])?;
            Box::new(move |note| note + by)
        }
        "scale" => {
            let tones = scale_tones(func, &args[1])?;
            Box::new(move |note| snap_to_scale(note, &tones))
        }
        _ => return Ok(None),
    };
    Ok(Some(op))
}

/// One pitch operation, ready to be applied to every note of a list.
type PitchOp = Box<dyn Fn(f64) -> f64>;

/// `op` applied to every note of a list, and everything else left where it was.
///
/// Recursive, because a nested list is a subdivision group — one step holding a
/// sequence of its own — and a group of notes is as much a thing to transpose
/// as a single note is. A rest passes through untouched: a rest marks a step
/// where nothing sounds, so there is no pitch there to move.
///
/// Each element is rebuilt with the `length` it arrived with, which is the
/// reason this exists rather than `riff.map(up)`. A list a builtin *builds*
/// carries no lengths, so a transposition through `map` or a `for` silently
/// drops the rhythm the notes were written with; here the elements survive the
/// operation, so their `;` lengths do too.
fn over_notes(func: &str, what: &str, items: &[Item], op: &PitchOp) -> Result<Value, String> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let value = match as_data(&item.value) {
            Value::List(inner) => over_notes(func, what, inner, op)?,
            Value::Rest => Value::Rest,
            // Anything else has to be a number, and `number` is what says so —
            // in the same words it uses when a bare `semi` is handed a signal.
            other => {
                let moved = op(number(func, what, other)?);
                if !moved.is_finite() {
                    return Err(format!(
                        "{func} produced {moved}, which cannot be used as a value"
                    ));
                }
                Value::Number(moved)
            }
        };
        out.push(Item { value, length: item.length });
    }
    Ok(Value::List(Rc::new(out)))
}

/// Snap a MIDI note to the nearest tone of `tones`, given as semitone offsets
/// within an octave. Candidates from the neighbouring octaves are considered
/// too, so a note just under the root snaps up rather than falling a seventh.
/// Ties snap down, so the result is deterministic.
fn snap_to_scale(note: f64, tones: &[f64]) -> f64 {
    let octave = (note / SEMITONES_PER_OCTAVE).floor() * SEMITONES_PER_OCTAVE;
    let mut best = f64::NAN;
    let mut best_distance = f64::INFINITY;

    for shift in [-SEMITONES_PER_OCTAVE, 0.0, SEMITONES_PER_OCTAVE] {
        for tone in tones {
            let candidate = octave + shift + tone.rem_euclid(SEMITONES_PER_OCTAVE);
            let distance = (candidate - note).abs();
            // `<` rather than `<=`, and candidates ascend, so a tie keeps the
            // lower one.
            if distance < best_distance {
                best_distance = distance;
                best = candidate;
            }
        }
    }
    best
}

/// Values shared with the tests below and useful when reading them.
#[cfg(test)]
const MAJOR: &str = "[0, 2, 4, 5, 7, 9, 11]";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swync_graph::ugen_nodes::NodeInput;
    use crate::scheduler::clock::Meter;
    use crate::lang::MATH_BUILTINS;
    use crate::lowerer::lower::lower;
    use crate::parser::parser::parse;

    /// Fold `src` and read back the constant it produced.
    fn num(src: &str) -> f64 {
        let items = parse(format!("sin({src})\n")).expect("parse failed");
        let g = lower(&items).expect("lower failed").graph;
        match g.nodes[0].inputs[0] {
            NodeInput::Const(v) => v,
            _ => panic!("expected a folded constant"),
        }
    }

    fn err(src: &str) -> String {
        let items = parse(format!("sin({src})\n")).expect("parse failed");
        match lower(&items) {
            Err(e) => e,
            Ok(_) => panic!("expected `{src}` to fail"),
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-4
    }

    // --- conversions ---

    #[test]
    fn a4_is_440() {
        assert!(close(num("69.m2h"), 440.0));
    }

    #[test]
    fn octaves_double_and_halve() {
        assert!(close(num("81.m2h"), 880.0));
        assert!(close(num("57.m2h"), 220.0));
    }

    #[test]
    fn middle_c_is_about_261_6() {
        assert!(close(num("60.m2h"), 261.625_565));
    }

    #[test]
    fn fractional_notes_interpolate() {
        let quarter_tone = num("69.5.m2h");
        assert!(quarter_tone > 440.0 && quarter_tone < 466.2, "got {quarter_tone}");
    }

    /// h2m undoes m2h.
    #[test]
    fn hertz_and_midi_round_trip() {
        assert!(close(num("440.h2m"), 69.0));
        assert!(close(num("60.m2h.h2m"), 60.0));
        assert!(close(num("100.h2m.m2h"), 100.0));
    }

    #[test]
    fn decibels_convert_both_ways() {
        assert!(close(num("0.db"), 1.0));
        assert!(close(num("(-6).db"), 0.501_187));
        assert!(close(num("(-20).db"), 0.1));
        assert!(close(num("1.amp"), 0.0));
        assert!(close(num("0.5.amp"), -6.020_6));
        assert!(close(num("0.25.db.amp"), 0.25));
    }

    #[test]
    fn cents_detune_a_frequency() {
        assert!(close(num("440.cents(0)"), 440.0));
        // A full 1200 cents is an octave.
        assert!(close(num("440.cents(1200)"), 880.0));
        assert!(close(num("440.cents(-1200)"), 220.0));
        assert!(num("440.cents(-14)") < 440.0);
    }

    /// The default tempo, expressed the way a musician would write it.
    #[test]
    fn bpm_matches_the_default_cps() {
        assert!(close(num("120.bpm"), 0.5));
        assert!(close(num("60.bpm"), 0.25));
        assert!(close(num("135.bpm"), 0.5625));
    }

    /// `bpm` answers in *bars* per second, so the signature reaches it: the
    /// same 120 bpm is half a bar a second in 4/4 and two thirds of one in
    /// 3/4, because the bar got shorter and the beat did not.
    #[test]
    fn bpm_answers_in_bars_and_so_follows_the_signature() {
        fn num_in(src: &str, meter: Meter) -> f64 {
            let items = parse(format!("sin({src})\n")).expect("parse failed");
            let g = crate::lowerer::lower::lower_in_meter(&items, meter)
                .expect("lower failed")
                .graph;
            match g.nodes[0].inputs[0] {
                NodeInput::Const(v) => v,
                _ => panic!("expected a folded constant"),
            }
        }

        assert!(close(num_in("120.bpm", Meter { top: 4, bottom: 4 }), 0.5));
        assert!(close(num_in("120.bpm", Meter { top: 3, bottom: 4 }), 2.0 / 3.0));
        assert!(close(num_in("120.bpm", Meter { top: 6, bottom: 8 }), 2.0 / 3.0));
    }

    // --- pitch arithmetic ---

    #[test]
    fn octave_and_semitone_transposition() {
        assert!(close(num("60.oct(-1)"), 48.0));
        assert!(close(num("60.oct(1)"), 72.0));
        assert!(close(num("60.semi(7)"), 67.0));
        assert!(close(num("60.semi(-1)"), 59.0));
    }

    /// A riff is written as a list, so transposing one is the same call as
    /// transposing a note. Every element moves by the same interval.
    #[test]
    fn semi_transposes_every_note_of_a_list() {
        assert!(close(num("[60, 63, 67].semi(7)[0]"), 67.0));
        assert!(close(num("[60, 63, 67].semi(7)[1]"), 70.0));
        assert!(close(num("[60, 63, 67].semi(7)[2]"), 74.0));
        assert!(close(num("len([60, 63, 67].semi(7))"), 3.0));
    }

    /// A nested list is one step holding a sequence of its own, and a group of
    /// notes is as much a thing to transpose as a single note — so the interval
    /// reaches all the way down rather than stopping at the group.
    #[test]
    fn semi_moves_a_nested_group_along_with_everything_else() {
        assert!(close(num("[60, [62, 64]].semi(2)[0]"), 62.0));
        assert!(close(num("[60, [62, 64]].semi(2)[1][0]"), 64.0));
        assert!(close(num("[60, [62, 64]].semi(2)[1][1]"), 66.0));
    }

    /// The subject may be a list, but the interval is still one number: a list
    /// there is a mistake, and it is named as one rather than being spread
    /// across the notes.
    #[test]
    fn semi_still_wants_a_single_interval() {
        let e = err("[60, 63].semi([1, 2])[0]");
        assert!(e.contains("semitones"), "got: {e}");
    }

    /// An element that is not a note is refused in the words a bare `semi`
    /// already uses, so the reason reads the same whichever form was written.
    #[test]
    fn semi_refuses_a_list_holding_something_that_is_not_a_note() {
        let e = err("[60, sin(220)].semi(7)[0]");
        assert!(e.contains("semi: note"), "got: {e}");
    }

    /// `oct` and `scale` take a list on the same terms `semi` does — they are
    /// the three operations whose subject is as naturally a riff as one pitch.
    #[test]
    fn oct_and_scale_take_a_list_as_well() {
        assert!(close(num("[60, 63, 67].oct(-1)[0]"), 48.0));
        assert!(close(num("[60, 63, 67].oct(-1)[2]"), 55.0));
        assert!(close(num(&format!("[61, 66].scale({MAJOR})[0]")), 60.0));
        assert!(close(num(&format!("[61, 66].scale({MAJOR})[1]")), 65.0));
    }

    /// A list reaches every name in this table, not only the three that want
    /// one, and the rest must refuse it in their own words.
    ///
    /// Written against a one-argument name on purpose: reading the interval
    /// out of a list call means reaching for a second parameter, and most names
    /// here do not have one. This is the test that keeps that reach behind the
    /// decision about which name it is.
    #[test]
    fn a_list_handed_to_a_name_that_is_not_about_pitch_is_refused() {
        let e = err("[60, 63].round[0]");
        assert!(e.contains("round: x"), "got: {e}");
        let e = err("[60, 63].clamp(0, 12)[0]");
        assert!(e.contains("clamp: x"), "got: {e}");
    }

    #[test]
    fn scale_leaves_notes_already_in_the_scale() {
        for note in [60, 62, 64, 65, 67, 69, 71, 72] {
            assert!(
                close(num(&format!("{note}.scale({MAJOR})")), note as f64),
                "{note} should be unchanged"
            );
        }
    }

    #[test]
    fn scale_snaps_notes_outside_it() {
        assert!(close(num(&format!("61.scale({MAJOR})")), 60.0)); // tie -> down
        assert!(close(num(&format!("63.scale({MAJOR})")), 62.0)); // tie -> down
        assert!(close(num(&format!("66.scale({MAJOR})")), 65.0));
        assert!(close(num(&format!("70.scale({MAJOR})")), 69.0));
    }

    /// A note just below the root snaps up to it, not down a seventh.
    #[test]
    fn scale_considers_neighbouring_octaves() {
        assert!(close(num("59.scale([0, 4, 7])"), 60.0));
        assert!(close(num("71.scale([0, 4, 7])"), 72.0));
    }

    #[test]
    fn scale_works_with_sparse_scales() {
        // A pentatonic scale, and a one-note scale that flattens everything.
        assert!(close(num("61.scale([0, 3, 5, 7, 10])"), 60.0));
        assert!(close(num("65.scale([0])"), 60.0));
        assert!(close(num("67.scale([0])"), 72.0));
    }

    // --- ranges and shaping ---

    #[test]
    fn clamp_constrains() {
        assert!(close(num("5.clamp(0, 1)"), 1.0));
        assert!(close(num("(-5).clamp(0, 1)"), 0.0));
        assert!(close(num("0.25.clamp(0, 1)"), 0.25));
    }

    #[test]
    fn norm_maps_the_unit_range() {
        assert!(close(num("0.norm(100, 200)"), 100.0));
        assert!(close(num("1.norm(100, 200)"), 200.0));
        assert!(close(num("0.5.norm(100, 200)"), 150.0));
        // Outside 0..1 extrapolates rather than clamping.
        assert!(close(num("2.norm(0, 10)"), 20.0));
    }

    #[test]
    fn wrap_folds_around() {
        assert!(close(num("13.wrap(0, 12)"), 1.0));
        assert!(close(num("(-1).wrap(0, 12)"), 11.0));
        assert!(close(num("12.wrap(0, 12)"), 0.0));
        assert!(close(num("5.wrap(0, 12)"), 5.0));
    }

    /// Postfix binds tighter than unary minus, as it does everywhere else:
    /// `-6.db` is `-(6.db)`. Negative subjects need parentheses.
    #[test]
    fn unary_minus_is_looser_than_a_method() {
        assert!(close(num("-6.db"), -num("6.db")));
        assert!(close(num("(-6).db"), 0.501_187));
        assert!(close(num("-9.sqrt"), -3.0));
        assert!(close(num("(-9).abs.sqrt"), 3.0));
    }

    #[test]
    fn rounding_and_magnitude() {
        assert!(close(num("1.4.round"), 1.0));
        assert!(close(num("1.5.round"), 2.0));
        assert!(close(num("1.9.floor"), 1.0));
        assert!(close(num("1.1.ceil"), 2.0));
        assert!(close(num("(-3).abs"), 3.0));
        assert!(close(num("(-1.5).floor"), -2.0));
    }

    #[test]
    fn powers_and_roots() {
        assert!(close(num("2.pow(10)"), 1024.0));
        assert!(close(num("9.sqrt"), 3.0));
        assert!(close(num("1024.log2"), 10.0));
        assert!(close(num("2.pow(0.5)"), 2.0f64.sqrt()));
    }

    // --- errors ---

    #[test]
    fn domain_errors_are_reported() {
        assert!(err("0.h2m").contains("above zero"));
        assert!(err("(-1).h2m").contains("above zero"));
        assert!(err("0.amp").contains("above zero"));
        assert!(err("(-4).sqrt").contains("root"));
        assert!(err("0.log2").contains("above zero"));
        assert!(err("0.bpm").contains("above zero"));
    }

    #[test]
    fn empty_ranges_are_reported() {
        assert!(err("1.clamp(5, 5)").contains("empty"));
        assert!(err("1.clamp(5, 0)").contains("empty"));
        assert!(err("1.wrap(3, 3)").contains("empty"));
    }

    #[test]
    fn scale_argument_errors_are_reported() {
        assert!(err("60.scale(5)").contains("list"));
        assert!(err("60.scale([])").contains("empty"));
        assert!(err("60.scale([sin(2)])").contains("compile-time number"));
    }

    #[test]
    fn a_signal_is_rejected() {
        assert!(err("sin(2).m2h").contains("compile-time number"));
        assert!(err("sin(2).clamp(0, 1)").contains("compile-time number"));
        assert!(err("1.clamp(sin(2), 1)").contains("compile-time number"));
    }

    #[test]
    fn wrong_arity_is_rejected() {
        assert!(err("60.m2h(1)").contains("expects"));
        assert!(err("60.clamp(1)").contains("expects"));
    }

    /// The same call, three spellings.
    #[test]
    fn dot_pipe_and_plain_call_agree() {
        assert_eq!(num("60.m2h"), num("m2h(60)"));
        assert_eq!(num("60.m2h"), num("60 >> m2h"));
        assert_eq!(num("5.clamp(0, 1)"), num("clamp(5, 0, 1)"));
    }

    /// Methods compose, which is the reason for the dot operator.
    #[test]
    fn methods_compose() {
        // A major-scale note, an octave down, as a frequency.
        assert!(close(
            num(&format!("61.scale({MAJOR}).oct(-1).m2h")),
            num("48.m2h")
        ));
        assert!(close(num("0.5.norm(0, 12).round.oct(1)"), 18.0));
    }

    /// The table drives dispatch, so every name listed must be handled.
    #[test]
    fn every_math_builtin_is_implemented() {
        for b in MATH_BUILTINS {
            let arity = *b.arities.first().expect("an arity");
            // A subject that is in every function's domain, plus filler args.
            let extra = match b.name {
                "scale" => vec!["[0, 4, 7]"; arity - 1],
                "clamp" | "wrap" => vec!["0", "12"],
                _ => vec!["1"; arity - 1],
            };
            let src = if extra.is_empty() {
                format!("2.{}", b.name)
            } else {
                format!("2.{}({})", b.name, extra.join(", "))
            };
            let items = parse(format!("sin({src})\n")).expect("parse failed");
            assert!(
                lower(&items).is_ok(),
                "`{}` is in MATH_BUILTINS but `{src}` did not lower",
                b.name
            );
        }
    }

    /// A name must not appear in two tables: dispatch tries list, then math,
    /// then random, then UGens, so a duplicate would silently shadow.
    #[test]
    fn builtin_names_do_not_collide() {
        use crate::lang::{LIST_BUILTINS, RANDOM_BUILTINS, UGENS};

        // Every table paired with every later one, in dispatch order — so a
        // name added to two of them names both here.
        let tables: [(&str, Vec<&str>); 4] = [
            ("LIST_BUILTINS", LIST_BUILTINS.iter().map(|b| b.name).collect()),
            ("MATH_BUILTINS", MATH_BUILTINS.iter().map(|b| b.name).collect()),
            ("RANDOM_BUILTINS", RANDOM_BUILTINS.iter().map(|b| b.name).collect()),
            ("UGENS", UGENS.iter().map(|u| u.name).collect()),
        ];

        for (i, (this, names)) in tables.iter().enumerate() {
            for (other, later) in &tables[i + 1..] {
                for name in names {
                    assert!(
                        !later.contains(name),
                        "`{name}` is in both {this} and {other}"
                    );
                }
            }
        }
        assert!(!UGENS.is_empty());
    }
}
