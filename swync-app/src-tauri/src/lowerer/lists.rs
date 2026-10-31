//! Compile-time list builtins.
//!
//! These all run during lowering and emit no nodes (except `sum`, which folds
//! numbers but emits `Add` nodes when the list holds signals). Lists are
//! immutable: every function returns a new list rather than mutating one.

use std::rc::Rc;

use rand::seq::SliceRandom;

use crate::swync_graph::environment::{Item, Value};
use crate::swync_graph::ugen_nodes::NodeKind;
use crate::lowerer::expr::{as_data, not_this_member, control_number};
use crate::lowerer::lower::Lowerer;

pub(crate) fn as_list(func: &str, v: &Value) -> Result<Rc<Vec<Item>>, String> {
    // `as_data` first, so a member declared as a list is one here: this is the
    // whole of what makes `61.scale(Scale.major)` read the offsets.
    match as_data(v) {
        Value::List(items) => Ok(items.clone()),
        _ => Err(not_this_member(v, &format!("{func} expects a list"))
            .unwrap_or_else(|| format!("{func} expects a list"))),
    }
}

/// A list's elements without their lengths, for the builtins that compute over
/// values rather than rearranging them. `sum` of `[1;4, 2]` is 3: a length says
/// how much of a *sequence* an element occupies, and arithmetic is not a
/// sequence.
fn as_values(func: &str, v: &Value) -> Result<Vec<Value>, String> {
    Ok(as_list(func, v)?.iter().map(|i| i.value.clone()).collect())
}

fn as_number(func: &str, what: &str, v: &Value) -> Result<f64, String> {
    // As in `math::number`: a list builtin folds during lowering, so a slider
    // here is the number it is standing at.
    if let Some(n) = control_number(v) {
        return Ok(n);
    }
    match as_data(v) {
        Value::Number(n) => Ok(*n),
        _ => Err(not_this_member(v, &format!("{func}: {what} must be a number"))
            .unwrap_or_else(|| format!("{func}: {what} must be a compile-time number"))),
    }
}

fn as_count(func: &str, what: &str, v: &Value) -> Result<i64, String> {
    let n = as_number(func, what, v)?;
    if n.fract() != 0.0 || !n.is_finite() {
        return Err(format!("{func}: {what} must be a whole number, got {n}"));
    }
    Ok(n as i64)
}

/// Check the argument count against the arities declared in `lang::LIST_BUILTINS`.
///
/// The allowed counts live in that table rather than at the call site so the
/// editor's signature help and this check cannot disagree.
fn arity(func: &str, args: &[Value]) -> Result<(), String> {
    check_arity(func, args, crate::lang::list_builtin(func))
}

/// Shared by the list and math builtins, which use the same spec shape.
pub(crate) fn check_arity(
    func: &str,
    args: &[Value],
    spec: Option<&crate::lang::ListBuiltin>,
) -> Result<(), String> {
    let Some(b) = spec else {
        // Only reachable if an arm has no matching table entry, which the
        // `every_*_builtin_is_*` tests rule out.
        return Err(format!("{func} is missing from the builtin table"));
    };

    let max = b.arities.iter().max().copied().unwrap_or(0);
    if b.arities.contains(&args.len()) || (b.variadic && args.len() >= max) {
        return Ok(());
    }

    let mut expected = b
        .arities
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(" or ");
    if b.variadic {
        expected = format!("at least {max}");
    }
    Err(format!(
        "{func} expects {expected} arguments, got {}",
        args.len()
    ))
}

/// A list built by a builtin, whose elements carry no length of their own.
fn list(items: Vec<Value>) -> Result<Option<Value>, String> {
    Ok(Some(Value::List(Item::all(items))))
}

/// A list rebuilt from elements that already had lengths — `rev`, `push`,
/// `split` and the like, where an element survives the operation and should
/// keep whatever `;` gave it.
fn relist(items: Vec<Item>) -> Result<Option<Value>, String> {
    Ok(Some(Value::List(Rc::new(items))))
}

impl Lowerer {
    pub fn list_builtin(&mut self, func: &str, args: &[Value]) -> Result<Option<Value>, String> {
        match func {
            // The written-out spelling of `'`, and the same value: `list(2, 3)`
            // is `'[2, 3]`. Two spellings because the two positions read
            // differently — a quote disappears into a bracket-heavy lane, and a
            // call reads better as the whole of one.
            "list" => {
                let nums = args
                    .iter()
                    .map(|v| match v {
                        Value::Number(n) => Ok(*n),
                        _ => Err("list takes numbers: `list(2, 3)`. Whatever else it \
                                  held would have to reach an instrument one note at \
                                  a time, which only numbers can do".to_string()),
                    })
                    .collect::<Result<Vec<f64>, String>>()?;
                Ok(Some(Value::Quoted(Rc::new(nums))))
            }

            "len" => {
                arity(func, args)?;
                Ok(Some(Value::Number(as_list(func, &args[0])?.len() as f64)))
            }

            // `zip(a, b)` pairs elements positionally: [[a0, b0], [a1, b1], ...].
            // Lengths must match — at lowering time a mismatch is a mistake, not
            // something to silently truncate.
            "zip" => {
                if args.is_empty() {
                    return Err("zip expects at least one list".into());
                }
                let lists = args
                    .iter()
                    .map(|v| match v {
                        Value::List(items) => Ok(items.clone()),
                        _ => Err("zip expects every argument to be a list".to_string()),
                    })
                    .collect::<Result<Vec<_>, _>>()?;

                let len = lists[0].len();
                if let Some(i) = lists.iter().position(|l| l.len() != len) {
                    return Err(format!(
                        "zip: argument {} has length {}, but argument 1 has length {}",
                        i + 1,
                        lists[i].len(),
                        len
                    ));
                }

                let rows = (0..len)
                    .map(|i| Value::List(Item::all(lists.iter().map(|l| l[i].value.clone()))))
                    .collect();
                list(rows)
            }

            // Layers rather than a sequence.
            //
            // Every argument is run through `to_pattern` and the result thrown
            // away, purely to refuse a non-pattern here rather than at the
            // `play` that eventually reads the stack. That is what the declared
            // `receives: Pattern` promises, and it puts the error on the call
            // that has the mistake in it.
            "stack" => {
                if args.is_empty() {
                    return Err("stack expects at least one pattern".into());
                }
                for (i, arg) in args.iter().enumerate() {
                    crate::lowerer::play::to_pattern(arg, self.meter)
                        .map_err(|e| format!("stack: layer {} is not a pattern — {e}", i + 1))?;
                }
                Ok(Some(Value::Stack(Rc::new(args.to_vec()))))
            }

            // A dot lengthens a written value by half. It lives here rather
            // than with the arithmetic because that dispatches on `f64`, and a
            // written value is a rational held exactly — the whole reason a
            // doubly-dotted note still lands on the right tuplet count.
            "dot" => {
                arity(func, args)?;
                let Value::Duration(b) = &args[0] else {
                    return Err(format!(
                        "{func} lengthens a written note value by half, and there is \
                         no such thing to lengthen here — write `q.dot` or `h.dot`"));
                };
                let dots = match args.get(1) {
                    None => 1,
                    Some(Value::Number(n)) if *n >= 1.0 && *n <= 8.0 && n.fract() == 0.0 => {
                        *n as u32
                    }
                    Some(_) => {
                        return Err(format!(
                            "{func}: the number of dots must be a whole number from 1 to 8"));
                    }
                };
                Ok(Some(Value::Duration(b.dotted(dots))))
            }

            "rev" => {
                arity(func, args)?;
                let items = as_list(func, &args[0])?;
                relist(items.iter().rev().cloned().collect())
            }

            // `[a, b, c]` -> `[a, b, c, c, b, a]`: the sequence, then its mirror.
            "palindrome" => {
                arity(func, args)?;
                let items = as_list(func, &args[0])?;
                let mut out: Vec<Item> = items.iter().cloned().collect();
                out.extend(items.iter().rev().cloned());
                relist(out)
            }

            // Rotation amount defaults to 1 and wraps, so `rotl(l, len(l))` is
            // the identity and a negative amount rotates the other way.
            "rotl" | "rotr" => {
                arity(func, args)?;
                let items = as_list(func, &args[0])?;
                if items.is_empty() {
                    return list(Vec::new());
                }
                let n = match args.get(1) {
                    None => 1,
                    Some(v) => as_count(func, "the rotation amount", v)?,
                };
                let n = if func == "rotr" { -n } else { n };
                let at = n.rem_euclid(items.len() as i64) as usize;
                let mut out: Vec<Item> = items[at..].to_vec();
                out.extend_from_slice(&items[..at]);
                relist(out)
            }

            "push" => {
                arity(func, args)?;
                let items = as_list(func, &args[0])?;
                let mut out: Vec<Item> = items.iter().cloned().collect();
                out.push(Item::plain(args[1].clone()));
                relist(out)
            }

            // Removes the last element. Lists are immutable, so nothing is
            // returned "off the top" — index the list for that.
            "pop" => {
                arity(func, args)?;
                let items = as_list(func, &args[0])?;
                if items.is_empty() {
                    return Err("pop: the list is already empty".into());
                }
                relist(items[..items.len() - 1].to_vec())
            }

            "sort" => {
                arity(func, args)?;
                let items = as_values(func, &args[0])?;
                let mut nums = items
                    .iter()
                    .map(|v| as_number(func, "every element", v))
                    .collect::<Result<Vec<_>, _>>()?;
                nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                list(nums.into_iter().map(Value::Number).collect())
            }

            "sum" => {
                arity(func, args)?;
                let items = as_values(func, &args[0])?;
                let mut it = items.iter();
                let Some(first) = it.next() else {
                    return Ok(Some(Value::Number(0.0)));
                };
                // combine folds numbers and emits Add nodes for signals, so a
                // list of oscillators sums into the graph just like `for` does.
                let mut acc = first.clone();
                for v in it {
                    acc = self.combine(NodeKind::Add, |a, b| a + b, acc, v.clone())?;
                }
                Ok(Some(acc))
            }

            // Chunks of `n`; a short final chunk is kept.
            "split" => {
                arity(func, args)?;
                let items = as_list(func, &args[0])?;
                let n = as_count(func, "the chunk size", &args[1])?;
                if n < 1 {
                    return Err(format!("split: the chunk size must be at least 1, got {n}"));
                }
                let chunks = items
                    .chunks(n as usize)
                    .map(|c| Value::List(Rc::new(c.to_vec())))
                    .collect();
                list(chunks)
            }

            "choice" => {
                arity(func, args)?;
                let items = as_list(func, &args[0])?;
                if items.is_empty() {
                    return Err("choice: the list is empty".into());
                }
                let i = self.index(items.len());
                Ok(Some(items[i].value.clone()))
            }

            // `wchoice(values, weights)` — parallel lists, like zip.
            "wchoice" => {
                arity(func, args)?;
                let items = as_list(func, &args[0])?;
                let weights = as_list(func, &args[1])?;
                if items.is_empty() {
                    return Err("wchoice: the list is empty".into());
                }
                if items.len() != weights.len() {
                    return Err(format!(
                        "wchoice: {} values but {} weights",
                        items.len(),
                        weights.len()
                    ));
                }
                let ws = weights
                    .iter()
                    .map(|w| as_number(func, "every weight", &w.value))
                    .collect::<Result<Vec<_>, _>>()?;
                if let Some(bad) = ws.iter().find(|w| **w < 0.0 || !w.is_finite()) {
                    return Err(format!("wchoice: weights must be finite and >= 0, got {bad}"));
                }
                let total: f64 = ws.iter().sum();
                if total <= 0.0 {
                    return Err("wchoice: the weights are all zero".into());
                }

                let mut point = self.unit() * total;
                for (i, w) in ws.iter().enumerate() {
                    point -= w;
                    if point < 0.0 {
                        return Ok(Some(items[i].value.clone()));
                    }
                }
                Ok(Some(items[items.len() - 1].value.clone()))
            }

            "scramble" => {
                arity(func, args)?;
                let items = as_list(func, &args[0])?;
                let mut out: Vec<Item> = items.iter().cloned().collect();
                out.shuffle(&mut self.rng);
                relist(out)
            }

            // `map(list, transform)` — the function applied to every element.
            //
            // Unlike `filter` nothing is asked of what comes back, so a list of
            // oscillators is as good as a list of numbers. That is the whole of
            // what this adds over `for`, which collects the same way but sums
            // instead as soon as its body is audio — leaving no other way to
            // hold several voices apart before mixing them.
            "map" => {
                arity(func, args)?;
                let items = as_list(func, &args[0])?;
                let Value::Function(def) = &args[1] else {
                    return Err("map: the second argument must be a function".into());
                };

                let mut out = Vec::with_capacity(items.len());
                for v in items.iter() {
                    out.push(self.apply("map", def.clone(), vec![v.value.clone()])?);
                }
                list(out)
            }

            // `filter(list, predicate)` — keeps elements the predicate answers
            // non-zero for. The predicate is an ordinary user `fn`.
            "filter" => {
                arity(func, args)?;
                let items = as_list(func, &args[0])?;
                let Value::Function(def) = &args[1] else {
                    return Err("filter: the second argument must be a function".into());
                };

                let mut out = Vec::new();
                for v in items.iter() {
                    let keep = self.apply("filter", def.clone(), vec![v.value.clone()])?;
                    match keep {
                        Value::Number(n) => {
                            if n != 0.0 {
                                out.push(v.clone());
                            }
                        }
                        _ => {
                            return Err(
                                "filter: the predicate must return a compile-time number".into()
                            );
                        }
                    }
                }
                relist(out)
            }

            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use crate::swync_graph::environment::Env;
    use crate::swync_graph::graph::SwyncGraph;

    fn lowerer() -> Lowerer {
        Lowerer {
            env: Env::new(),
            graph: SwyncGraph::default(),
            depth: 0,
            bindings: Vec::new(),
            play_start: 0.0,
            // Fixed, so a failure here is reproducible.
            rng: rand::rngs::SmallRng::seed_from_u64(0x9E37_79B9_7F4A_7C15),
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

    /// Every name in `lang::LIST_BUILTINS` must actually be handled here —
    /// otherwise the editor would complete a name that lowers to "not a
    /// function". Detected by the arm returning something other than the
    /// `Ok(None)` fallthrough.
    #[test]
    fn every_table_entry_is_handled() {
        for b in crate::lang::LIST_BUILTINS {
            let mut lowerer = lowerer();
            // One list argument is enough to get past the match arm; whether it
            // then succeeds or errors on arity/types is irrelevant here.
            let args = vec![Value::List(Item::all([Value::Number(1.0)]))];
            let handled = match lowerer.list_builtin(b.name, &args) {
                Ok(None) => false,
                Ok(Some(_)) | Err(_) => true,
            };
            assert!(handled, "{} is in the table but has no arm in list_builtin", b.name);
        }
    }

    /// Arity errors now read from the table. `rotl` taking 1 or 2 is the case
    /// that would regress if `arities` were flattened to a single number.
    #[test]
    fn arity_errors_list_every_accepted_count() {
        let items = Value::List(Item::all([Value::Number(1.0)]));
        let mut lowerer = lowerer();

        // Value has no Debug, so unwrap_err() is unavailable here.
        let err = |r: Result<Option<Value>, String>| match r {
            Err(e) => e,
            Ok(_) => panic!("expected an arity error"),
        };

        assert_eq!(
            err(lowerer.list_builtin("rotl", &[items.clone(), Value::Number(1.0), Value::Number(2.0)])),
            "rotl expects 1 or 2 arguments, got 3"
        );
        assert_eq!(
            err(lowerer.list_builtin("push", &[items])),
            "push expects 2 arguments, got 1"
        );
    }
}
