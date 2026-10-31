//! Random number generators, drawn while the program is lowered.
//!
//! Like the math builtins these emit no nodes: a draw becomes a constant, so
//! the graph and the pattern bindings an eval produces are *settled* — a riff
//! from `randis` plays the same eight notes until you evaluate again. Wanting
//! the opposite is what audio-rate `noise` and `hold` are for.
//!
//! The exception is worth knowing about: a voice is lowered afresh for every
//! note, so a draw inside an instrument body is a new number on each note.
//!
//! Everything here shares the lowerer's one RNG, which is why `seed` can pin
//! `choice` and `scramble` along with the rest of it.

use std::rc::Rc;

use rand::seq::SliceRandom;
use rand::{RngExt, SeedableRng};
use rand::rngs::SmallRng;
use rand_distr::{Cauchy, Distribution, Exp, Normal, Pareto, Poisson, Triangular};

use crate::lang::ListBuiltin;
use crate::lowerer::lists::{as_list, check_arity};
use crate::lowerer::lower::Lowerer;
use crate::lowerer::math::{number, positive, range, scale_tones};
use crate::swync_graph::environment::{Item, Value};

const SEMITONES_PER_OCTAVE: f64 = 12.0;

/// A list long enough to be a mistake rather than a pattern. Every list builtin
/// here allocates its whole result, so an unchecked `rands(1e9)` would hang the
/// editor between the keystroke and the sound.
const MAX_COUNT: f64 = 100_000.0;

/// The widest note range `randscale` will enumerate. MIDI is 0..127, so this is
/// already absurd — it is here because the enumeration is proportional to the
/// span rather than to the count asked for, and so is the only argument that
/// `count` does not already bound.
const MAX_SCALE_SPAN: f64 = 2_000.0;

fn whole(func: &str, what: &str, n: f64) -> Result<f64, String> {
    if n.fract() != 0.0 || !n.is_finite() {
        return Err(format!("{func}: {what} must be a whole number, got {n}"));
    }
    Ok(n)
}

fn count(func: &str, what: &str, v: &Value) -> Result<usize, String> {
    let n = whole(func, what, number(func, what, v)?)?;
    if n < 0.0 {
        return Err(format!("{func}: {what} cannot be negative, got {n}"));
    }
    if n > MAX_COUNT {
        return Err(format!("{func}: {what} of {n} is more than {MAX_COUNT} — a typo?"));
    }
    Ok(n as usize)
}

/// The MIDI notes of `tones` — semitone offsets within an octave — that fall in
/// `lo..=hi`, ascending and without duplicates.
///
/// Enumerating them is what lets `randscale` draw *evenly*. Snapping a uniform
/// draw instead would favour whichever degrees have the widest gap either side
/// of them, so a pentatonic riff would lean on its sparse notes.
fn scale_notes(tones: &[f64], lo: f64, hi: f64) -> Vec<f64> {
    let first = (lo / SEMITONES_PER_OCTAVE).floor() * SEMITONES_PER_OCTAVE;
    let mut notes = Vec::new();
    let mut base = first;
    while base <= hi {
        for tone in tones {
            let note = base + tone.rem_euclid(SEMITONES_PER_OCTAVE);
            if note >= lo && note <= hi {
                notes.push(note);
            }
        }
        base += SEMITONES_PER_OCTAVE;
    }
    notes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    notes.dedup();
    notes
}

impl Lowerer {
    /// A uniform float in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        self.rng.random::<f64>()
    }

    /// A uniform index into a list of `len` elements. `len` must not be zero —
    /// every caller checks for the empty list first, to say so in its own words.
    pub fn index(&mut self, len: usize) -> usize {
        self.rng.random_range(0..len)
    }

    /// A uniform float in `[lo, hi)`.
    fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.unit() * (hi - lo)
    }

    pub fn random_builtin(&mut self, func: &str, args: &[Value]) -> Result<Option<Value>, String> {
        let Some(spec) = crate::lang::random_builtin(func) else {
            return Ok(None);
        };
        check_arity(func, args, Some(spec))?;

        // Positional readers, so an arm can say "the second argument if it was
        // written, else this default" without repeating the parameter name that
        // the error message needs.
        let req = |i: usize| number(func, spec.params[i], &args[i]);
        let opt = |i: usize, fallback: f64| -> Result<f64, String> {
            match args.get(i) {
                None => Ok(fallback),
                Some(v) => number(func, spec.params[i], v),
            }
        };

        let out = match func {
            // --- uniform ---
            "rand" => {
                let (lo, hi) = if args.is_empty() { (0.0, 1.0) } else { (req(0)?, req(1)?) };
                range(func, lo, hi)?;
                Value::Number(self.uniform(lo, hi))
            }

            "randi" => {
                let lo = whole(func, spec.params[0], req(0)?)?;
                let hi = whole(func, spec.params[1], req(1)?)?;
                range(func, lo, hi)?;
                Value::Number(self.rng.random_range(lo as i64..hi as i64) as f64)
            }

            "coin" => {
                let p = opt(0, 0.5)?;
                if !(0.0..=1.0).contains(&p) {
                    return Err(format!("{func}: the probability must be in 0..=1, got {p}"));
                }
                Value::Number(if self.unit() < p { 1.0 } else { 0.0 })
            }

            // Answers with the seed so it reads as a value — `let s = seed(42)`
            // keeps the number that produced a take.
            "seed" => {
                let n = req(0)?;
                if !n.is_finite() {
                    return Err(format!("{func}: the seed must be a finite number, got {n}"));
                }
                // Through the bits rather than a cast, so `seed(0.5)` and
                // `seed(-1)` are seeds like any other instead of collapsing
                // onto zero.
                self.rng = SmallRng::seed_from_u64(n.to_bits());
                Value::Number(n)
            }

            // --- shaped ---
            "gauss" => {
                let (mean, deviation) = (opt(0, 0.0)?, opt(1, 1.0)?);
                Value::Number(self.normal(func, spec, mean, deviation)?)
            }

            "humanize" => {
                let (x, amount) = (req(0)?, req(1)?);
                Value::Number(x + self.normal(func, spec, 0.0, amount)?)
            }

            // Stated as a mean rather than fundsp-style as a rate, because the
            // musical question is "how long, on average" and not "how often".
            "expo" => {
                let mean = positive(func, spec.params[0], opt(0, 1.0)?)?;
                let dist = Exp::new(1.0 / mean)
                    .map_err(|e| format!("{func}: {} is not a usable mean — {e}", mean))?;
                Value::Number(dist.sample(&mut self.rng))
            }

            "tri" => {
                let (lo, hi) = (req(0)?, req(1)?);
                range(func, lo, hi)?;
                let mode = opt(2, (lo + hi) / 2.0)?;
                let dist = Triangular::new(lo, hi, mode).map_err(|e| {
                    format!("{func}: {mode} is not a usable mode for {lo}..{hi} — {e}")
                })?;
                Value::Number(dist.sample(&mut self.rng))
            }

            "cauchy" => {
                let median = opt(0, 0.0)?;
                let spread = positive(func, spec.params[1], opt(1, 1.0)?)?;
                let dist = Cauchy::new(median, spread)
                    .map_err(|e| format!("{func}: {spread} is not a usable spread — {e}"))?;
                Value::Number(dist.sample(&mut self.rng))
            }

            "pareto" => {
                let scale = positive(func, spec.params[0], opt(0, 1.0)?)?;
                let shape = positive(func, spec.params[1], opt(1, 1.0)?)?;
                let dist = Pareto::new(scale, shape)
                    .map_err(|e| format!("{func}: {scale} and {shape} do not make a shape — {e}"))?;
                Value::Number(dist.sample(&mut self.rng))
            }

            "poisson" => {
                let mean = positive(func, spec.params[0], opt(0, 1.0)?)?;
                let dist = Poisson::new(mean)
                    .map_err(|e| format!("{func}: {mean} is not a usable mean — {e}"))?;
                Value::Number(dist.sample(&mut self.rng))
            }

            // --- lists ---
            "rands" => {
                let n = count(func, spec.params[0], &args[0])?;
                let (lo, hi) = if args.len() == 1 { (0.0, 1.0) } else { (req(1)?, req(2)?) };
                range(func, lo, hi)?;
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(Value::Number(self.uniform(lo, hi)));
                }
                Value::List(Item::all(out))
            }

            "randis" => {
                let n = count(func, spec.params[0], &args[0])?;
                let lo = whole(func, spec.params[1], req(1)?)?;
                let hi = whole(func, spec.params[2], req(2)?)?;
                range(func, lo, hi)?;
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    out.push(Value::Number(self.rng.random_range(lo as i64..hi as i64) as f64));
                }
                Value::List(Item::all(out))
            }

            // `start` is the first element rather than the first thing stepped
            // away from, so `walk(n, 60, s)` begins where it says it does.
            "walk" | "walki" => {
                let n = count(func, spec.params[0], &args[0])?;
                let mut at = req(1)?;
                let step = req(2)?;
                if step < 0.0 {
                    return Err(format!("{func}: the step cannot be negative, got {step}"));
                }
                let step = if func == "walki" {
                    whole(func, spec.params[2], step)?
                } else {
                    step
                };

                let mut out = Vec::with_capacity(n);
                for i in 0..n {
                    if i > 0 {
                        at += if func == "walki" {
                            // Inclusive of both ends: at step 1 the walk may go
                            // down one, stay, or go up one.
                            self.rng.random_range(-(step as i64)..=step as i64) as f64
                        } else {
                            self.uniform(-step, step)
                        };
                    }
                    out.push(Value::Number(at));
                }
                Value::List(Item::all(out))
            }

            // Shuffled, then cut. `choose_multiple` would be cheaper but keeps
            // the elements in the order they were written, and a handful drawn
            // for a melody wants its order drawn too.
            "choices" => {
                let items = as_list(func, &args[0])?;
                let n = count(func, spec.params[1], &args[1])?;
                if n > items.len() {
                    return Err(format!(
                        "{func}: cannot take {n} distinct elements from a list of {}",
                        items.len()
                    ));
                }
                let mut out: Vec<Item> = items.iter().cloned().collect();
                out.shuffle(&mut self.rng);
                out.truncate(n);
                Value::List(Rc::new(out))
            }

            "randscale" => {
                let n = count(func, spec.params[0], &args[0])?;
                let tones = scale_tones(func, &args[1])?;
                let (lo, hi) = if args.len() == 2 { (60.0, 72.0) } else { (req(2)?, req(3)?) };
                if hi < lo {
                    return Err(format!("{func}: the range {lo}..{hi} is empty"));
                }
                if hi - lo > MAX_SCALE_SPAN {
                    return Err(format!(
                        "{func}: {lo}..{hi} spans {} semitones — a typo?",
                        hi - lo
                    ));
                }
                let notes = scale_notes(&tones, lo, hi);
                if notes.is_empty() {
                    return Err(format!(
                        "{func}: no tone of the scale falls between {lo} and {hi}"
                    ));
                }
                let mut out = Vec::with_capacity(n);
                for _ in 0..n {
                    let i = self.index(notes.len());
                    out.push(Value::Number(notes[i]));
                }
                Value::List(Item::all(out))
            }

            // Unreachable while the table and this match agree, which
            // `every_random_builtin_is_implemented` checks.
            _ => return Ok(None),
        };

        // The heavy-tailed draws are the reason this is here and not just a
        // tidy-up: a `cauchy` that lands on infinity would otherwise poison a
        // frequency and surface as silence a long way from the call.
        if let Value::Number(n) = out {
            if !n.is_finite() {
                return Err(format!("{func} produced {n}, which cannot be used as a value"));
            }
        }
        Ok(Some(out))
    }

    /// Shared by `gauss` and `humanize`, which differ only in where the centre
    /// comes from.
    fn normal(
        &mut self,
        func: &str,
        spec: &ListBuiltin,
        mean: f64,
        deviation: f64,
    ) -> Result<f64, String> {
        if deviation < 0.0 {
            return Err(format!(
                "{func}: {} cannot be negative, got {deviation}",
                spec.params[1]
            ));
        }
        let dist = Normal::new(mean, deviation)
            .map_err(|e| format!("{func}: {mean} and {deviation} do not make a shape — {e}"))?;
        Ok(dist.sample(&mut self.rng))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lowerer::lower::lower;
    use crate::parser::parser::parse;
    use crate::swync_graph::environment::Env;
    use crate::swync_graph::graph::SwyncGraph;
    use crate::swync_graph::ugen_nodes::NodeInput;

    /// A lowerer with a fixed seed, so a failure here is reproducible even
    /// though what it tests is not.
    fn lowerer() -> Lowerer {
        Lowerer {
            env: Env::new(),
            graph: SwyncGraph::default(),
            depth: 0,
            bindings: Vec::new(),
            play_start: 0.0,
            rng: SmallRng::seed_from_u64(0x5EED),
            // Nothing here reads a buffer.
            samples: Default::default(),
            choices: Vec::new(),
            meter: Default::default(),
            // Nothing here measures a wait in seconds.
            beat_secs: crate::lowerer::lower::DEFAULT_BEAT_SECS,
            octave: None,
            warnings: Vec::new(),
            clocks: Vec::new(),
        }
    }

    fn nums(args: &[f64]) -> Vec<Value> {
        args.iter().map(|n| Value::Number(*n)).collect()
    }

    /// `n` draws of `func`, all from one lowerer — so the RNG advances between
    /// them, as it does in a real program.
    fn draws(func: &str, args: &[f64], n: usize) -> Vec<f64> {
        let mut lw = lowerer();
        let args = nums(args);
        (0..n)
            .map(|_| match lw.random_builtin(func, &args) {
                Ok(Some(Value::Number(v))) => v,
                Ok(_) => panic!("{func} did not answer with a number"),
                Err(e) => panic!("{func} failed: {e}"),
            })
            .collect()
    }

    /// One list-valued draw, flattened to its numbers.
    fn list_draw(func: &str, args: &[Value]) -> Vec<f64> {
        match lowerer().random_builtin(func, args) {
            Ok(Some(Value::List(items))) => items
                .iter()
                .map(|i| match i.value {
                    Value::Number(n) => n,
                    _ => panic!("{func} produced a non-number element"),
                })
                .collect(),
            Ok(_) => panic!("{func} did not answer with a list"),
            Err(e) => panic!("{func} failed: {e}"),
        }
    }

    /// Fold a whole program and read back the constant its first node took.
    fn constant(src: &str) -> f64 {
        let items = parse(format!("{src}\n")).expect("parse failed");
        let g = lower(&items).expect("lower failed").graph;
        match g.nodes[0].inputs[0] {
            NodeInput::Const(v) => v,
            _ => panic!("expected a folded constant"),
        }
    }

    fn err(src: &str) -> String {
        let items = parse(format!("{src}\n")).expect("parse failed");
        match lower(&items) {
            Err(e) => e,
            Ok(_) => panic!("expected `{src}` to fail"),
        }
    }

    // --- uniform ---

    #[test]
    fn rand_stays_inside_its_range() {
        for v in draws("rand", &[], 200) {
            assert!((0.0..1.0).contains(&v), "{v} is outside 0..1");
        }
        for v in draws("rand", &[60.0, 72.0], 200) {
            assert!((60.0..72.0).contains(&v), "{v} is outside 60..72");
        }
        // A range below zero is a range like any other.
        for v in draws("rand", &[-1.0, -0.5], 100) {
            assert!((-1.0..-0.5).contains(&v), "{v} is outside -1..-0.5");
        }
    }

    #[test]
    fn randi_is_whole_and_excludes_the_top() {
        let vs = draws("randi", &[60.0, 72.0], 400);
        for v in &vs {
            assert_eq!(v.fract(), 0.0, "{v} is not whole");
            assert!((60.0..72.0).contains(v), "{v} is outside 60..72");
        }
        // Both ends of the half-open range are reachable.
        assert!(vs.contains(&60.0), "the low bound never came up");
        assert!(vs.contains(&71.0), "the high bound never came up");
        assert!(!vs.contains(&72.0), "the excluded bound came up");
    }

    #[test]
    fn coin_honours_its_odds() {
        assert!(draws("coin", &[0.0], 50).iter().all(|v| *v == 0.0));
        assert!(draws("coin", &[1.0], 50).iter().all(|v| *v == 1.0));
        let even = draws("coin", &[], 200);
        assert!(even.iter().all(|v| *v == 0.0 || *v == 1.0));
        assert!(even.iter().any(|v| *v == 1.0) && even.iter().any(|v| *v == 0.0));
    }

    // --- shaped ---

    #[test]
    fn gauss_centres_on_its_mean() {
        let vs = draws("gauss", &[5.0, 1.0], 2000);
        let mean = vs.iter().sum::<f64>() / vs.len() as f64;
        // The standard error over 2000 draws is about 0.022, so 0.2 is a very
        // wide net — this catches a wrong centre, not an unlucky run.
        assert!((mean - 5.0).abs() < 0.2, "mean was {mean}");
        assert!(vs.iter().any(|v| *v < 5.0) && vs.iter().any(|v| *v > 5.0));
    }

    /// A deviation of zero is degenerate but not an error: it pins the value.
    #[test]
    fn gauss_with_no_deviation_is_its_mean() {
        assert!(draws("gauss", &[3.0, 0.0], 20).iter().all(|v| *v == 3.0));
    }

    #[test]
    fn humanize_stays_near_what_it_was_given() {
        let vs = draws("humanize", &[0.5, 0.01], 500);
        assert!(vs.iter().all(|v| (v - 0.5).abs() < 0.1), "a nudge went far too far");
        assert!(vs.iter().any(|v| *v != 0.5), "nothing was nudged at all");
    }

    #[test]
    fn expo_is_positive_and_averages_its_mean() {
        let vs = draws("expo", &[2.0], 4000);
        assert!(vs.iter().all(|v| *v > 0.0));
        let mean = vs.iter().sum::<f64>() / vs.len() as f64;
        assert!((mean - 2.0).abs() < 0.4, "mean was {mean}");
    }

    #[test]
    fn tri_stays_between_its_bounds() {
        for v in draws("tri", &[0.0, 10.0, 9.0], 500) {
            assert!((0.0..=10.0).contains(&v), "{v} is outside 0..10");
        }
    }

    #[test]
    fn cauchy_is_centred_but_wanders_far() {
        let vs = draws("cauchy", &[0.0, 1.0], 2000);
        let near = vs.iter().filter(|v| v.abs() < 1.0).count();
        // Half of a Cauchy falls within one spread of the median.
        assert!(near > 800 && near < 1200, "{near} of 2000 landed within one spread");
        // And the tail is the reason to reach for it over `gauss`.
        assert!(vs.iter().any(|v| v.abs() > 10.0), "nothing strayed");
    }

    #[test]
    fn pareto_never_falls_below_its_scale() {
        for v in draws("pareto", &[2.0, 1.5], 500) {
            assert!(v >= 2.0, "{v} is below the scale");
        }
    }

    #[test]
    fn poisson_counts_in_whole_numbers() {
        let vs = draws("poisson", &[4.0], 500);
        assert!(vs.iter().all(|v| v.fract() == 0.0 && *v >= 0.0));
        let mean = vs.iter().sum::<f64>() / vs.len() as f64;
        assert!((mean - 4.0).abs() < 0.6, "mean was {mean}");
    }

    // --- lists ---

    #[test]
    fn the_list_builtins_produce_the_length_asked_for() {
        assert_eq!(constant("sin(len(rands(8)))"), 8.0);
        assert_eq!(constant("sin(len(randis(5, 60, 72)))"), 5.0);
        assert_eq!(constant("sin(len(walk(12, 0, 0.1)))"), 12.0);
        assert_eq!(constant("sin(len(walki(3, 60, 2)))"), 3.0);
        assert_eq!(constant("sin(len(choices([1, 2, 3, 4], 2)))"), 2.0);
        assert_eq!(constant("sin(len(randscale(6, [0, 4, 7])))"), 6.0);
        // Nothing is not an error, it is an empty list.
        assert_eq!(constant("sin(len(rands(0)))"), 0.0);
    }

    #[test]
    fn rands_spreads_over_its_range() {
        let vs = list_draw("rands", &nums(&[400.0, 10.0, 20.0]));
        assert!(vs.iter().all(|v| (10.0..20.0).contains(v)));
        // Distinct draws, not one value repeated.
        assert!(vs.iter().any(|v| *v != vs[0]));
    }

    #[test]
    fn randis_is_whole_and_in_range() {
        let vs = list_draw("randis", &nums(&[200.0, 60.0, 64.0]));
        assert!(vs.iter().all(|v| v.fract() == 0.0 && (60.0..64.0).contains(v)));
    }

    #[test]
    fn a_walk_starts_where_it_says_and_steps_no_further() {
        let vs = list_draw("walk", &nums(&[100.0, 0.5, 0.05]));
        assert_eq!(vs[0], 0.5, "the walk did not start at its start");
        for pair in vs.windows(2) {
            assert!((pair[1] - pair[0]).abs() <= 0.05, "{pair:?} jumped further than a step");
        }
        // It is a walk, not a constant.
        assert!(vs.iter().any(|v| *v != 0.5));
    }

    #[test]
    fn walki_stays_on_the_semitone_grid() {
        let vs = list_draw("walki", &nums(&[200.0, 60.0, 2.0]));
        assert_eq!(vs[0], 60.0);
        for pair in vs.windows(2) {
            let step = pair[1] - pair[0];
            assert_eq!(step.fract(), 0.0, "{step} is not a whole step");
            assert!(step.abs() <= 2.0, "{step} is more than the step allows");
        }
        // Both directions and standing still are all reachable.
        assert!(vs.windows(2).any(|p| p[1] > p[0]));
        assert!(vs.windows(2).any(|p| p[1] < p[0]));
    }

    /// A zero step is degenerate but legal: the walk stands still.
    #[test]
    fn a_walk_of_no_step_holds_its_start() {
        assert!(list_draw("walk", &nums(&[10.0, 7.0, 0.0])).iter().all(|v| *v == 7.0));
        assert!(list_draw("walki", &nums(&[10.0, 7.0, 0.0])).iter().all(|v| *v == 7.0));
    }

    #[test]
    fn choices_never_repeats_an_element() {
        let source = Value::List(Item::all((0..12).map(|i| Value::Number(i as f64))));
        for _ in 0..20 {
            let vs = list_draw("choices", &[source.clone(), Value::Number(5.0)]);
            assert_eq!(vs.len(), 5);
            let mut sorted = vs.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            sorted.dedup();
            assert_eq!(sorted.len(), 5, "{vs:?} repeats an element");
            assert!(vs.iter().all(|v| (0.0..12.0).contains(v)));
        }
    }

    /// Taking the whole list is a shuffle, which is the boundary case that
    /// would break if the truncation were off by one.
    #[test]
    fn choosing_everything_is_a_permutation() {
        let source = Value::List(Item::all((0..6).map(|i| Value::Number(i as f64))));
        let mut vs = list_draw("choices", &[source, Value::Number(6.0)]);
        vs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(vs, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
    }

    #[test]
    fn randscale_only_draws_tones_of_the_scale() {
        let major = Value::List(Item::all(
            [0.0, 2.0, 4.0, 5.0, 7.0, 9.0, 11.0].map(Value::Number),
        ));
        let vs = list_draw(
            "randscale",
            &[Value::Number(300.0), major, Value::Number(48.0), Value::Number(72.0)],
        );
        for v in &vs {
            assert!((48.0..=72.0).contains(v), "{v} is outside 48..=72");
            let degree = (v % 12.0) as i64;
            assert!(
                [0, 2, 4, 5, 7, 9, 11].contains(&degree),
                "{v} is not a tone of the scale"
            );
        }
        // It spans the range rather than sitting in one octave.
        assert!(vs.iter().any(|v| *v < 60.0) && vs.iter().any(|v| *v > 60.0));
    }

    /// Every degree is equally likely — which is the reason this enumerates the
    /// scale rather than snapping a uniform draw. A pentatonic scale has uneven
    /// gaps, so snapping would visibly favour the notes with room around them.
    #[test]
    fn randscale_does_not_favour_the_sparse_degrees() {
        let pentatonic =
            Value::List(Item::all([0.0, 3.0, 5.0, 7.0, 10.0].map(Value::Number)));
        let vs = list_draw(
            "randscale",
            &[Value::Number(5000.0), pentatonic, Value::Number(60.0), Value::Number(71.0)],
        );
        for degree in [0, 3, 5, 7, 10] {
            let hits = vs.iter().filter(|v| (**v as i64 - 60) == degree).count();
            // One fifth of 5000 is 1000; a snapping implementation would be off
            // by far more than this band allows.
            assert!(hits > 800 && hits < 1200, "degree {degree} came up {hits} times");
        }
    }

    // --- seeding ---

    /// The point of the whole feature: the same seed replays the same take.
    #[test]
    fn a_seed_makes_an_eval_repeat() {
        let src = "seed(42)\nsin(rand(0, 1) + sum(rands(4)) + choice([1, 2, 3, 4, 5]))";
        assert_eq!(constant(src), constant(src));
        // And a different seed is a different take.
        let other = src.replace("seed(42)", "seed(43)");
        assert_ne!(constant(src), constant(&other));
    }

    /// `choice` and `scramble` predate this module and draw from the same RNG,
    /// so a seed has to reach them too — otherwise "pin the program" would pin
    /// only half of it.
    #[test]
    fn a_seed_reaches_the_list_builtins_too() {
        let src = "seed(9)\nsin(sum(scramble([1, 10, 100, 1000])) + choice([1, 2, 3]) * 0.5)";
        assert_eq!(constant(src), constant(src));
    }

    /// Without a seed each eval is its own roll. Two draws of a full-precision
    /// float colliding is not a thing that happens.
    #[test]
    fn without_a_seed_every_eval_is_fresh() {
        assert_ne!(constant("sin(rand())"), constant("sin(rand())"));
    }

    #[test]
    fn a_seed_answers_with_itself() {
        assert_eq!(constant("sin(seed(42))"), 42.0);
    }

    // --- spellings ---

    /// The same three spellings the math builtins have, since these share the
    /// dot operator with them.
    #[test]
    fn the_dot_and_pipe_forms_reach_the_same_builtin() {
        for src in ["60.rand(72)", "rand(60, 72)", "60 >> rand(72)", "60.randi(72)"] {
            let v = constant(&format!("sin({src})"));
            assert!((60.0..72.0).contains(&v), "`{src}` produced {v}");
        }
        assert_eq!(constant("sin(len(8.rands(0, 1)))"), 8.0);
    }

    // --- errors ---

    #[test]
    fn empty_ranges_are_reported() {
        assert!(err("sin(rand(5, 5))").contains("empty"));
        assert!(err("sin(rand(5, 0))").contains("empty"));
        assert!(err("sin(randi(3, 3))").contains("empty"));
        assert!(err("sin(len(rands(4, 1, 1)))").contains("empty"));
        assert!(err("sin(len(randis(4, 9, 2)))").contains("empty"));
    }

    #[test]
    fn whole_number_arguments_are_required_where_they_matter() {
        assert!(err("sin(randi(60.5, 72))").contains("whole"));
        assert!(err("sin(len(walki(4, 60, 1.5)))").contains("whole"));
        assert!(err("sin(len(rands(2.5)))").contains("whole"));
    }

    #[test]
    fn domain_errors_are_reported() {
        assert!(err("sin(coin(2))").contains("0..=1"));
        assert!(err("sin(coin(-1))").contains("0..=1"));
        assert!(err("sin(expo(0))").contains("above zero"));
        assert!(err("sin(pareto(0, 1))").contains("above zero"));
        assert!(err("sin(cauchy(0, 0))").contains("above zero"));
        assert!(err("sin(poisson(-1))").contains("above zero"));
        assert!(err("sin(gauss(0, -1))").contains("negative"));
        assert!(err("sin(len(walk(4, 0, -1)))").contains("negative"));
        assert!(err("sin(len(rands(-1)))").contains("negative"));
    }

    #[test]
    fn asking_for_more_than_a_list_holds_is_an_error() {
        let e = err("sin(len(choices([1, 2, 3], 5)))");
        assert!(e.contains("distinct"), "got: {e}");
    }

    #[test]
    fn a_scale_with_no_tone_in_range_is_reported() {
        let e = err("sin(len(randscale(4, [0], 61, 71)))");
        assert!(e.contains("no tone"), "got: {e}");
    }

    /// An absurd count would allocate for a very long time before failing, and
    /// the eval runs between a keystroke and a sound.
    #[test]
    fn an_absurd_count_is_refused_rather_than_allocated() {
        assert!(err("sin(len(rands(1000000000)))").contains("typo"));
        // `randscale` enumerates its range, which `count` does not bound.
        assert!(err("sin(len(randscale(1, [0], 0, 1000000000)))").contains("typo"));
    }

    /// Same rule as the math builtins: these fold, so a signal cannot be one of
    /// their arguments.
    #[test]
    fn a_signal_is_rejected() {
        assert!(err("sin(2).rand(1)").contains("compile-time number"));
        assert!(err("sin(rand(0, sin(2)))").contains("compile-time number"));
        assert!(err("sin(len(choices(sin(2), 1)))").contains("expects a list"));
    }

    #[test]
    fn wrong_arity_is_rejected() {
        assert!(err("sin(rand(1))").contains("expects"));
        assert!(err("sin(randi(1))").contains("expects"));
        assert!(err("sin(len(walk(4, 1)))").contains("expects"));
    }

    /// The three lines the README's Random Numbers section opens with, run for
    /// real — including the voice, which is where the "a new draw per note"
    /// claim actually lives.
    #[test]
    fn the_readme_examples_compile() {
        let src = "\
fn lead(n) = sin(n.m2h) * perc(0.01, 0.2)
fn bass(n, cut = 800) = lowpass(saw(n.m2h), cut, 1) * perc(0.01, 0.3)
fn snare(n) = noise() * perc(0.001, 0.1) * rand(0.7, 1)
let riff = [48, 51, 55, 48]
play(randis(8, 60, 72), lead)
play(riff, bass, cut: rands(4, 400, 2000))
";
        let items = parse(src.to_string()).expect("the README example should parse");
        let lowered = lower(&items).expect("the README example should lower");
        assert_eq!(lowered.bindings.len(), 2);

        let ins = crate::scheduler::voice::Instruments::from_program(&items);
        for name in ["lead", "bass", "snare"] {
            crate::scheduler::voice::build_voice(
                &ins, name, 60.0, &[], 0.5, crate::lowerer::lower::DEFAULT_BEAT_SECS,
                Default::default(),
            )
                .unwrap_or_else(|e| panic!("{name} should build a voice: {e}"));
        }
    }

    // --- the table ---

    /// Every name the editor completes must reach an arm here, or it would
    /// lower to "is not a function". Detected by the arm answering with
    /// anything other than the `Ok(None)` fallthrough.
    #[test]
    fn every_table_entry_is_handled() {
        for b in crate::lang::RANDOM_BUILTINS {
            let mut lw = lowerer();
            // Enough arguments to get past the match arm; whether it then
            // succeeds or errors on types is irrelevant here.
            let n = b.arities.iter().max().copied().unwrap_or(0);
            let args = vec![Value::Number(1.0); n];
            let handled = !matches!(lw.random_builtin(b.name, &args), Ok(None));
            assert!(handled, "{} is in the table but has no arm here", b.name);
        }
    }
}
