use crate::swync_graph::graph::SwyncGraph;
use crate::swync_graph::ugen_nodes::{NodeId, NodeInput, NodeKind, UGenNode};
use crate::lowerer::lower::{lower, DEFAULT_BEAT_SECS};
use crate::parser::parser::parse;
use NodeInput::{Const, Node};

fn lower_src(src: &str) -> Result<SwyncGraph, String> {
    let items = parse(src.to_string()).expect("parse failed");
    lower(&items).map(|l| l.graph)
}

fn node(kind: NodeKind, inputs: Vec<NodeInput>) -> UGenNode {
    UGenNode { kind, inputs, span: None }
}

/// `sin(220 * 2)` — arithmetic on numbers folds during lowering; no Mul node exists.
#[test]
fn constant_folding_inside_call() {
    let g = lower_src("sin(220 * 2)\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(440.0)])]);
    assert_eq!(g.output, Some(NodeId(0)));
}

/// `let a = sin(2); a * a` — one oscillator, referenced twice. Sharing, not duplication.
#[test]
fn let_binding_shares_one_node() {
    let g = lower_src("let a = sin(2)\na * a\n").unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(2.0)]),
        node(NodeKind::Mul, vec![Node(NodeId(0)), Node(NodeId(0))]),
    ]);
    assert_eq!(g.output, Some(NodeId(1)));
}

/// Each call site of a user fn expands to its own subgraph.
#[test]
fn function_calls_inline_separately() {
    let g = lower_src("fn voice(f) = sin(f) / 5\nvoice(220) + voice(330)\n").unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(220.0)]),
        node(NodeKind::Div, vec![Node(NodeId(0)), Const(5.0)]),
        node(NodeKind::Sin, vec![Const(330.0)]),
        node(NodeKind::Div, vec![Node(NodeId(2)), Const(5.0)]),
        node(NodeKind::Add, vec![Node(NodeId(1)), Node(NodeId(3))]),
    ]);
    assert_eq!(g.output, Some(NodeId(4)));
}

/// Two top-level signal expressions get summed into a single output.
#[test]
fn top_level_signals_sum_into_output() {
    let g = lower_src("sin(1)\nsin(2)\n").unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(1.0)]),
        node(NodeKind::Sin, vec![Const(2.0)]),
        node(NodeKind::Add, vec![Node(NodeId(0)), Node(NodeId(1))]),
    ]);
    assert_eq!(g.output, Some(NodeId(2)));
}

/// `a >> f(b)` means `f(a, b)` — the piped value becomes the first argument.
#[test]
fn chain_pipes_lhs_as_first_argument() {
    let g = lower_src("fn gain(x, amt) = x * amt\nsin(440) >> gain(5)\n").unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(440.0)]),
        node(NodeKind::Mul, vec![Node(NodeId(0)), Const(5.0)]),
    ]);
    assert_eq!(g.output, Some(NodeId(1)));
}

/// `a >> f` (bare identifier) is a zero-arg call receiving the piped value.
#[test]
fn chain_into_bare_identifier() {
    let g = lower_src("sin(4) >> saw\n").unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(4.0)]),
        node(NodeKind::Saw, vec![Node(NodeId(0))]),
    ]);
    assert_eq!(g.output, Some(NodeId(1)));
}

/// A defaulted parameter fills in when the argument is omitted.
#[test]
fn default_param_fills_missing_argument() {
    let g = lower_src("fn v(f = 220) = sin(f)\nv()\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(220.0)])]);
    assert_eq!(g.output, Some(NodeId(0)));
}

#[test]
fn unbound_name_is_an_error() {
    let err = lower_src("boo * 2\n").unwrap_err();
    assert!(err.contains("unbound name: boo"), "got: {err}");
}

#[test]
fn recursive_function_hits_depth_guard() {
    let err = lower_src("fn boom(x) = boom(x)\nboom(1)\n").unwrap_err();
    assert!(err.contains("depth"), "got: {err}");
}

#[test]
fn function_used_as_signal_is_an_error() {
    let err = lower_src("fn f(x) = x\nf + 1\n").unwrap_err();
    assert!(err.contains("function"), "got: {err}");
}

/// `for i in 1..=3 { sin(i * 110) }` unrolls into three oscillators, summed
/// left to right — the same graph `a + b + c` would produce. Every oscillator
/// is built before any of the sums, because the loop runs its body out in full
/// before deciding it is audio rather than a list.
#[test]
fn for_loop_unrolls_and_sums() {
    let g = lower_src("for i in 1..=3 { sin(i * 110) }\n").unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(110.0)]),
        node(NodeKind::Sin, vec![Const(220.0)]),
        node(NodeKind::Sin, vec![Const(330.0)]),
        node(NodeKind::Add, vec![Node(NodeId(0)), Node(NodeId(1))]),
        node(NodeKind::Add, vec![Node(NodeId(3)), Node(NodeId(2))]),
    ]);
    assert_eq!(g.output, Some(NodeId(4)));
}

/// A `for` is an expression, so it composes with chains like any other signal.
#[test]
fn for_loop_is_an_expression() {
    let g = lower_src("for i in 1..=2 { sin(i * 100) } >> lowpass(800, 1)\n").unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(100.0)]),
        node(NodeKind::Sin, vec![Const(200.0)]),
        node(NodeKind::Add, vec![Node(NodeId(0)), Node(NodeId(1))]),
        node(NodeKind::Lowpass, vec![Node(NodeId(2)), Const(800.0), Const(1.0)]),
    ]);
    assert_eq!(g.output, Some(NodeId(3)));
}

/// A body with no signal in it is values, not voices, so the loop collects
/// them. Adding them up is `sum`'s job, and saying so is the difference
/// between building a list and mixing one down.
#[test]
fn for_loop_over_constants_collects() {
    let g = lower_src("sin(sum(for i in 1..=4 { i }))\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(10.0)])]);

    // Unsummed it is a list, which is not something to listen to.
    let err = lower_src("sin(for i in 1..=4 { i })\n").unwrap_err();
    assert!(err.contains("cannot use a list as a signal"), "got: {err}");
}

/// The loop that could not be written before: values in, list out. Read back
/// through a lane, which is where a built list is most likely to be going.
#[test]
fn for_loop_builds_a_list() {
    let bs = bindings_of(&format!(
        "{BASS}fn cutoffs() = for i in 0..=3 {{ (i + 1) * 100 }}\n\
         play([220, 330], bass, cut: cutoffs())\n"));

    assert_eq!(
        bs[0].lanes[0].steps().values(),
        vec![Some(100.0), Some(200.0), Some(300.0), Some(400.0)]);
}

/// A `for` collecting a sequence may say how long one step of it is, and the
/// list it builds is then the list that was written out — which is the only way
/// a generated pattern reaches note values at all, since a list built by a
/// builtin has nowhere to put a `;`.
#[test]
fn for_loop_steps_may_be_given_a_length() {
    let built = bindings_of(&format!(
        "{BASS}play(for i in 0..=4 {{ 65;e }}, bass)\n"));
    let written = bindings_of(&format!(
        "{BASS}play([65;e, 65, 65, 65, 65], bass)\n"));

    assert_eq!(built[0].source.pattern().cloned().expect("a written pattern"), written[0].source.pattern().cloned().expect("a written pattern"));
    assert_eq!(built[0].bars, written[0].bars);
}

/// The length is read where the body is, so it may be computed from the
/// element the step was built from — the loop is one place, not two.
#[test]
fn a_step_may_be_as_long_as_the_element_it_came_from() {
    let built = bindings_of(&format!(
        "{BASS}play(for i in 1..=3 {{ 220 * i;i }}, bass)\n"));
    let written = bindings_of(&format!(
        "{BASS}play([220;1, 440;2, 660;3], bass)\n"));

    assert_eq!(built[0].source.pattern().cloned().expect("a written pattern"), written[0].source.pattern().cloned().expect("a written pattern"));
}

/// The length a `for` gives its steps is read the same way a written one is,
/// `;t` included — so a loop can build a sequence of tuplets, which is the one
/// shape that is most tedious to write out.
#[test]
fn a_for_loop_may_collect_tuplets() {
    let built = bindings_of(&format!(
        "{BASS}play(for i in 1..=2 {{ [60;q, 63, 67];t }}, bass)\n"));
    let written = bindings_of(&format!(
        "{BASS}play([[60;q, 63, 67];t, [60;q, 63, 67];t], bass)\n"));

    assert_eq!(built[0].source.pattern().cloned().expect("a written pattern"), written[0].source.pattern().cloned().expect("a written pattern"));
    assert_eq!(built[0].bars, written[0].bars);
}

/// A `;` is a step's length, so it means nothing where the loop is not
/// collecting steps: a loop over oscillators is summed into one voice, and
/// there is no sequence left to have measured.
#[test]
fn a_length_on_a_loop_over_audio_is_refused() {
    let err = lower_src("for i in 1..=3 { sin(i * 110);q }\n").unwrap_err();
    assert!(err.contains("audio"), "got: {err}");
}

/// Each iteration gets its own scope; the loop variable does not outlive it.
#[test]
fn loop_variable_does_not_leak() {
    let err = lower_src("for i in 1..=2 { sin(i) }\ni\n").unwrap_err();
    assert!(err.contains("unbound name: i"), "got: {err}");
}

/// A block body binds locals in its own scope and returns its tail expression.
#[test]
fn block_body_binds_locals() {
    let g = lower_src("for i in 1..=2 {\n  let f = i * 110\n  sin(f)\n}\n").unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(110.0)]),
        node(NodeKind::Sin, vec![Const(220.0)]),
        node(NodeKind::Add, vec![Node(NodeId(0)), Node(NodeId(1))]),
    ]);
    assert_eq!(g.output, Some(NodeId(2)));
}

/// A user function called from the loop body inlines once per iteration.
#[test]
fn for_loop_calls_user_function_per_iteration() {
    let g = lower_src("fn voice(f) = sin(f) / 4\nfor i in 1..=2 { voice(i * 200) }\n").unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(200.0)]),
        node(NodeKind::Div, vec![Node(NodeId(0)), Const(4.0)]),
        node(NodeKind::Sin, vec![Const(400.0)]),
        node(NodeKind::Div, vec![Node(NodeId(2)), Const(4.0)]),
        node(NodeKind::Add, vec![Node(NodeId(1)), Node(NodeId(3))]),
    ]);
    assert_eq!(g.output, Some(NodeId(4)));
}

/// Nested loops unroll to the product of their ranges.
#[test]
fn nested_for_loops_unroll() {
    let g = lower_src("for i in 1..=2 { for j in 1..=2 { sin(i * 100 + j) } }\n").unwrap();
    let sines: Vec<_> = g.nodes.iter()
        .filter(|n| n.kind == NodeKind::Sin)
        .map(|n| n.inputs.clone())
        .collect();
    assert_eq!(sines, vec![
        vec![Const(101.0)], vec![Const(102.0)],
        vec![Const(201.0)], vec![Const(202.0)],
    ]);
}

#[test]
fn empty_range_is_an_error() {
    let err = lower_src("for i in 3..=1 { sin(i) }\n").unwrap_err();
    assert!(err.contains("empty"), "got: {err}");
}

#[test]
fn runaway_unroll_is_an_error() {
    let err = lower_src("for i in 1..=100000 { sin(i) }\n").unwrap_err();
    assert!(err.contains("limit"), "got: {err}");
}

/// A chain may break before the `>>`. The closing brace must not pick up a
/// statement terminator when the next line continues the expression.
#[test]
fn chain_continues_across_a_newline() {
    let g = lower_src("for i in 1..=2 { sin(i * 100) }\n  >> lowpass(800, 1)\n").unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(100.0)]),
        node(NodeKind::Sin, vec![Const(200.0)]),
        node(NodeKind::Add, vec![Node(NodeId(0)), Node(NodeId(1))]),
        node(NodeKind::Lowpass, vec![Node(NodeId(2)), Const(800.0), Const(1.0)]),
    ]);
    assert_eq!(g.output, Some(NodeId(3)));
}

#[test]
fn if_true_lowers_only_the_then_branch() {
    let g = lower_src("if 1 { sin(220) } else { saw(220) }\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(220.0)])]);
    assert_eq!(g.output, Some(NodeId(0)));
}

/// The untaken branch emits nothing at all — not a node, not an orphan.
#[test]
fn if_false_lowers_only_the_else_branch() {
    let g = lower_src("if 0 { sin(220) } else { saw(220) }\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Saw, vec![Const(220.0)])]);
}

/// A missing `else` yields 0.0, the identity for the output sum.
#[test]
fn if_without_else_contributes_nothing_when_false() {
    let g = lower_src("if 0 { sin(220) }\n").unwrap();
    assert_eq!(g.nodes, vec![]);
    assert_eq!(g.output, None);
}

#[test]
fn else_if_chain() {
    let g = lower_src("if 0 { sin(1) } else if 1 { sin(2) } else { sin(3) }\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(2.0)])]);
}

#[test]
fn comparisons_fold_to_one_or_zero() {
    let g = lower_src("sin(1 < 2)\nsin(3 <= 2)\nsin(2 == 2)\nsin(2 != 2)\n").unwrap();
    let sines: Vec<_> = g.nodes.iter()
        .filter(|n| n.kind == NodeKind::Sin).map(|n| n.inputs.clone()).collect();
    assert_eq!(sines, vec![
        vec![Const(1.0)], vec![Const(0.0)], vec![Const(1.0)], vec![Const(0.0)],
    ]);
}

/// The real payoff: a branch chosen per iteration, resolved at lowering time.
#[test]
fn if_inside_for_selects_per_iteration() {
    let g = lower_src("for i in 1..=4 { if i % 2 == 0 { sin(i) } else { saw(i) } }\n").unwrap();
    let kinds: Vec<_> = g.nodes.iter()
        .filter(|n| n.kind != NodeKind::Add)
        .map(|n| (n.kind.clone(), n.inputs.clone())).collect();
    assert_eq!(kinds, vec![
        (NodeKind::Saw, vec![Const(1.0)]),
        (NodeKind::Sin, vec![Const(2.0)]),
        (NodeKind::Saw, vec![Const(3.0)]),
        (NodeKind::Sin, vec![Const(4.0)]),
    ]);
}

/// `else` on its own line must not pick up a statement terminator.
#[test]
fn multiline_else_parses() {
    let g = lower_src("if 0 {\n  sin(1)\n}\nelse {\n  sin(2)\n}\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(2.0)])]);
}

#[test]
fn comparison_precedence_below_arithmetic() {
    let g = lower_src("if 1 + 1 == 2 { sin(7) }\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(7.0)])]);
}

/// Adding `>` must not steal the pipeline operator.
#[test]
fn shift_right_still_beats_greater_than() {
    let g = lower_src("sin(4) >> saw\n").unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(4.0)]),
        node(NodeKind::Saw, vec![Node(NodeId(0))]),
    ]);
}

#[test]
fn signal_condition_is_an_error() {
    let err = lower_src("if sin(2) { sin(1) }\n").unwrap_err();
    assert!(err.contains("compile-time number"), "got: {err}");
}

/// Audio-rate choice is arithmetic, not control flow — no new NodeKind needed.
#[test]
fn audio_rate_select_needs_no_new_nodes() {
    let g = lower_src(
        "fn select(g, a, b) = a * g + b * (1 - g)\nselect(sin(2), saw(110), square(110))\n"
    ).unwrap();
    let kinds: Vec<_> = g.nodes.iter().map(|n| n.kind.clone()).collect();
    assert_eq!(kinds, vec![
        NodeKind::Sin, NodeKind::Saw, NodeKind::Square,
        NodeKind::Mul, NodeKind::Sub, NodeKind::Mul, NodeKind::Add,
    ]);
}

/// `let ... in` binds for the body only, and shares the node rather than
/// rebuilding it.
#[test]
fn let_expr_shares_one_node() {
    let g = lower_src("fn voice(f) = let e = sin(f) in e * e\nvoice(220)\n").unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(220.0)]),
        node(NodeKind::Mul, vec![Node(NodeId(0)), Node(NodeId(0))]),
    ]);
}

/// The binding is scoped to the body.
///
/// Named `x` rather than `a`: a letter that is also a note answers with the
/// octave-less-note error instead, which proves the same thing about scoping
/// but says it in words this test would then be pinning for no reason.
#[test]
fn let_expr_does_not_leak() {
    let err = lower_src("let x = 2 in sin(x)\nsin(x)\n").unwrap_err();
    assert!(err.contains("unbound name: x"), "got: {err}");
}

/// The value is evaluated in the enclosing scope — `let` is not `letrec`.
#[test]
fn let_expr_value_sees_the_outer_binding() {
    let g = lower_src("let a = 2\nsin(let a = a * 10 in a)\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(20.0)])]);
}

/// Reordering the choices must not disturb the statement form of `let`.
#[test]
fn block_let_statement_still_works() {
    let g = lower_src("{\n  let a = 220\n  sin(a)\n}\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(220.0)])]);
}

#[test]
fn let_expr_works_at_top_level_and_in_blocks() {
    assert!(lower_src("let a = 220 in sin(a)\n").is_ok());
    assert!(lower_src("{ let a = 220 in sin(a) }\n").is_ok());
}

#[test]
fn for_over_a_list_literal() {
    let g = lower_src("for f in [220, 277, 330] { sin(f) }\n").unwrap();
    let sines: Vec<_> = g.nodes.iter().filter(|n| n.kind == NodeKind::Sin)
        .map(|n| n.inputs.clone()).collect();
    assert_eq!(sines, vec![vec![Const(220.0)], vec![Const(277.0)], vec![Const(330.0)]]);
}

#[test]
fn list_can_be_bound_and_indexed() {
    let g = lower_src("let scale = [220, 247, 277]\nsin(scale[2])\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(277.0)])]);
}

/// A range is a value, not special syntax bolted onto `for`.
#[test]
fn range_is_a_list_too() {
    let g = lower_src("let r = 1..=3\nfor i in r { sin(i) }\n").unwrap();
    let sines: Vec<_> = g.nodes.iter().filter(|n| n.kind == NodeKind::Sin)
        .map(|n| n.inputs.clone()).collect();
    assert_eq!(sines, vec![vec![Const(1.0)], vec![Const(2.0)], vec![Const(3.0)]]);
}

/// Something the old `Num ..= Num` grammar could not express.
#[test]
fn range_bounds_can_be_expressions() {
    let g = lower_src("let n = 2\nfor i in 1..=n * 2 { sin(i) }\n").unwrap();
    let sines: Vec<_> = g.nodes.iter().filter(|n| n.kind == NodeKind::Sin)
        .map(|n| n.inputs.clone()).collect();
    assert_eq!(sines, vec![
        vec![Const(1.0)], vec![Const(2.0)], vec![Const(3.0)], vec![Const(4.0)],
    ]);
}

/// Lists hold Values, so a list of oscillators works.
#[test]
fn lists_hold_signals_not_just_numbers() {
    let g = lower_src("for v in [sin(110), saw(220)] { v / 2 }\n").unwrap();
    let kinds: Vec<_> = g.nodes.iter().map(|n| n.kind.clone()).collect();
    assert_eq!(kinds, vec![
        NodeKind::Sin, NodeKind::Saw, NodeKind::Div, NodeKind::Div, NodeKind::Add,
    ]);
}

#[test]
fn nested_index() {
    let g = lower_src("let chords = [[220, 277], [247, 311]]\nsin(chords[1][0])\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(247.0)])]);
}

#[test]
fn multiline_list_without_trailing_comma() {
    let g = lower_src("let s = [\n  220,\n  330\n]\nsin(s[0])\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(220.0)])]);
}

#[test]
fn list_in_signal_position_is_an_error() {
    let err = lower_src("sin([1, 2])\n").unwrap_err();
    assert!(err.contains("cannot use a list as a signal"), "got: {err}");
}

#[test]
fn index_out_of_bounds_is_an_error() {
    let err = lower_src("let s = [1, 2]\nsin(s[5])\n").unwrap_err();
    assert!(err.contains("out of bounds"), "got: {err}");
}

#[test]
fn iterating_a_non_list_is_an_error() {
    let err = lower_src("for i in 5 { sin(i) }\n").unwrap_err();
    assert!(err.contains("list or range"), "got: {err}");
}

/// `len` is a compile-time value, so it folds into whatever consumes it.
#[test]
fn len_of_a_list_folds() {
    let g = lower_src("sin(len([220, 247, 277]))\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(3.0)])]);
}

/// A range is a list, so `len` works on it too.
#[test]
fn len_of_a_range() {
    let g = lower_src("sin(len(1..=8))\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(8.0)])]);
}

/// List builtins go through the same call path, so chaining works.
#[test]
fn len_can_be_chained() {
    let g = lower_src("sin([1, 2, 3] >> len)\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(3.0)])]);
}

/// `len` composes with ranges to iterate a list by index.
#[test]
fn len_drives_a_range() {
    let g = lower_src("let s = [110, 220]\nfor i in 1..=len(s) { sin(s[i - 1]) }\n").unwrap();
    let sines: Vec<_> = g.nodes.iter().filter(|n| n.kind == NodeKind::Sin)
        .map(|n| n.inputs.clone()).collect();
    assert_eq!(sines, vec![vec![Const(110.0)], vec![Const(220.0)]]);
}

/// The reason zip exists: walk two lists in step.
#[test]
fn zip_pairs_two_lists() {
    let g = lower_src(
        "let fs = [110, 220]\nlet amps = [0.5, 0.25]\n\
         for p in zip(fs, amps) { sin(p[0]) * p[1] }\n"
    ).unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(110.0)]),
        node(NodeKind::Mul, vec![Node(NodeId(0)), Const(0.5)]),
        node(NodeKind::Sin, vec![Const(220.0)]),
        node(NodeKind::Mul, vec![Node(NodeId(2)), Const(0.25)]),
        node(NodeKind::Add, vec![Node(NodeId(1)), Node(NodeId(3))]),
    ]);
    assert_eq!(g.output, Some(NodeId(4)));
}

/// zip is variadic.
#[test]
fn zip_handles_three_lists() {
    let g = lower_src("for t in zip([1, 2], [3, 4], [5, 6]) { sin(t[0] + t[1] + t[2]) }\n").unwrap();
    let sines: Vec<_> = g.nodes.iter().filter(|n| n.kind == NodeKind::Sin)
        .map(|n| n.inputs.clone()).collect();
    assert_eq!(sines, vec![vec![Const(9.0)], vec![Const(12.0)]]);
}

/// Signals survive a zip — the rows hold whatever Values went in.
#[test]
fn zip_carries_signals() {
    let g = lower_src("for p in zip([sin(110)], [0.5]) { p[0] * p[1] }\n").unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(110.0)]),
        node(NodeKind::Mul, vec![Node(NodeId(0)), Const(0.5)]),
    ]);
}

#[test]
fn zip_rejects_length_mismatch() {
    let err = lower_src("for p in zip([1, 2, 3], [4, 5]) { sin(p[0]) }\n").unwrap_err();
    assert!(err.contains("length"), "got: {err}");
}

#[test]
fn zip_rejects_non_lists() {
    let err = lower_src("for p in zip([1, 2], 3) { sin(p[0]) }\n").unwrap_err();
    assert!(err.contains("every argument to be a list"), "got: {err}");
}

#[test]
fn len_rejects_non_lists() {
    let err = lower_src("sin(len(5))\n").unwrap_err();
    assert!(err.contains("len expects a list"), "got: {err}");
}

#[test]
fn len_rejects_wrong_arity() {
    let err = lower_src("sin(len([1], [2]))\n").unwrap_err();
    assert!(err.contains("1 argument"), "got: {err}");
}
/// A rest is a value, so it occupies a slot in a list like any element.
#[test]
fn rest_is_an_element_of_a_list() {
    let g = lower_src("sin(len([220, `, 330, `]))\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(4.0)])]);
}

/// Rests are only meaningful in patterns; using one as audio is an error.
#[test]
fn rest_used_as_a_signal_is_an_error() {
    let err = lower_src("sin(`)\n").unwrap_err();
    assert!(err.contains("rest"), "got: {err}");
}

#[test]
fn rest_in_arithmetic_is_an_error() {
    let err = lower_src("sin(220) * `\n").unwrap_err();
    assert!(err.contains("rest"), "got: {err}");
}

/// A bare rest contributes nothing, like any non-signal top-level value.
#[test]
fn bare_rest_produces_no_output() {
    let g = lower_src("`\n").unwrap();
    assert_eq!(g.nodes, vec![]);
    assert_eq!(g.output, None);
}

/// Rests must not confuse the newline-to-terminator pass.
#[test]
fn rests_survive_a_multiline_list() {
    let g = lower_src("sin(len([\n  220,\n  `,\n  330\n]))\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(3.0)])]);
}

/// Indexing reaches a rest, and it still refuses to be audio.
#[test]
fn indexing_a_rest_still_errors_as_a_signal() {
    let err = lower_src("let p = [220, `]\nsin(p[1])\n").unwrap_err();
    assert!(err.contains("rest"), "got: {err}");
}
#[test]
fn list_binding_then_statement() {
    let g = lower_src("let s = [220, 330]\nsin(s[0])\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(220.0)])]);
}

#[test]
fn pattern_shaped_list_binding() {
    let g = lower_src("let p = [220, `, 330, `]\nsin(len(p))\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(4.0)])]);
}

// ---- play / patterns ----

use crate::lowerer::lower::lower as lower_full;
use crate::pattern::pattern::{Pattern, Step};
use crate::pattern::patterns::LaneValues;
use crate::pattern::rate::Rate;

fn bindings_of(src: &str) -> Vec<crate::pattern::patterns::Binding> {
    let items = parse(src.to_string()).expect("parse failed");
    lower_full(&items).expect("lower failed").bindings
}

fn play_err(src: &str) -> String {
    let items = parse(src.to_string()).expect("parse failed");
    match lower_full(&items) {
        Err(e) => e,
        Ok(_) => panic!("expected an error"),
    }
}

#[test]
fn play_binds_a_pattern_to_an_instrument() {
    let bs = bindings_of("fn kick(f) = sin(f)\nplay([220, `, 330, `], kick)\n");
    assert_eq!(bs.len(), 1);
    assert_eq!(bs[0].target, "kick");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![
        Step::Value(220.0), Step::Rest, Step::Value(330.0), Step::Rest,
    ]));
}

/// Rate is a playback property of `play`, not a pattern transformation.
#[test]
fn play_rate_wraps_the_pattern() {
    let bs = bindings_of("fn kick(f) = sin(f)\nplay([220, 330], kick, 2)\n");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::Fast(Rate::Fixed(2.0),
        Box::new(Pattern::seq(vec![Step::Value(220.0), Step::Value(330.0)]))));
}

/// A fractional rate slows the pattern down.
#[test]
fn play_rate_below_one_slows_down() {
    let bs = bindings_of("fn kick(f) = sin(f)\nplay([220], kick, 0.5)\n");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::Fast(Rate::Fixed(0.5),
        Box::new(Pattern::seq(vec![Step::Value(220.0)]))));
}

/// Rate 1 is the default and adds no wrapper.
#[test]
fn omitted_rate_is_one() {
    let bs = bindings_of("fn kick(f) = sin(f)\nplay([220], kick)\n");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![Step::Value(220.0)]));
}

/// Nested lists subdivide their slot.
#[test]
fn nested_list_becomes_a_group() {
    let bs = bindings_of("fn kick(f) = sin(f)\nplay([220, [330, 440]], kick)\n");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![
        Step::Value(220.0),
        Step::Group(Box::new(Pattern::seq(vec![
            Step::Value(330.0), Step::Value(440.0),
        ]))),
    ]));
}

/// Layering is just two bindings — no `stack` needed.
#[test]
fn multiple_plays_layer() {
    let bs = bindings_of(
        "fn kick(f) = sin(f)\nfn hat(f) = saw(f)\n\
         play([220, `], kick)\nplay([880, 880, 880], hat)\n");
    assert_eq!(bs.len(), 2);
    assert_eq!(bs[0].target, "kick");
    assert_eq!(bs[1].target, "hat");
}

/// Patterns compose with the rest of the language.
#[test]
fn pattern_elements_are_ordinary_expressions() {
    let bs = bindings_of("fn kick(f) = sin(f)\nlet r = 110\nplay([r, r * 2, `], kick)\n");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![
        Step::Value(110.0), Step::Value(220.0), Step::Rest,
    ]));
}

/// `play` works through the pipe.
#[test]
fn play_accepts_a_piped_pattern() {
    let bs = bindings_of("fn kick(f) = sin(f)\n[220, 330] >> play(kick)\n");
    assert_eq!(bs[0].target, "kick");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![
        Step::Value(220.0), Step::Value(330.0),
    ]));
}

#[test]
fn play_pipes_with_a_rate() {
    let bs = bindings_of("fn kick(f) = sin(f)\n[220] >> play(kick, 4)\n");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::Fast(Rate::Fixed(4.0),
        Box::new(Pattern::seq(vec![Step::Value(220.0)]))));
}

/// Bindings can be generated in a loop.
#[test]
fn play_inside_a_for_makes_several_bindings() {
    let bs = bindings_of(
        "fn kick(f) = sin(f)\nfor i in 1..=3 { play([110 * i], kick, i) }\n");
    assert_eq!(bs.len(), 3);
    assert_eq!(bs[2].source.pattern().cloned().expect("a written pattern"), Pattern::Fast(Rate::Fixed(3.0),
        Box::new(Pattern::seq(vec![Step::Value(330.0)]))));
}

/// A program can have both a persistent graph and patterns.
#[test]
fn graph_and_bindings_coexist() {
    let items = parse("fn kick(f) = sin(f)\nplay([220], kick)\nsin(55) / 8\n".to_string())
        .unwrap();
    let out = lower_full(&items).unwrap();
    assert_eq!(out.bindings.len(), 1);
    assert!(out.graph.output.is_some(), "the drone should still reach the output");
}

/// `play` contributes nothing to the audio output itself.
#[test]
fn play_alone_produces_no_graph_output() {
    let items = parse("fn kick(f) = sin(f)\nplay([220], kick)\n".to_string()).unwrap();
    let out = lower_full(&items).unwrap();
    assert_eq!(out.graph.output, None);
}

// ---- play errors ----

#[test]
fn play_rejects_an_unknown_instrument() {
    let err = play_err("play([220], ghost)\n");
    assert!(err.contains("not a function"), "got: {err}");
}

#[test]
fn play_rejects_a_non_identifier_instrument() {
    let err = play_err("fn kick(f) = sin(f)\nplay([220], kick(1))\n");
    assert!(err.contains("plain function name"), "got: {err}");
}

#[test]
fn play_rejects_a_signal_in_a_pattern() {
    let err = play_err("fn kick(f) = sin(f)\nplay([sin(220)], kick)\n");
    assert!(err.contains("signal"), "got: {err}");
}

#[test]
fn play_rejects_a_non_positive_rate() {
    let err = play_err("fn kick(f) = sin(f)\nplay([220], kick, 0)\n");
    assert!(err.contains("positive"), "got: {err}");
}

#[test]
fn play_rejects_a_signal_rate() {
    let err = play_err("fn kick(f) = sin(f)\nplay([220], kick, sin(2))\n");
    assert!(err.contains("compile-time number"), "got: {err}");
}

// ---- accel ----

/// A rate that moves wraps the pattern the same way a number does; the whole
/// of the difference is in the rate it carries.
#[test]
fn accel_wraps_the_pattern_as_a_curve() {
    let bs = bindings_of("fn kick(f) = sin(f)\nplay([220], kick, accel(1, 3, 4))\n");
    assert_eq!(
        bs[0].source.pattern().cloned().expect("a written pattern"),
        Pattern::Fast(
            Rate::accel(1.0, 3.0, 4.0),
            Box::new(Pattern::seq(vec![Step::Value(220.0)]))
        )
    );
}

/// The number that makes an arrangement possible: a bounded section still has
/// a length, and it is the one the curve's own integral gives — eight passes
/// of a one-bar pattern accelerating 1x to 3x over four bars take exactly
/// those four bars, not the eight they would at 1x.
#[test]
fn a_section_that_speeds_up_still_knows_how_long_it_is() {
    let bs = bindings_of("fn kick(f) = sin(f)\nplayn([220], kick, 8, accel(1, 3, 4))\n");
    let bars = bs[0].bars.expect("a counted play is bounded");
    assert!((bars - 4.0).abs() < 1e-9, "got {bars}");
}

/// And so `.then` can place what follows: the next section opens where the
/// accelerating one actually stopped rather than where it would have at its
/// starting speed.
#[test]
fn then_starts_where_an_accelerating_section_ends() {
    let bs = bindings_of(
        "fn kick(f) = sin(f)\nfn snare(f) = sin(f)\n\
         playn([220], kick, 8, accel(1, 3, 4)).then(play_once([330], snare))\n",
    );
    assert_eq!(bs.len(), 2);
    assert!((bs[1].start - 4.0).abs() < 1e-9, "got {}", bs[1].start);
}

/// A rate curve is measured in bars of the pattern, so the two halves of the
/// language agree: `play_once` at a curve is one pass however fast it went.
#[test]
fn play_once_under_a_curve_is_still_one_pass() {
    let bs = bindings_of("fn kick(f) = sin(f)\nplay_once([220], kick, accel(2, 4, 1))\n");
    // One pass at a rate rising 2x to 4x over the first bar. Solving the
    // curve for where it has covered one whole pattern gives √2 - 1 bars —
    // sooner than the half-bar the opening 2x alone would take, because it
    // is already up to 2√2 by the time it gets there.
    let bars = bs[0].bars.expect("play_once is bounded");
    assert!((bars - (2f64.sqrt() - 1.0)).abs() < 1e-9, "got {bars}");
}

/// A curve that does not curve is the plain rate it describes, and lowers to
/// exactly what writing that number would have.
#[test]
fn an_accel_between_equal_rates_is_a_plain_rate() {
    let curved = bindings_of("fn kick(f) = sin(f)\nplay([220], kick, accel(2, 2, 4))\n");
    let plain = bindings_of("fn kick(f) = sin(f)\nplay([220], kick, 2)\n");
    assert_eq!(curved[0].source.pattern().cloned().expect("a written pattern"), plain[0].source.pattern().cloned().expect("a written pattern"));
}

/// `accel(1, 1, ...)` is rate 1, which needs no wrapper at all — the same
/// reading `omitted_rate_is_one` pins for the number.
#[test]
fn an_accel_that_is_no_change_at_all_adds_no_wrapper() {
    let bs = bindings_of("fn kick(f) = sin(f)\nplay([220], kick, accel(1, 1, 8))\n");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![Step::Value(220.0)]));
}

/// A pattern written in note values is already wrapped once — it is as long as
/// its values add up to rather than a bar — so a curve wraps it a second
/// time. The two clocks compose: four passes of a 3/4 bar is three bars of
/// pattern, and the curve says how long covering that takes.
#[test]
fn a_curve_composes_with_a_pattern_written_in_note_values() {
    let bs = bindings_of("fn kick(f) = sin(f)\nplayn([c4;q, e4, g4], kick, 4, accel(1, 2, 4))\n");
    let bars = bs[0].bars.expect("a counted play is bounded");

    // Solving `c + c²/8 = 3` — three bars of pattern under a rate rising 1x
    // to 2x over four bars — against the 3 flat it would take at 1x.
    assert!((bars - 2.324555320336759).abs() < 1e-9, "got {bars}");
}

/// Zero is not a slow pattern but a stopped one, and a bounded section at zero
/// would never reach the end it has to hand on from.
#[test]
fn accel_rejects_a_rate_of_zero_at_either_end() {
    for src in ["accel(0, 2, 4)", "accel(2, 0, 4)", "accel(-1, 2, 4)"] {
        let err = play_err(&format!("fn kick(f) = sin(f)\nplay([220], kick, {src})\n"));
        assert!(err.contains("positive rate"), "{src} got: {err}");
    }
}

#[test]
fn accel_rejects_a_signal_argument() {
    let err = play_err("fn kick(f) = sin(f)\nplay([220], kick, accel(1, sin(2), 4))\n");
    assert!(err.contains("compile-time number"), "got: {err}");
}

/// A rate is only meaningful as a rate. Anywhere else it is a value the rest
/// of the language has no reading for, and says so rather than being folded to
/// a number that would mean something else.
#[test]
fn a_rate_is_not_a_signal_or_a_step() {
    let err = play_err("fn kick(f) = sin(f)\nplay([accel(1, 2, 4)], kick)\n");
    assert!(err.contains("pattern cannot contain a rate"), "got: {err}");

    let err = play_err("sin(accel(1, 2, 4))\n");
    assert!(err.contains("cannot use a rate as a signal"), "got: {err}");
}

// ---- play_once / playn ----

/// `play` loops; the whole of the difference is the binding's `bars`.
#[test]
fn play_loops_forever() {
    let bs = bindings_of("fn kick(f) = sin(f)\nplay([220], kick)\n");
    assert_eq!(bs[0].bars, None);
}

#[test]
fn play_once_bounds_the_binding_to_one_bar() {
    let bs = bindings_of("fn kick(f) = sin(f)\nplay_once([220, 330], kick)\n");
    assert_eq!(bs[0].target, "kick");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![
        Step::Value(220.0), Step::Value(330.0),
    ]));
    assert_eq!(bs[0].bars, Some(1.0));
}

#[test]
fn playn_bounds_the_binding_to_its_count() {
    let bs = bindings_of("fn kick(f) = sin(f)\nplayn([220], kick, 4)\n");
    assert_eq!(bs[0].bars, Some(4.0));
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![Step::Value(220.0)]));
}

/// Repeats count passes of the pattern, so a rate that packs two passes into
/// each bar halves the number of bars they take.
#[test]
fn a_rate_shortens_the_window_it_speeds_up() {
    let bs = bindings_of("fn kick(f) = sin(f)\nplayn([220], kick, 4, 2)\n");
    assert_eq!(bs[0].bars, Some(2.0));
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::Fast(Rate::Fixed(2.0),
        Box::new(Pattern::seq(vec![Step::Value(220.0)]))));

    let bs = bindings_of("fn kick(f) = sin(f)\nplay_once([220], kick, 0.5)\n");
    assert_eq!(bs[0].bars, Some(2.0), "a half-speed pass takes two bars");
}

/// Both take a piped pattern and lanes, like `play`.
#[test]
fn the_bounded_plays_pipe_and_take_lanes() {
    let bs = bindings_of(&format!("{BASS}[220, 330] >> play_once(bass, cut: [400, 2000])\n"));
    assert_eq!(bs[0].bars, Some(1.0));
    assert_eq!(bs[0].lanes.len(), 1);

    let bs = bindings_of(&format!("{BASS}[220] >> playn(bass, 3, legato: 0.5)\n"));
    assert_eq!(bs[0].bars, Some(3.0));
    assert_eq!(bs[0].lanes.len(), 1);
}

#[test]
fn playn_needs_a_count() {
    let err = play_err("fn kick(f) = sin(f)\nplayn([220], kick)\n");
    assert!(err.contains("number of repeats"), "got: {err}");
}

#[test]
fn playn_rejects_a_count_below_one() {
    for src in ["playn([220], kick, 0)\n", "playn([220], kick, -2)\n"] {
        let err = play_err(&format!("fn kick(f) = sin(f)\n{src}"));
        assert!(err.contains("at least 1"), "got: {err}");
    }
}

#[test]
fn playn_rejects_a_signal_count() {
    let err = play_err("fn kick(f) = sin(f)\nplayn([220], kick, sin(2))\n");
    assert!(err.contains("compile-time number"), "got: {err}");
}

/// The count is an argument like any other, so the arity message has to say
/// four rather than `play`'s three.
#[test]
fn playn_reports_its_own_arity() {
    let err = play_err("fn kick(f) = sin(f)\nplayn([220], kick, 2, 1, 9)\n");
    assert!(err.contains("playn expects at most 4 arguments, got 5"), "got: {err}");
}

/// Errors name the function that was actually called.
#[test]
fn a_bounded_play_reports_under_its_own_name() {
    let err = play_err("fn kick(f) = sin(f)\nplay_once([220], kick, 0)\n");
    assert!(err.starts_with("play_once:"), "got: {err}");
}

// ---- lanes ----

const BASS: &str = "fn bass(n, cut = 800, amp = 1) = saw(n) * amp\n";

#[test]
fn a_lane_is_bound_as_a_pattern() {
    let bs = bindings_of(&format!("{BASS}play([220, 330], bass, cut: [400, 2000])\n"));

    assert_eq!(bs[0].lanes.len(), 1);
    assert_eq!(bs[0].lanes[0].name, "cut");
    assert_eq!(
        *bs[0].lanes[0].steps(),
        Pattern::seq(vec![Step::Value(400.0), Step::Value(2000.0)])
    );
}

// ---- lanes of quoted lists ----

/// The instrument the quoted-list tests bind against: a parameter it walks
/// rather than reads, which is the whole reason a lane would carry a list.
const POLY: &str = "fn poly(n, divs = [4], amp = 1) = for d in divs { saw(n * d) * amp }\n";

/// The values a lane of quoted lists carries, in the order the notes read them.
fn lane_lists(src: &str) -> Vec<Option<Vec<f64>>> {
    let bs = bindings_of(src);
    match &bs[0].lanes[0].values {
        LaneValues::Lists(vs) => vs.clone(),
        LaneValues::Steps(_) => panic!("this lane carries numbers, not quoted lists"),
    }
}

/// The point of the quote: without it these brackets are two notes' worth of
/// values, and with it they are one value both notes read.
#[test]
fn a_quoted_lane_is_one_value_where_the_same_brackets_would_be_two() {
    assert_eq!(
        lane_lists(&format!("{POLY}play([220, 330], poly, divs: '[2, 3])\n")),
        vec![Some(vec![2.0, 3.0])]
    );

    let bs = bindings_of(&format!("{POLY}play([220, 330], poly, divs: [2, 3])\n"));
    assert_eq!(*bs[0].lanes[0].steps(),
        Pattern::seq(vec![Step::Value(2.0), Step::Value(3.0)]));
}

/// A lane of lists is read exactly as a lane of numbers is — by note, wrapping
/// — so a pattern of them is the ordinary thing and one value is the case of a
/// lane one long.
#[test]
fn a_lane_of_quoted_lists_is_read_by_note_like_any_other() {
    assert_eq!(
        lane_lists(&format!("{POLY}play([220, 330], poly, divs: ['[2, 3], '[4]])\n")),
        vec![Some(vec![2.0, 3.0]), Some(vec![4.0])]
    );
}

/// `list` is the same value written out, so the two spellings bind alike.
#[test]
fn list_is_the_written_out_spelling_of_a_quote() {
    assert_eq!(
        lane_lists(&format!("{POLY}play([220], poly, divs: list(2, 3))\n")),
        lane_lists(&format!("{POLY}play([220], poly, divs: '[2, 3])\n"))
    );
}

/// A `;` counts notes here as it does in any lane — the difference is only
/// that what it repeats is a whole list.
#[test]
fn a_semicolon_in_a_lane_of_lists_counts_the_notes_a_value_covers() {
    assert_eq!(
        lane_lists(&format!("{POLY}play([220], poly, divs: ['[2, 3];2, '[4]])\n")),
        vec![Some(vec![2.0, 3.0]), Some(vec![2.0, 3.0]), Some(vec![4.0])]
    );
}

/// And a rest means what it means in a lane of numbers: this note is not told,
/// so the instrument's own default stands.
#[test]
fn a_rest_in_a_lane_of_lists_leaves_the_parameter_to_its_default() {
    assert_eq!(
        lane_lists(&format!("{POLY}play([220], poly, divs: ['[2, 3], `])\n")),
        vec![Some(vec![2.0, 3.0]), None]
    );
}

/// Refused where it is written rather than where it would be discovered: a
/// voice that fails to build halts its whole pattern, and a lane that were a
/// list on one note and a number on the next would do it a note into a bar.
#[test]
fn a_lane_cannot_mix_quoted_lists_with_plain_numbers() {
    let err = play_err(&format!("{POLY}play([220], poly, divs: ['[2, 3], 4])\n"));
    assert!(err.contains("has to be one"), "got: {err}");
}

/// The two reserved lanes are single numbers by definition — one scales a
/// length, the other places a voice — so there is nothing a list could mean.
#[test]
fn a_reserved_lane_refuses_a_quoted_list() {
    for lane in ["legato", "pan"] {
        let err = play_err(&format!("{POLY}play([220], poly, {lane}: '[1, 2])\n"));
        assert!(err.contains("quoted list has nothing to say"), "{lane}: {err}");
    }
}

/// Checked at the quote, where the mistake is, rather than one note at a time
/// on the scheduler thread — which has no way to report it but to stop.
#[test]
fn a_quoted_list_holds_numbers_and_says_so_when_it_does_not() {
    let err = play_err(&format!("{POLY}play([220], poly, divs: '[2, [3, 4]])\n"));
    assert!(err.contains("quoted list holds numbers"), "got: {err}");
}

/// A note sounds at one value, so the quote means nothing in the pattern
/// position — and the error says where it does mean something.
#[test]
fn a_pattern_cannot_contain_a_quoted_list() {
    let err = play_err(&format!("{POLY}play(['[2, 3]], poly)\n"));
    assert!(err.contains("pattern cannot contain a quoted list"), "got: {err}");
}

/// A scalar lane is just a one-step pattern, so `amp: 0.8` needs no special
/// case anywhere downstream.
#[test]
fn a_scalar_lane_is_a_one_step_pattern() {
    let bs = bindings_of(&format!("{BASS}play([220], bass, amp: 0.8)\n"));
    assert_eq!(*bs[0].lanes[0].steps(), Pattern::seq(vec![Step::Value(0.8)]));
}

#[test]
fn lanes_survive_the_pipe_form() {
    let bs = bindings_of(&format!("{BASS}[220, 330] >> play(bass, cut: [400, 2000])\n"));
    assert_eq!(bs[0].target, "bass");
    assert_eq!(bs[0].lanes.len(), 1);
}

/// `rate` speeds the pattern up and leaves the lanes alone. A lane is read by
/// position — the nth note takes the nth value — so it follows the notes at
/// whatever speed they go, and compressing it here would compress it twice.
#[test]
fn rate_speeds_the_pattern_and_not_the_lanes() {
    let bs = bindings_of(&format!("{BASS}play([220, 330], bass, 2, cut: [400, 2000])\n"));

    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::Fast(Rate::Fixed(2.0), Box::new(
        Pattern::seq(vec![Step::Value(220.0), Step::Value(330.0)]))));
    assert_eq!(*bs[0].lanes[0].steps(),
        Pattern::seq(vec![Step::Value(400.0), Step::Value(2000.0)]));
}

/// A sequenced section plays its pattern from the top, and its lane from its
/// first value — end to end, in the shape that found this: seventeen eighths
/// under a rising volume, placed eight bars in by a `.then`. The pass is 2.125
/// bars, which divides neither the bar nor the offset, so laid on the
/// transport's grid the section opened on its fourteenth note and the ramp
/// began three quarters of the way up before jumping back to the bottom.
#[test]
fn a_sequenced_section_starts_at_the_top_of_its_pattern() {
    use crate::pattern::pattern::Span;
    use crate::pattern::patterns::Patterns;

    let src = "\
fn breath(n, vol = 1) = sin(n) * vol
fn ramp() = play_once(for i in 0..=16 { 220;e }, breath, vol: 0..=16)
fn lead(n) = sin(n)
playn([220], lead, 8).then(ramp)
";
    let pats = Patterns { bindings: bindings_of(src), origin: 0.0, epoch: 0, choices: Vec::new() };
    let vols: Vec<f64> = (0..14)
        .flat_map(|bar| pats.query(Span::new(bar as f64, bar as f64 + 1.0)))
        .filter(|e| e.target == "breath")
        .map(|e| e.args.iter().find(|(n, _)| n == "vol").expect("vol lane").1.as_num().expect("a number lane"))
        .collect();

    assert_eq!(vols, (0..=16).map(|i| i as f64).collect::<Vec<_>>());
}

/// End to end, from source to scheduled events: a lane longer than the pattern
/// walks its whole list rather than being squeezed into the pattern's bar.
/// Twenty cutoffs under two notes take ten bars, and every one is heard.
#[test]
fn a_long_lane_is_played_through_from_source() {
    use crate::pattern::pattern::Span;
    use crate::pattern::patterns::Patterns;

    let bs = bindings_of(&format!("{BASS}play([220, 330], bass, cut: 1..=20)\n"));
    let pats = Patterns { bindings: bs, origin: 0.0, epoch: 0, choices: Vec::new() };

    let cuts: Vec<f64> = (0..10)
        .flat_map(|c| pats.query(Span::new(c as f64, c as f64 + 1.0)))
        .map(|e| e.args.iter().find(|(n, _)| n == "cut").expect("cut lane").1.as_num().expect("a number lane"))
        .collect();

    assert_eq!(cuts, (1..=20).map(|i| i as f64).collect::<Vec<_>>());

    // And then around again, in phase with the pattern.
    let next: Vec<f64> = pats.query(Span::new(10.0, 11.0))
        .iter()
        .map(|e| e.args.iter().find(|(n, _)| n == "cut").expect("cut lane").1.as_num().expect("a number lane"))
        .collect();
    assert_eq!(next, vec![1.0, 2.0]);
}

#[test]
fn legato_is_accepted_without_being_a_parameter() {
    let bs = bindings_of(&format!("{BASS}play([220], bass, legato: 0.4)\n"));
    assert_eq!(bs[0].lanes[0].name, "legato");
}

#[test]
fn pan_is_accepted_without_being_a_parameter() {
    let bs = bindings_of(&format!("{BASS}play([220], bass, pan: -1)\n"));
    assert_eq!(bs[0].lanes[0].name, "pan");
}

/// A pan lane is a pattern like any other, so a voice can be placed somewhere
/// different on every note.
#[test]
fn pan_may_be_a_pattern() {
    let bs = bindings_of(&format!("{BASS}play([220, 330], bass, pan: [-1, 1])\n"));
    assert_eq!(bs[0].lanes[0].steps().values(), vec![Some(-1.0), Some(1.0)]);
}

/// A lane shorter than the pattern is read by position and wraps, so a
/// two-value pan alternates note by note however many notes there are — the
/// spelling the reference gives for ping-pong percussion.
#[test]
fn a_short_pan_lane_alternates_across_the_pattern() {
    let bs = bindings_of(
        "fn hat() = noise()\nplay([\\, `, \\, `, \\, `, \\, `], hat, pan: [-0.8, 0.8])\n");
    assert_eq!(bs[0].lanes[0].steps().values(), vec![Some(-0.8), Some(0.8)]);
}

/// And a zero-parameter instrument takes one, which is most of a drum kit.
#[test]
fn pan_works_on_an_instrument_that_takes_nothing() {
    let bs = bindings_of("fn kick() = sin(50)\nplay([\\], kick, pan: 0)\n");
    assert_eq!(bs[0].lanes[0].name, "pan");
}

// ---- lane errors ----

#[test]
fn play_rejects_a_lane_the_instrument_has_no_parameter_for() {
    let err = play_err(&format!("{BASS}play([220], bass, cutt: 400)\n"));
    assert!(err.contains("no parameter named 'cutt'"), "got: {err}");
}

/// The pattern fills the first parameter, so naming it as a lane is a
/// contradiction rather than an override.
#[test]
fn play_rejects_a_lane_naming_the_first_parameter() {
    let err = play_err(&format!("{BASS}play([220], bass, n: 400)\n"));
    assert!(err.contains("first parameter"), "got: {err}");
}

#[test]
fn play_rejects_a_repeated_lane() {
    let err = play_err(&format!("{BASS}play([220], bass, cut: 400, cut: 900)\n"));
    assert!(err.contains("given twice"), "got: {err}");
}

#[test]
fn play_rejects_an_instrument_whose_parameter_nothing_fills() {
    let err = play_err("fn bass(n, cut) = saw(n)\nplay([220], bass)\n");
    assert!(err.contains("needs 'cut'"), "got: {err}");
}

/// Filling it by name is exactly what that error asks for.
#[test]
fn a_lane_satisfies_a_parameter_with_no_default() {
    let bs = bindings_of("fn bass(n, cut) = saw(n) * cut\nplay([220], bass, cut: 400)\n");
    assert_eq!(bs[0].lanes[0].name, "cut");
}

/// `legato` changes the note's length, so it can never reach a parameter —
/// which has to be said at bind time, not discovered by ear.
#[test]
fn play_rejects_an_instrument_with_a_legato_parameter() {
    let err = play_err("fn bass(n, legato = 1) = saw(n)\nplay([220], bass, legato: 0.5)\n");
    assert!(err.contains("sets the note's length"), "got: {err}");
}

/// The same for `pan`, which is spent on the finished voice.
#[test]
fn play_rejects_an_instrument_with_a_pan_parameter() {
    let err = play_err("fn bass(n, pan = 0) = saw(n)\nplay([220], bass, pan: 1)\n");
    assert!(err.contains("stereo field"), "got: {err}");
}

/// The reserved names are checked one lane at a time, so a pan alongside
/// ordinary lanes is refused on its own account.
#[test]
fn play_rejects_a_pan_parameter_among_other_lanes() {
    let err = play_err("fn bass(n, cut = 1, pan = 0) = saw(n) * cut\n\
                        play([220], bass, cut: 2, pan: 0)\n");
    assert!(err.contains("stereo field"), "got: {err}");
}

#[test]
fn play_rejects_a_signal_in_a_lane() {
    let err = play_err(&format!("{BASS}play([220], bass, cut: [sin(2)])\n"));
    assert!(err.contains("signal"), "got: {err}");
}

#[test]
fn play_rejects_a_positional_argument_after_a_lane() {
    let err = play_err(&format!("{BASS}play([220], cut: 400, bass)\n"));
    assert!(err.contains("must come before named"), "got: {err}");
}

// ---- list builtins ----

/// Lower `expr` and read back the constant it folded to.
fn num(src: &str) -> f64 {
    let g = lower_src(&format!("sin({src})\n")).expect("lower failed");
    match g.nodes[0].inputs[0] {
        Const(v) => v,
        _ => panic!("expected a folded constant"),
    }
}

/// Read a whole list back as numbers.
///
/// Binds the list once and reads every element out of that single binding —
/// re-evaluating the source per index would re-roll the random builtins.
fn nums(src: &str) -> Vec<f64> {
    let n = num(&format!("len({src})")) as usize;
    let mut prog = format!("let __l = {src}\n");
    for i in 0..n {
        prog.push_str(&format!("sin(__l[{i}])\n"));
    }
    let g = lower_src(&prog).expect("lower failed");
    g.nodes
        .iter()
        .filter(|nd| nd.kind == NodeKind::Sin)
        .map(|nd| match nd.inputs[0] {
            Const(v) => v,
            _ => panic!("expected a folded constant"),
        })
        .collect()
}

fn list_err(src: &str) -> String {
    lower_src(&format!("sin({src})\n")).unwrap_err()
}

#[test]
fn rev_reverses() {
    assert_eq!(nums("rev([1, 2, 3])"), vec![3.0, 2.0, 1.0]);
    assert_eq!(nums("rev([])"), Vec::<f64>::new());
}

#[test]
fn palindrome_mirrors() {
    assert_eq!(nums("palindrome([1, 2, 3])"), vec![1.0, 2.0, 3.0, 3.0, 2.0, 1.0]);
}

#[test]
fn rotate_left_and_right() {
    assert_eq!(nums("rotl([1, 2, 3, 4])"), vec![2.0, 3.0, 4.0, 1.0]);
    assert_eq!(nums("rotr([1, 2, 3, 4])"), vec![4.0, 1.0, 2.0, 3.0]);
    assert_eq!(nums("rotl([1, 2, 3, 4], 2)"), vec![3.0, 4.0, 1.0, 2.0]);
    assert_eq!(nums("rotr([1, 2, 3, 4], 2)"), vec![3.0, 4.0, 1.0, 2.0]);
}

/// Rotating by the length is the identity, and negatives go the other way.
#[test]
fn rotation_wraps() {
    assert_eq!(nums("rotl([1, 2, 3], 3)"), vec![1.0, 2.0, 3.0]);
    assert_eq!(nums("rotl([1, 2, 3], 4)"), nums("rotl([1, 2, 3], 1)"));
    assert_eq!(nums("rotl([1, 2, 3], -1)"), nums("rotr([1, 2, 3], 1)"));
    assert_eq!(nums("rotl([], 3)"), Vec::<f64>::new());
}

#[test]
fn push_and_pop() {
    assert_eq!(nums("push([1, 2], 3)"), vec![1.0, 2.0, 3.0]);
    assert_eq!(nums("pop([1, 2, 3])"), vec![1.0, 2.0]);
    assert!(list_err("pop([])").contains("empty"));
}

/// Lists are immutable: push returns a new one.
#[test]
fn push_does_not_mutate() {
    let g = lower_src("let a = [1, 2]\nlet b = push(a, 3)\nsin(len(a) * 10 + len(b))\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(23.0)])]);
}

#[test]
fn sort_ascending() {
    assert_eq!(nums("sort([3, 1, 2])"), vec![1.0, 2.0, 3.0]);
    assert_eq!(nums("sort([-1.5, 2, 0])"), vec![-1.5, 0.0, 2.0]);
    assert!(list_err("sort([1, [2]])").contains("number"));
}

#[test]
fn sum_folds_numbers() {
    assert_eq!(num("sum([1, 2, 3])"), 6.0);
    assert_eq!(num("sum([])"), 0.0);
}

/// A list of signals sums into the graph, like `for` does.
#[test]
fn sum_of_signals_emits_add_nodes() {
    let g = lower_src("sum([sin(110), sin(220), sin(330)])\n").unwrap();
    assert_eq!(g.nodes, vec![
        node(NodeKind::Sin, vec![Const(110.0)]),
        node(NodeKind::Sin, vec![Const(220.0)]),
        node(NodeKind::Sin, vec![Const(330.0)]),
        node(NodeKind::Add, vec![Node(NodeId(0)), Node(NodeId(1))]),
        node(NodeKind::Add, vec![Node(NodeId(3)), Node(NodeId(2))]),
    ]);
    assert_eq!(g.output, Some(NodeId(4)));
}

#[test]
fn split_chunks() {
    assert_eq!(num("len(split([1, 2, 3, 4], 2))"), 2.0);
    assert_eq!(nums("split([1, 2, 3, 4], 2)[0]"), vec![1.0, 2.0]);
    assert_eq!(nums("split([1, 2, 3, 4], 2)[1]"), vec![3.0, 4.0]);
    // A short final chunk is kept.
    assert_eq!(num("len(split([1, 2, 3], 2))"), 2.0);
    assert_eq!(nums("split([1, 2, 3], 2)[1]"), vec![3.0]);
    assert!(list_err("split([1, 2], 0)").contains("at least 1"));
}

#[test]
fn map_applies_the_transform_to_every_element() {
    let src = "fn up(n) = n + 12\n";
    let g = lower_src(&format!("{src}sin(len(map([60, 63], up)))\n")).unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(2.0)])]);

    let g = lower_src(&format!("{src}sin(map([60, 63], up)[0])\n")).unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(72.0)])]);

    let g = lower_src(&format!("{src}sin(map([60, 63], up)[1])\n")).unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(75.0)])]);
}

#[test]
fn map_reads_through_the_dot() {
    let g = lower_src("fn up(n) = n + 12\nsin([60, 63].map(up)[0])\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(72.0)])]);
}

#[test]
fn mapping_an_empty_list_is_empty() {
    let g = lower_src("fn up(n) = n + 12\nsin(len(map([], up)))\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(0.0)])]);
}

/// The reason `map` earns its place beside `for`: a `for` whose body is audio
/// sums, so the voices are gone by the time it answers. `map` keeps them apart.
#[test]
fn map_can_build_a_list_of_signals() {
    let g = lower_src("fn voice(n) = sin(n)\nmap([110, 220], voice)[1]\n").unwrap();
    let sines: Vec<_> = g.nodes.iter().filter(|n| n.kind == NodeKind::Sin).collect();
    assert_eq!(sines.len(), 2, "both voices should still be in the graph");
    assert_eq!(sines[1].inputs, vec![Const(220.0)]);
    // Indexing picked one out, so nothing summed them.
    assert!(!g.nodes.iter().any(|n| n.kind == NodeKind::Add));
}

#[test]
fn map_rejects_a_non_function() {
    let err = lower_src("sin(len(map([1, 2], 3)))\n").unwrap_err();
    assert!(err.contains("function"), "got: {err}");
}

#[test]
fn filter_keeps_matching_elements() {
    let src = "fn big(x) = x > 2\n";
    let g = lower_src(&format!("{src}sin(len(filter([1, 2, 3, 4], big)))\n")).unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(2.0)])]);

    let g = lower_src(&format!("{src}sin(filter([1, 2, 3, 4], big)[0])\n")).unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(3.0)])]);
}

#[test]
fn filter_rejects_a_non_function() {
    let err = lower_src("sin(len(filter([1, 2], 3)))\n").unwrap_err();
    assert!(err.contains("function"), "got: {err}");
}

#[test]
fn filter_rejects_a_signal_predicate_result() {
    let err = lower_src("fn p(x) = sin(x)\nsin(len(filter([1, 2], p)))\n").unwrap_err();
    assert!(err.contains("compile-time number"), "got: {err}");
}

// Random builtins: assert properties, since the seed changes per eval.

#[test]
fn choice_returns_a_member() {
    for _ in 0..20 {
        let v = num("choice([1, 2, 3])");
        assert!([1.0, 2.0, 3.0].contains(&v), "got {v}");
    }
    assert!(list_err("choice([])").contains("empty"));
}

#[test]
fn scramble_is_a_permutation() {
    for _ in 0..20 {
        let mut got = nums("scramble([1, 2, 3, 4, 5])");
        got.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(got, vec![1.0, 2.0, 3.0, 4.0, 5.0]);
    }
}

/// A zero weight is never selected.
#[test]
fn weighted_choice_respects_zero_weights() {
    for _ in 0..30 {
        let v = num("wchoice([1, 2, 3], [0, 1, 0])");
        assert_eq!(v, 2.0, "only the weighted element should be chosen");
    }
}

#[test]
fn weighted_choice_validates_its_arguments() {
    assert!(list_err("wchoice([1, 2], [1])").contains("weights"));
    assert!(list_err("wchoice([1, 2], [0, 0])").contains("zero"));
    assert!(list_err("wchoice([1, 2], [-1, 1])").contains(">= 0"));
}

/// Random choices advance the RNG, so repeated calls are independent.
#[test]
fn repeated_choices_are_not_all_identical() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..40 {
        seen.insert(num("choice([1, 2, 3, 4, 5, 6, 7, 8])").to_bits());
    }
    assert!(seen.len() > 1, "choice should vary across evals");
}

/// The list functions compose with patterns, which is the point of having them.
#[test]
fn list_builtins_feed_patterns() {
    let bs = bindings_of(
        "fn kick(f) = sin(f)\nplay(rotl(rev([110, 220, `, 330])), kick)\n");
    assert_eq!(bs.len(), 1);
    // rev -> [330, `, 220, 110]; rotl 1 -> [`, 220, 110, 330]
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![
        Step::Rest, Step::Value(220.0), Step::Value(110.0), Step::Value(330.0),
    ]));
}

/// A generated list is a pattern like any other — the reason `randis` and
/// friends answer with a list rather than with something of their own.
#[test]
fn random_lists_feed_patterns() {
    let bs = bindings_of("fn lead(n) = sin(n.m2h)\nplay(randis(4, 60, 72), lead)\n");
    assert_eq!(bs.len(), 1);
    let Pattern::Steps(slots) = &bs[0].source.pattern().cloned().expect("a written pattern") else { panic!("expected a sequence") };
    assert_eq!(slots.len(), 4);
    for slot in slots {
        let Step::Value(n) = slot.step else { panic!("expected a plain value") };
        assert!(n.fract() == 0.0 && (60.0..72.0).contains(&n), "{n} is not a note in range");
    }
}

/// The draw happens once, while the program is lowered — so the riff a pattern
/// is bound to is settled, and plays the same until the next eval. Something
/// re-rolled per bar would be a different feature, and not this one.
#[test]
fn a_random_pattern_is_settled_at_eval() {
    let bs = bindings_of("fn lead(n) = sin(n)\nplay(rands(6, 100, 900), lead)\n");
    let Pattern::Steps(slots) = &bs[0].source.pattern().cloned().expect("a written pattern") else { panic!("expected a sequence") };
    // Every step is already a number, not a thunk to be evaluated later.
    assert!(slots.iter().all(|s| matches!(s.step, Step::Value(_))));
    assert_eq!(slots.len(), 6);
}

/// Seeding reaches the scheduler too: the same program binds the same riff.
#[test]
fn a_seeded_pattern_binds_the_same_notes_twice() {
    let src = "fn lead(n) = sin(n)\nseed(1234)\nplay(randis(8, 48, 72), lead)\n";
    assert_eq!(bindings_of(src)[0].source.pattern().cloned().expect("a written pattern"), bindings_of(src)[0].source.pattern().cloned().expect("a written pattern"));
    let other = src.replace("seed(1234)", "seed(1235)");
    assert_ne!(bindings_of(src)[0].source.pattern().cloned().expect("a written pattern"), bindings_of(&other)[0].source.pattern().cloned().expect("a written pattern"));
}

// ---- triggers and zero-parameter instruments ----

/// `\` is a sounding step that carries no data.
#[test]
fn trigger_is_a_sounding_step() {
    let bs = bindings_of("fn kick() = sin(50)\nplay([\\, `, \\, \\], kick)\n");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![
        Step::Value(1.0), Step::Rest, Step::Value(1.0), Step::Value(1.0),
    ]));
}

/// Triggers subdivide like any other step.
#[test]
fn triggers_nest() {
    let bs = bindings_of("fn kick() = sin(50)\nplay([\\, [\\, \\]], kick)\n");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![
        Step::Value(1.0),
        Step::Group(Box::new(Pattern::seq(vec![
            Step::Value(1.0), Step::Value(1.0),
        ]))),
    ]));
}

/// Triggers and numbers mix freely in one pattern.
#[test]
fn triggers_and_numbers_mix() {
    let bs = bindings_of("fn k(f) = sin(f)\nplay([220, \\, `, 330], k)\n");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![
        Step::Value(220.0), Step::Value(1.0), Step::Rest, Step::Value(330.0),
    ]));
}

/// A trigger is pattern data, not audio.
#[test]
fn trigger_used_as_a_signal_is_an_error() {
    let err = lower_src("sin(\\)\n").unwrap_err();
    assert!(err.contains("trigger"), "got: {err}");
}

/// List builtins are value-agnostic, so they work on triggers too.
#[test]
fn triggers_survive_list_builtins() {
    let bs = bindings_of("fn kick() = sin(50)\nplay(rotl([\\, `, `, `]), kick)\n");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![
        Step::Rest, Step::Rest, Step::Rest, Step::Value(1.0),
    ]));
}

/// A trigger must not confuse the newline-to-terminator pass.
#[test]
fn trigger_across_a_newline() {
    let bs = bindings_of("fn kick() = sin(50)\nplay([\n  \\,\n  `\n], kick)\n");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![Step::Value(1.0), Step::Rest]));
}

// ---- note names ----

/// The anchors: middle C, concert A, and the value from the original request.
#[test]
fn note_names_use_the_standard_numbering() {
    assert_eq!(num("c4"), 60.0);
    assert_eq!(num("a4"), 69.0);
    assert_eq!(num("a1"), 33.0);
    assert_eq!(num("g3"), 55.0);
}

/// `a4` really is 440 Hz, so note names and `m2h` agree.
#[test]
fn note_names_agree_with_m2h() {
    assert!((num("a4.m2h") - 440.0).abs() < 1e-9);
    assert!((num("c4.m2h") - 261.625_565).abs() < 1e-4);
}

#[test]
fn sharps_and_flats_shift_one_semitone() {
    assert_eq!(num("a1"), 33.0);
    assert_eq!(num("as1"), 34.0);
    assert_eq!(num("af1"), 32.0);
    assert_eq!(num("cs4"), 61.0);
    assert_eq!(num("df4"), 61.0); // the same pitch, spelled two ways
}

/// Enharmonics across the octave boundary resolve correctly.
#[test]
fn enharmonics_cross_octaves() {
    assert_eq!(num("bs3"), num("c4"));
    assert_eq!(num("cf4"), num("b3"));
    assert_eq!(num("es4"), num("f4"));
    assert_eq!(num("ff4"), num("e4"));
}

#[test]
fn every_natural_note_in_an_octave() {
    let expected = [("c4", 60.0), ("d4", 62.0), ("e4", 64.0),
                    ("f4", 65.0), ("g4", 67.0), ("a4", 69.0), ("b4", 71.0)];
    for (name, midi) in expected {
        assert_eq!(num(name), midi, "{name}");
    }
}

/// Octaves span the MIDI range: c0 is 12, g9 is 127.
#[test]
fn octave_range_covers_midi() {
    assert_eq!(num("c0"), 12.0);
    assert_eq!(num("g9"), 127.0);
}

/// Outside a sequence there is no octave to carry, so a bare letter is an
/// error — which is what keeps short names usable as names.
#[test]
fn a_bare_letter_is_not_a_note_outside_a_sequence() {
    assert!(lower_src("sin(f)\n").unwrap_err().contains("without an octave"));
    assert!(lower_src("sin(as)\n").unwrap_err().contains("without an octave"));
    let g = lower_src("fn voice(f) = sin(f)\nvoice(220)\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(220.0)])]);
}

/// The octave carries to the notes after it, like a written length does.
#[test]
fn an_octave_carries_forward_through_a_sequence() {
    assert_eq!(nums("[a1, a, a, a]"), vec![33.0; 4]);
    // And it is the *last* octave written that carries, not the first.
    assert_eq!(nums("[c4, d4, g, c5, d, g]"),
               vec![60.0, 62.0, 67.0, 72.0, 74.0, 79.0]);
}

/// Accidentals carry the octave as plain letters do, enharmonics included.
///
/// The last case is the one worth pinning: an octave-less note *reads* the
/// register and does not set it, so `bs` running over the top of octave 3 into
/// 60 leaves the following `cf` still in 3 rather than following it up. Only a
/// spelled octave moves the register, which is what makes a line's octave
/// something the reader can find by looking backwards for a digit.
#[test]
fn a_carried_octave_takes_accidentals() {
    assert_eq!(nums("[c4, ef, g]"), vec![60.0, 63.0, 67.0]);
    assert_eq!(nums("[c4, gs, bf]"), vec![60.0, 68.0, 70.0]);
    assert_eq!(nums("[b3, bs, cf]"), vec![59.0, 60.0, 47.0]);
}

/// The register is the written sequence's, so it works in either paradigm —
/// nothing about carrying an octave is about rhythm.
#[test]
fn an_octave_carries_in_both_paradigms() {
    assert_eq!(nums("[a1;q, a, a, a]"), vec![33.0; 4]);
    assert_eq!(nums("[a1;2, a, a;3]"), vec![33.0; 3]);
}

/// A group inherits the octave around it and gives it back at the closing
/// bracket — the same rule a written length follows, so there is one thing to
/// learn rather than two opposite ones.
///
/// Summed because the point is what the notes inside came to: `[g, c5]` is
/// g4 and c5, so the inner `g` took octave 4 from outside the bracket, and the
/// outer `g` after it is g4 again rather than following the group up to 5.
#[test]
fn a_group_inherits_the_octave_but_does_not_export_it() {
    assert_eq!(nums("[c4, [g, c5].sum, g]"), vec![60.0, 67.0 + 72.0, 67.0]);
}

/// An octave-less note needs one to have been written first. Nothing defaults:
/// a sequence that opens on a bare letter is a mistake, not octave 4.
#[test]
fn a_sequence_must_open_on_a_spelled_octave() {
    let e = list_err("[a, b, c]");
    assert!(e.contains("without an octave"), "got: {e}");
}

/// Only a note *written* as the whole step opens the register. A computed pitch
/// lands in an octave nobody wrote down, so there is nothing there to carry.
#[test]
fn a_computed_pitch_does_not_set_the_octave() {
    let e = list_err("[c4.oct(1), g]");
    assert!(e.contains("without an octave"), "got: {e}");
    // The note that opened the register is still the one that carries, so an
    // expression between two notes does not disturb them.
    assert_eq!(nums("[c4, c4 + 12, g]"), vec![60.0, 72.0, 67.0]);
}

/// A binding shadows an octave-less note exactly as it shadows a spelled one,
/// and a shadowed letter cannot open a register either.
#[test]
fn bindings_shadow_octave_less_notes_too() {
    assert_eq!(nums("[c4, let a = 7 in a, a]"), vec![60.0, 7.0, 69.0]);
    let e = list_err("let a4 = 7 in [a4, g]");
    assert!(e.contains("without an octave"), "got: {e}");
}

/// A function body is not part of the sequence that called it. Its own bare
/// letters are unbound names, whatever octave the caller happened to be in.
#[test]
fn the_octave_does_not_reach_into_a_function_body() {
    let e = lower_src("fn pitch() = g\nsin([c4, pitch()][1])\n").unwrap_err();
    assert!(e.contains("without an octave"), "got: {e}");
}

/// `e` is the eighth note and also the note E. With an octave in force both
/// readings are live, so the step is refused rather than silently taking the
/// duration — which is the reading nobody means between two notes.
#[test]
fn a_note_that_is_also_a_written_value_is_refused() {
    let err = list_err("[c4, e, g]");
    assert!(err.contains("the note E"), "got: {err}");
    assert!(err.contains("`e4`"), "got: {err}");

    // Spelling the octave is the way out that means the note.
    assert_eq!(nums("[c4, e4, g]"), vec![60.0, 64.0, 67.0]);
    // And in the length position `e` is never a pitch, so an octave in force
    // does not disturb it — there is nothing there for a note to be.
    assert_eq!(nums("[c4;e, g;e]"), vec![60.0, 67.0]);
}

/// The beat reaches the persistent graph as an ordinary folded number, in both
/// the units a signal asks for it in. The default tempo is 120 bpm, so the
/// quarter is half a second long and comes round twice a second.
#[test]
fn the_beat_is_bound_in_the_persistent_graph() {
    assert_eq!(num("qvs"), 0.5);
    assert_eq!(num("qvh"), 2.0);
    // And it is a number like any other, so it folds with the arithmetic
    // around it rather than becoming a node: an eighth-note sweep is one
    // oscillator at 4 Hz.
    let g = lower_src("sin(qvh * 2)\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(4.0)])]);
}

/// The two are one number written twice, so nothing can drift between them —
/// and a name can still shadow either, as it can shadow a note.
#[test]
fn the_beat_in_hertz_is_the_inverse_of_the_beat_in_seconds() {
    assert_eq!(num("qvs * qvh"), 1.0);
    let g = lower_src("let qvs = 3\nsin(qvs)\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(3.0)])]);
}

/// Bindings shadow note names rather than colliding with them.
#[test]
fn bindings_shadow_note_names() {
    assert_eq!(num("c4"), 60.0);
    let g = lower_src("let c4 = 100\nsin(c4)\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(100.0)])]);

    let g = lower_src("fn f(a4) = sin(a4)\nf(7)\n").unwrap();
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(7.0)])]);
}

/// Builtin names beginning with a note letter must not be read as notes.
#[test]
fn builtin_names_are_not_notes() {
    assert!(lower_src("sin(abs)\n").unwrap_err().contains("unbound name: abs"));
    assert_eq!(num("(-3).abs"), 3.0);
    assert_eq!(num("2.pow(3)"), 8.0);
}

/// An out-of-range octave says so, rather than reporting an unbound name.
#[test]
fn an_impossible_octave_is_reported() {
    let e = lower_src("sin(c12)\n").unwrap_err();
    assert!(e.contains("octave 12"), "got: {e}");
    assert!(e.contains("0..=9"), "got: {e}");
}

/// Note names are numbers, so everything numeric works on them.
#[test]
fn note_names_compose_with_methods_and_patterns() {
    assert_eq!(num("c4.oct(1)"), 72.0);
    assert_eq!(num("c4.semi(7)"), num("g4"));
    assert_eq!(num("[c4, e4, g4][1]"), 64.0);

    let bs = bindings_of("fn lead(n) = sin(n.m2h)\nplay([c4, ef4, `, g4], lead)\n");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![
        Step::Value(60.0), Step::Value(63.0), Step::Rest, Step::Value(67.0),
    ]));
}

/// `semi` over a list moves the notes and leaves the rests, because a rest
/// marks a step where nothing sounds and there is no pitch there to move.
#[test]
fn transposing_a_list_moves_its_notes_and_leaves_its_rests() {
    let bs = bindings_of("fn lead(n) = sin(n.m2h)\nplay([c4, ef4, `, g4].semi(2), lead)\n");
    assert_eq!(bs[0].source.pattern().cloned().expect("a written pattern"), Pattern::seq(vec![
        Step::Value(62.0), Step::Value(65.0), Step::Rest, Step::Value(69.0),
    ]));
}

/// The notes survive the transposition, so the lengths they were written with
/// survive it too.
///
/// This is the whole reason `semi` takes a list rather than leaving the job to
/// `riff.map(up)`: a list a builtin *builds* carries no lengths, so going round
/// through `map` flattens `[c4;q, ef4;e]` into an even division of the bar and
/// the rhythm is gone. Measured in bars per pass, which is where that loss
/// shows up.
#[test]
fn transposing_a_list_keeps_the_lengths_its_notes_were_written_with() {
    let written = bindings_of("fn lead(n) = sin(n.m2h)\nplayn([c4;q, ef4;e], lead, 4)\n");
    let moved = bindings_of("fn lead(n) = sin(n.m2h)\nplayn([c4;q, ef4;e].semi(2), lead, 4)\n");
    // A quarter and an eighth is three eighths of a 4/4 bar, so four passes is
    // a bar and a half — not the four bars an even division would give.
    assert_eq!(written[0].bars, Some(1.5));
    assert_eq!(moved[0].bars, written[0].bars);
}

/// Every file in `examples/` must parse, lower, and realize.
///
/// A shipped example that does not compile is worse than no example, and these
/// are the first thing anyone reads. Realizing too, not just lowering, because
/// port/param mistakes only surface there.
#[test]
fn every_example_compiles_and_realizes() {
    let dir = std::path::Path::new("../examples");
    let mut checked = 0;

    for entry in std::fs::read_dir(dir).expect("examples/ should exist") {
        let path = entry.expect("a readable entry").path();
        if path.extension().map(|e| e != "swync").unwrap_or(true) {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&path).expect("a readable file");

        let items = parse(src.clone()).unwrap_or_else(|e| panic!("{name} failed to parse: {e}"));

        // Through the same pass the app runs, so an example that imports is
        // checked as the thing it will actually be: one file with the modules
        // it names folded into it. Its own path is what `use` resolves from.
        let workspace =
            crate::imports::Workspace::new(Some(path.display().to_string()), None);
        let items = crate::imports::expand(items, &src, &workspace)
            .unwrap_or_else(|e| panic!("{name} failed to resolve its imports: {e}"));

        // A file may name a pattern drawn in the side panel, which lives in a
        // project's `patterns.swync` — and `examples/` is not that project.
        // That is the one legitimate reason an example refers to a name it
        // never defines, so it is skipped rather than failed. Every other
        // kind of mistake — a
        // misspelled UGen, a bad arity, a wrong argument — reports differently
        // and still fails here.
        let lowered = match crate::lowerer::lower::lower(&items) {
            Ok(lowered) => lowered,
            Err(e) if e.starts_with("unbound name:") => {
                println!("{name}: skipped, needs a drawn pattern ({e})");
                continue;
            }
            Err(e) => panic!("{name} failed to lower: {e}"),
        };
        if crate::swync_graph::realizer::realize(&lowered.graph).is_err() {
            panic!("{name} failed to realize");
        }

        // An example that makes no sound is a broken example.
        assert!(
            !lowered.bindings.is_empty() || lowered.graph.output.is_some(),
            "{name} produces neither patterns nor a graph output"
        );

        // Every instrument a pattern names must exist and build.
        let instruments = crate::scheduler::voice::Instruments::from_program(&items);
        for binding in &lowered.bindings {
            // Lanes go in as the scheduler would send them: the value the first
            // note takes, so an example's named arguments are exercised too.
            use crate::pattern::patterns::{LaneArg, LaneValues};
            let lanes: Vec<(String, LaneArg)> = binding
                .lanes
                .iter()
                .filter(|l| l.name != crate::pattern::patterns::LEGATO)
                .filter_map(|l| match &l.values {
                    LaneValues::Steps(p) => p.values().first().copied().flatten()
                        .map(|v| (l.name.clone(), LaneArg::Num(v))),
                    LaneValues::Lists(vs) => vs.first().cloned().flatten()
                        .map(|v| (l.name.clone(), LaneArg::List(v))),
                })
                .collect();
            let voice = crate::scheduler::voice::build_voice(
                &instruments, binding.target.instrument().expect("an example plays an instrument"),
                60.0, &lanes, 0.5, DEFAULT_BEAT_SECS, Default::default());
            if voice.is_err() {
                panic!("{name}: instrument `{}` failed to build", binding.target);
            }
        }
        checked += 1;
    }

    assert!(checked > 0, "no examples were checked");
}

// ---- .then ----

/// The instruments every sequencing test below plays.
const SECTIONS: &str = "\
fn lead(n) = sin(n)
fn bass(n) = saw(n)
fn hat(n) = noise() * n
fn chorus() = play([c4, e4], lead)
fn tail() = play_once([c2], bass)
";

/// What follows starts where the previous one stopped.
#[test]
fn then_offsets_what_follows() {
    let bs = bindings_of(&format!(
        "{SECTIONS}playn([c3], bass, 4).then(chorus)\n"));
    assert_eq!(bs.len(), 2);

    assert_eq!(bs[0].target, "bass");
    assert_eq!(bs[0].start, 0.0);
    assert_eq!(bs[0].bars, Some(4.0));

    // The chorus opens exactly where the four bars ran out.
    assert_eq!(bs[1].target, "lead");
    assert_eq!(bs[1].start, 4.0);
    assert_eq!(bs[1].bars, None);
}

/// `play_once` is one bar, so what follows starts at bar 1.
#[test]
fn play_once_hands_over_after_one_bar() {
    let bs = bindings_of(&format!("{SECTIONS}play_once([c3], bass).then(chorus)\n"));
    assert_eq!(bs[1].start, 1.0);
}

/// Chaining: each link starts where the previous stopped, and the offsets add.
#[test]
fn then_chains() {
    let bs = bindings_of(&format!(
        "{SECTIONS}playn([c3], bass, 2).then(tail).then(chorus)\n"));
    assert_eq!(bs.len(), 3);
    assert_eq!(bs[0].start, 0.0);       // playn, 2 bars
    assert_eq!(bs[1].start, 2.0);       // tail: play_once, 1 bar
    assert_eq!(bs[2].start, 3.0);       // chorus, after both
}

/// A `.then` nested inside the section is relative to that section's start,
/// so the offsets compose rather than fighting.
#[test]
fn nested_then_offsets_compose() {
    let src = "\
fn lead(n) = sin(n)
fn bass(n) = saw(n)
fn inner() = play_once([c4], lead)
fn outer() = play_once([c3], bass).then(inner)
playn([c2], bass, 2).then(outer)
";
    let bs = bindings_of(src);
    assert_eq!(bs.len(), 3);
    assert_eq!(bs[0].start, 0.0);   // the playn
    assert_eq!(bs[1].start, 2.0);   // outer's own play_once
    assert_eq!(bs[2].start, 3.0);   // inner, one bar after that
}

/// `rate` packs repeats into fewer bars, and the handover follows.
#[test]
fn rate_shortens_the_wait() {
    let bs = bindings_of(&format!("{SECTIONS}playn([c3], bass, 4, 2).then(chorus)\n"));
    // Four passes at double rate is two bars.
    assert_eq!(bs[0].bars, Some(2.0));
    assert_eq!(bs[1].start, 2.0);
}

/// A section that starts several things hands over after the longest.
#[test]
fn a_section_with_several_plays_ends_with_the_last() {
    let src = "\
fn lead(n) = sin(n)
fn bass(n) = saw(n)
fn both() = {
  playn([c4], lead, 2)
  playn([c3], bass, 5)
}
fn after() = play_once([c2], bass)
play_once([c1], bass).then(both).then(after)
";
    let bs = bindings_of(src);
    // both's two plays start at bar 1; the longer runs 5, so `after` is at 6.
    assert_eq!(bs[1].start, 1.0);
    assert_eq!(bs[2].start, 1.0);
    assert_eq!(bs[3].start, 6.0);
}

/// A loop of plays finishes when its last one does.
#[test]
fn a_for_loop_of_plays_hands_over_after_all_of_them() {
    let src = "\
fn lead(n) = sin(n)
fn after() = play_once([c2], lead)
for i in 1..=3 { playn([c4], lead, i) }.then(after)
";
    let bs = bindings_of(src);
    assert_eq!(bs.len(), 4);
    // The longest of the three runs 3 bars.
    assert_eq!(bs[3].start, 3.0);
}

// ---- .then errors ----

#[test]
fn then_refuses_an_endless_play() {
    let e = play_err(&format!("{SECTIONS}play([c3], bass).then(chorus)\n"));
    assert!(e.contains("never finishes"), "got: {e}");
}

#[test]
fn then_refuses_a_non_play_receiver() {
    let e = play_err(&format!("{SECTIONS}(4).then(chorus)\n"));
    assert!(e.contains("left side must be a play"), "got: {e}");
}


#[test]
fn then_refuses_a_function_taking_parameters() {
    let e = play_err(&format!(
        "{SECTIONS}fn takes(x) = play([x], lead)\nplay_once([c3], bass).then(takes)\n"));
    assert!(e.contains("no parameters"), "got: {e}");
}

// ---- sections written out rather than named ----

/// Instrument, start and length of every binding: enough to pin an arrangement
/// exactly, and the form in which two spellings can be compared.
fn shape(src: &str) -> Vec<(String, f64, Option<f64>)> {
    bindings_of(src)
        .into_iter()
        .map(|b| (b.target.label(), b.start, b.bars))
        .collect()
}

/// The whole of the feature: a play written out inside `.then` opens where the
/// chain has reached, not at the origin where a bare `play` would land.
#[test]
fn a_play_written_out_is_placed_rather_than_left_at_the_origin() {
    let bs = bindings_of(&format!(
        "{SECTIONS}playn([c3], bass, 4).then(play([c4, e4], lead))\n"));
    assert_eq!(bs.len(), 2, "the section is placed once, not written twice");
    assert_eq!(bs[0].start, 0.0);
    assert_eq!(bs[1].target, "lead");
    assert_eq!(bs[1].start, 4.0);
}

/// Naming a section and writing it out are one mechanism, so they have to
/// agree binding for binding across every combinator that places one.
#[test]
fn a_section_written_out_matches_the_same_section_named() {
    let base = "playn([c3], bass, 2)";
    let cases = [
        (format!("{base}.then(chorus)"),
         format!("{base}.then(play([c4, e4], lead))")),
        (format!("{base}.then_after(2, tail)"),
         format!("{base}.then_after(2, play_once([c2], bass))")),
        (format!("{base}.overlap(1, tail)"),
         format!("{base}.overlap(1, play_once([c2], bass))")),
        (format!("{base}.with(tail)"),
         format!("{base}.with(play_once([c2], bass))")),
        (format!("{base}.then_n(tail, 3)"),
         format!("{base}.then_n(play_once([c2], bass), 3)")),
        ("at(8, chorus)".to_string(),
         "at(8, play([c4, e4], lead))".to_string()),
        ("seq(tail, tail)".to_string(),
         "seq(play_once([c2], bass), play_once([c2], bass))".to_string()),
    ];
    for (named, written) in cases {
        assert_eq!(
            shape(&format!("{SECTIONS}{named}\n")),
            shape(&format!("{SECTIONS}{written}\n")),
            "`{named}` and `{written}` should be the same music");
    }
}

/// Both spellings in one chain, which is the shape that sends people looking
/// for this: two variations of a phrase, back to back, neither worth a name.
#[test]
fn written_out_sections_chain_with_named_ones() {
    let bs = bindings_of(&format!(
        "{SECTIONS}playn([c3], bass, 2)\n  \
           .then(playn([c4], lead, 2))\n  \
           .then(tail)\n  \
           .then(playn([e4], lead, 3))\n"));
    assert_eq!(bs.len(), 4);
    assert_eq!(bs[0].start, 0.0);
    assert_eq!(bs[1].start, 2.0);
    assert_eq!(bs[2].start, 4.0);   // tail: play_once, one bar
    assert_eq!(bs[3].start, 5.0);
}

/// A chain inside a written-out section is relative to where that section was
/// placed, exactly as it is inside a `fn` — the offsets compose either way.
#[test]
fn a_chain_inside_a_written_out_section_composes() {
    let bs = bindings_of(&format!(
        "{SECTIONS}playn([c3], bass, 2).then(play_once([c2], bass).then(tail))\n"));
    assert_eq!(bs.len(), 3);
    assert_eq!(bs[1].start, 2.0);   // the inner section's own start
    assert_eq!(bs[2].start, 3.0);   // one bar further, not back at bar 1
}

/// `then_n` re-lowers its section once per pass, so a `rand` inside it is
/// drawn afresh — and a play written out is re-lowered just as a `fn` is. Were
/// it copied instead, the three passes would be one draw repeated and the two
/// spellings would part company.
#[test]
fn then_n_redraws_a_written_out_section_each_pass() {
    let drawn = "playn([c4], lead, 1, rand(1, 5))";
    let named = format!(
        "{SECTIONS}fn drawn() = {drawn}\nseed(7)\nplayn([c3], bass, 2).then_n(drawn, 3)\n");
    let written = format!(
        "{SECTIONS}seed(7)\nplayn([c3], bass, 2).then_n({drawn}, 3)\n");
    assert_eq!(shape(&named), shape(&written));

    let passes: Vec<_> = bindings_of(&written)[1..].iter().map(|b| b.bars).collect();
    assert_eq!(passes.len(), 3);
    assert!(passes[0] != passes[1] || passes[1] != passes[2],
            "the passes were drawn once and copied: {passes:?}");
}

/// A combinator *inside* the section — one that reasons about where the chain
/// has reached, rather than only about notes — still means the same in both
/// spellings. It has to: the section is lowered at its offset either way, so
/// there is only one thing for `quantize` to round.
#[test]
fn a_combinator_inside_a_written_out_section_matches_the_named_form() {
    let inner = "playn([c4], lead, 3, 2).quantize()";
    let named = format!(
        "{SECTIONS}fn rounded() = {inner}\n\
         playn([c3], bass, 2).then(rounded).then(tail)\n");
    let written = format!(
        "{SECTIONS}playn([c3], bass, 2).then({inner}).then(tail)\n");
    assert_eq!(shape(&named), shape(&written));
}

/// A written-out section carries everything a `play` call carries — its rate
/// and its named lanes included. Those are the parts that have to survive the
/// call being an argument rather than a statement.
#[test]
fn a_written_out_section_keeps_its_rate_and_lanes() {
    let bs = bindings_of(&format!(
        "{SECTIONS}fn wide(n, cut = 400) = lowpass(saw(n), cut, 1)\n\
         playn([c3], bass, 2).then(playn([c4, `, e4, `], wide, 4, 0.5, cut: 800))\n"));
    assert_eq!(bs.len(), 2);
    assert_eq!(bs[1].target, "wide");
    assert_eq!(bs[1].start, 2.0);
    assert_eq!(bs[1].lanes.len(), 1);
    assert_eq!(bs[1].lanes[0].name, "cut");
}

/// The combinators that must be able to run a section again say so, rather
/// than repeating `then`'s more permissive rule.
#[test]
fn the_combinators_that_rerun_a_section_still_want_a_fn() {
    for src in [
        "playn([c3], bass, 2).then_each([1, 2], playn([c4], lead, 1))",
        "playn([c3], bass, 2).rthen([play_once([c2], bass)])",
        "playn([c3], bass, 2).wthen([play_once([c2], bass)], [1])",
        "playn([c3], bass, 2).shuffle_then([play_once([c2], bass)])",
        "playn([c3], bass, 2).maybe(0.5, play_once([c2], bass))",
    ] {
        let e = play_err(&format!(
            "{SECTIONS}fn faster(x) = playn([c4], lead, x)\n{src}\n"));
        assert!(e.contains("expects a `fn`"), "`{src}` got: {e}");
        assert!(e.contains("runs its section afresh"), "`{src}` got: {e}");
    }
}

/// A play bound to a name has already sounded where it was written, so it is
/// not a section. Accepting it placed nothing at all: the notes stayed at the
/// origin and the `.then` read as though it had moved them, which is the same
/// music whether the `.then` is there or commented out.
#[test]
fn a_play_bound_to_a_name_is_not_a_section() {
    let e = play_err(&format!(
        "{SECTIONS}let named = playn([c4, e4], lead, 2)\n\
         playn([c3], bass, 4).then(named)\n"));
    assert!(e.contains("already been placed"), "got: {e}");
    assert!(e.contains("sounds at bar 0"), "got: {e}");
    assert!(e.contains("Wrap it in a `fn`"), "got: {e}");
}

/// Every combinator that places a section refuses the same thing, since they
/// all place it through `place`.
#[test]
fn no_combinator_places_a_play_bound_to_a_name() {
    for src in [
        "playn([c3], bass, 2).then(named)",
        "playn([c3], bass, 2).then_after(1, named)",
        "playn([c3], bass, 2).overlap(1, named)",
        "playn([c3], bass, 2).with(named)",
        "playn([c3], bass, 2).then_n(named, 2)",
        "at(8, named)",
        "seq(named, tail)",
    ] {
        let e = play_err(&format!(
            "{SECTIONS}let named = playn([c4, e4], lead, 2)\n{src}\n"));
        assert!(e.contains("already been placed"), "`{src}` got: {e}");
    }
}

/// The named `fn` the refusal points at is the spelling that works, and it
/// places the section exactly where the chain had reached.
#[test]
fn naming_the_section_as_a_fn_places_it() {
    let bs = bindings_of(&format!(
        "{SECTIONS}fn named() = playn([c4, e4], lead, 2)\n\
         playn([c3], bass, 4).then(named)\n"));
    assert_eq!(bs.len(), 2);
    assert_eq!(bs[0].start, 0.0);
    assert_eq!(bs[1].target, "lead");
    assert_eq!(bs[1].start, 4.0);
}

/// Reaching back is refused however it is spelled: a chain hung off a play that
/// was written earlier puts its new notes after *that* play, not where the
/// section is being placed.
#[test]
fn a_section_chained_off_an_older_play_is_refused() {
    let e = play_err(&format!(
        "{SECTIONS}let named = playn([c4], lead, 2)\n\
         playn([c3], bass, 4).then(named.then(tail))\n"));
    assert!(e.contains("already been placed"), "got: {e}");
}

/// The rule is about *placing* a section, not about naming a play at all: a
/// handle is still what a chain continues from, which is the whole reason to
/// bind one to a name.
#[test]
fn a_named_play_is_still_a_receiver() {
    let bs = bindings_of(&format!(
        "{SECTIONS}let intro = playn([c3], bass, 4)\nintro.then(chorus)\n"));
    assert_eq!(bs.len(), 2);
    assert_eq!(bs[1].target, "lead");
    assert_eq!(bs[1].start, 4.0);
}

/// A section is still only a `fn` or a play, and the refusal names both.
#[test]
fn then_refuses_what_is_neither_a_function_nor_a_play() {
    let e = play_err(&format!("{SECTIONS}play_once([c3], bass).then(4)\n"));
    assert!(e.contains("`fn` written by name"), "got: {e}");
    assert!(e.contains("play written out"), "got: {e}");
}

/// A section that never stops cannot be followed by anything.
#[test]
fn then_after_an_endless_section_is_refused() {
    let src = "\
fn lead(n) = sin(n)
fn endless() = play([c4], lead)
fn after() = play_once([c2], lead)
play_once([c3], lead).then(endless).then(after)
";
    let e = play_err(src);
    assert!(e.contains("never finishes"), "got: {e}");
}

// ---- play_all ----

/// Nothing is sequenced: the parts keep the start they already had.
#[test]
fn play_all_leaves_its_parts_where_they_were() {
    let bs = bindings_of(&format!(
        "{SECTIONS}play_all(playn([c3], bass, 4), play_once([c4], lead))\n"));
    assert_eq!(bs.len(), 2);
    assert_eq!((bs[0].start, bs[0].bars), (0.0, Some(4.0)));
    assert_eq!((bs[1].start, bs[1].bars), (0.0, Some(1.0)));
}

/// The point of the grouping: `.then` follows the longest part, not the last
/// one written.
#[test]
fn play_all_hands_over_after_its_longest_part() {
    let bs = bindings_of(&format!(
        "{SECTIONS}play_all(playn([c3], bass, 4), play_once([c4], lead)).then(chorus)\n"));
    assert_eq!(bs.len(), 3);
    assert_eq!(bs[2].target, "lead");   // chorus
    assert_eq!(bs[2].start, 4.0);
}

/// A group inside a section is offset with it, and the chain goes on past it.
#[test]
fn play_all_composes_with_then_in_both_directions() {
    let src = "\
fn lead(n) = sin(n)
fn bass(n) = saw(n)
fn middle() = play_all(playn([c4], lead, 2), play_once([c3], bass))
fn after() = play_once([c2], bass)
play_once([c1], bass).then(middle).then(after)
";
    let bs = bindings_of(src);
    assert_eq!(bs.len(), 4);
    assert_eq!(bs[0].start, 0.0);   // the leading play_once
    assert_eq!(bs[1].start, 1.0);   // middle's two, together
    assert_eq!(bs[2].start, 1.0);
    assert_eq!(bs[3].start, 3.0);   // after the longer of the two
}

/// Groups nest, and the outer one still ends with the last of everything.
#[test]
fn play_all_nests() {
    let bs = bindings_of(&format!(
        "{SECTIONS}play_all(play_all(play_once([c3], bass), playn([c4], lead, 3)), \
         play_once([c2], hat)).then(chorus)\n"));
    assert_eq!(bs.len(), 4);
    assert!(bs[..3].iter().all(|b| b.start == 0.0));
    assert_eq!(bs[3].start, 3.0);
}

/// One part that never stops makes the group never stop.
#[test]
fn play_all_with_an_endless_part_cannot_be_followed() {
    let e = play_err(&format!(
        "{SECTIONS}play_all(play([c3], bass), play_once([c4], lead)).then(chorus)\n"));
    assert!(e.contains("never finishes"), "got: {e}");
}

/// A second counted section, for the tests that need two. `chorus` above is
/// also the name of a UGen, which is only ever written bare here.
const VERSE: &str = "fn verse() = playn([c3], bass, 4)\n";

#[test]
fn play_all_refuses_an_argument_that_is_not_a_section() {
    let e = play_err(&format!("{SECTIONS}play_all(play_once([c3], bass), 4)\n"));
    assert!(e.contains("argument 2 is not a section"), "got: {e}");
}

/// A section is written the same two ways everywhere it is taken, `play_all`
/// included: `seq(verse, chorus)` and `play_all(verse, chorus)` name their
/// sections alike, and neither needs the parentheses the other does not.
#[test]
fn play_all_takes_a_section_by_name() {
    let named = bindings_of(&format!("{SECTIONS}{VERSE}play_all(verse, tail)\n"));
    let written = bindings_of(&format!("{SECTIONS}{VERSE}play_all(verse(), tail())\n"));

    assert_eq!(named.len(), written.len());
    for (a, b) in named.iter().zip(written.iter()) {
        assert_eq!(a.target, b.target);
        assert_eq!(a.start, b.start);
        assert_eq!(a.bars, b.bars);
    }
}

/// And it is a section wherever a section goes, so a group named inside a
/// `seq` opens where the `seq` placed it rather than at the origin.
#[test]
fn a_named_section_inside_a_group_is_placed_with_the_group() {
    let bs = bindings_of(&format!(
        "{SECTIONS}{VERSE}seq(tail, play_all(verse, tail))\n"));
    let intro = bs[0].bars.expect("a counted intro");

    assert!(intro > 0.0);
    for b in &bs[1..] {
        assert_eq!(b.start, intro, "every part of the group opens where the seq reached");
    }
}

/// A section that takes parameters is not one, whichever combinator is asking.
#[test]
fn play_all_refuses_a_section_that_needs_arguments() {
    let e = play_err(&format!(
        "{SECTIONS}fn faster(n) = playn([c4], lead, n)\nplay_all(tail, faster)\n"));
    assert!(e.contains("no parameters"), "got: {e}");
}

#[test]
fn play_all_needs_something_to_group() {
    let e = play_err(&format!("{SECTIONS}play_all()\n"));
    assert!(e.contains("at least one section"), "got: {e}");
}

/// A play handle is not audio and not pattern data.
#[test]
fn a_play_handle_is_not_a_value_to_compute_with() {
    let e = play_err(&format!("{SECTIONS}sin(play_once([c3], bass))\n"));
    assert!(e.contains("not audio"), "got: {e}");
}







// ---- samples ----

/// The buffer every test below writes, under the path they all name.
///
/// A ramp from -1 to 1, so what comes out of a `sample` node says where in the
/// buffer it was read from. Synthesized rather than decoded: what a file
/// *contains* is `samples`'s business, and this is about what the language
/// does with one once it has it.
fn sampled(src: &str) -> Result<crate::lowerer::lower::Lowered, String> {
    use std::sync::Arc;
    let mut wave = fundsp::wave::Wave::new(1, 1000.0);
    for i in 0..2000 {
        wave.push(i as f32 / 1000.0 - 1.0);
    }
    let samples = crate::samples::Samples::from_pairs(
        [("break.wav".to_string(), Arc::new(wave))]);

    let items = parse(src.to_string()).expect("parse failed");
    crate::lowerer::lower::lower_with_samples(&items, samples)
}

fn sample_err(src: &str) -> String {
    match sampled(src) {
        Err(e) => e,
        Ok(_) => panic!("expected an error"),
    }
}

#[test]
fn a_loaded_buffer_can_be_read_at_a_position() {
    let g = sampled("sample(load(\"break.wav\"), ramp(0.5))\n").expect("should lower").graph;

    assert_eq!(g.samples.len(), 1, "the buffer should be in the graph");
    let read = g.nodes.iter().find(|n| n.kind == NodeKind::Sample).expect("a Sample node");
    // The position is wired; the buffer index and channel are baked in.
    assert!(matches!(read.inputs[0], Node(_)), "the position should be a wired input");
    assert_eq!(read.inputs[1], Const(0.0), "buffer 0");
    assert_eq!(read.inputs[2], Const(0.0), "channel 0 by default");
}

/// `secs` is known while lowering, which is what lets it divide into a `ramp`
/// frequency rather than having to be measured at audio rate.
#[test]
fn a_buffers_length_is_a_compile_time_number() {
    // 2000 frames at 1000 Hz is two seconds, so the phasor is 0.5 Hz — and
    // folds to a constant rather than becoming a Div node.
    let g = sampled("ramp(1 / load(\"break.wav\").secs)\n").expect("should lower").graph;
    assert_eq!(g.nodes, vec![node(NodeKind::Ramp, vec![Const(0.5)])]);
}

#[test]
fn a_buffers_channel_count_is_a_number() {
    let g = sampled("sin(load(\"break.wav\").channels * 100)\n").expect("should lower").graph;
    assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(100.0)])]);
}

/// One file read four ways is one buffer. A break chopped sixteen times should
/// not be sixteen copies of the audio.
#[test]
fn one_file_read_many_times_is_stored_once() {
    let src = "\
let b = load(\"break.wav\")
sample(b, ramp(1)) + sample(b, ramp(2)) + sample(load(\"break.wav\"), ramp(4))
";
    let g = sampled(src).expect("should lower").graph;
    assert_eq!(g.samples.len(), 1, "one file, one buffer");
    assert_eq!(g.nodes.iter().filter(|n| n.kind == NodeKind::Sample).count(), 3);
}

/// The chosen shape: direction and speed are arithmetic on the position, so
/// reversing needs nothing from `sample` at all.
#[test]
fn reversing_is_arithmetic_on_the_position() {
    let g = sampled("sample(load(\"break.wav\"), 1 - ramp(0.5))\n").expect("should lower").graph;
    assert!(g.nodes.iter().any(|n| n.kind == NodeKind::Sub), "1 - ramp is a Sub node");
    assert!(g.nodes.iter().any(|n| n.kind == NodeKind::Sample));
}

#[test]
fn a_second_channel_can_be_asked_for() {
    let g = sampled("sample(load(\"break.wav\"), ramp(1), 1)\n").expect("should lower").graph;
    let read = g.nodes.iter().find(|n| n.kind == NodeKind::Sample).unwrap();
    assert_eq!(read.inputs[2], Const(1.0));
}

// ---- slice ----

/// Where the read head sits at each of `times`, in position units.
///
/// The buffer these tests share is a ramp from -1 to 1, so what a graph renders
/// says where in the buffer it read — undo that mapping and a rendered sample
/// *is* a position. Everything below is about which positions are read and
/// when, which is the only thing `slice` decides.
fn read_positions(src: &str, times: &[f64]) -> Vec<f32> {
    use fundsp::prelude64::AudioUnit;

    let lowered = sampled(src).expect("should lower");
    let mut net = crate::swync_graph::realizer::realize(&lowered.graph).expect("should realize");
    net.set_sample_rate(44100.0);

    let last = (times.last().expect("a time") * 44100.0) as usize + 1;
    let rendered: Vec<f32> = (0..last).map(|_| net.get_mono()).collect();
    times
        .iter()
        .map(|t| (rendered[((t * 44100.0) as usize).min(last - 1)] + 1.0) / 2.0)
        .collect()
}

/// A slice is a `Line` feeding a `Sample`, and nothing else — no new node kind,
/// and no state anywhere. The buffer is two seconds, so a quarter of it is half
/// a second, and that half-second is the whole feature: it is the number a
/// hand-written phasor has to be told and can therefore be told wrong.
#[test]
fn a_slice_is_a_line_into_the_ordinary_reader() {
    let g = sampled("slice(load(\"break.wav\"), 0.25, 0.5)\n").expect("should lower").graph;

    assert_eq!(g.nodes.len(), 2, "a slice is two nodes: {:?}", g.nodes);
    assert_eq!(g.nodes[0], node(NodeKind::Line, vec![Const(0.25), Const(0.5), Const(0.5)]));
    assert_eq!(
        g.nodes[1],
        node(NodeKind::Sample, vec![Node(NodeId(0)), Const(0.0), Const(0.0)]),
    );
}

/// The reason the builtin exists: the portion is read at the speed the buffer
/// was recorded at, so a quarter of a two-second break takes half a second.
#[test]
fn a_slice_reads_its_portion_at_the_buffers_own_speed() {
    let at = read_positions(
        "slice(load(\"break.wav\"), 0.25, 0.5)\n", &[0.0, 0.25, 0.5, 0.75]);

    assert!((at[0] - 0.25).abs() < 0.01, "should start at 0.25, got {}", at[0]);
    assert!((at[1] - 0.375).abs() < 0.01, "halfway at 0.375, got {}", at[1]);
    assert!((at[2] - 0.5).abs() < 0.01, "arrives at 0.5, got {}", at[2]);
    assert!((at[3] - 0.5).abs() < 0.01, "and holds there, got {}", at[3]);
}

/// The thing the hand-written form kept getting wrong, pinned as a difference:
/// scaling a `ramp`'s position without scaling its frequency reads the right
/// portion at a quarter of the speed.
#[test]
fn a_slice_is_not_the_naive_scaled_phasor() {
    let sliced = read_positions("slice(load(\"break.wav\"), 0, 0.25)\n", &[0.5]);
    let naive = read_positions(
        "sample(load(\"break.wav\"), ramp(1 / load(\"break.wav\").secs) * 0.25)\n", &[0.5]);

    assert!((sliced[0] - 0.25).abs() < 0.01, "the slice is done, got {}", sliced[0]);
    assert!((naive[0] - 0.0625).abs() < 0.01, "the naive one crawls, got {}", naive[0]);
}

/// And it really is only sugar: the spelling it replaces renders the same
/// samples, so nothing was added to the audio path.
#[test]
fn a_slice_is_the_hand_written_form_exactly() {
    let times = [0.0, 0.1, 0.3, 0.49, 0.6];
    assert_eq!(
        read_positions("slice(load(\"break.wav\"), 0.25, 0.5)\n", &times),
        read_positions(
            "let b = load(\"break.wav\")\nsample(b, line(0.25, 0.5, 0.25 * b.secs))\n",
            &times,
        ),
    );

    // And with a rate, which is the same line with the same end points and a
    // shorter one of them — both spellings are written out in the reference.
    assert_eq!(
        read_positions("slice(load(\"break.wav\"), 0.25, 0.5, 2)\n", &times),
        read_positions(
            "let b = load(\"break.wav\")\nsample(b, line(0.25, 0.5, 0.25 * b.secs / 2))\n",
            &times,
        ),
    );
}

/// Backwards costs nothing and needs no flag: the ends are simply the other way
/// round, and the portion still takes as long as it lasts.
#[test]
fn a_slice_written_backwards_plays_backwards() {
    let at = read_positions(
        "slice(load(\"break.wav\"), 0.5, 0.25)\n", &[0.0, 0.25, 0.5]);

    assert!((at[0] - 0.5).abs() < 0.01, "should start at 0.5, got {}", at[0]);
    assert!((at[1] - 0.375).abs() < 0.01, "and fall, got {}", at[1]);
    assert!((at[2] - 0.25).abs() < 0.01, "landing on 0.25, got {}", at[2]);
}

#[test]
fn a_slice_can_be_asked_for_a_second_channel() {
    let g = sampled("slice(load(\"break.wav\"), 0, 1, 1, 1)\n").expect("should lower").graph;
    let read = g.nodes.iter().find(|n| n.kind == NodeKind::Sample).unwrap();
    assert_eq!(read.inputs[2], Const(1.0));
}

/// The rate is spent on the line's length and nowhere else: the same portion,
/// covered in half the time, is that portion at double speed. The buffer is two
/// seconds, so a quarter of it is half a second at rate 1.
#[test]
fn a_rate_shortens_the_line_rather_than_moving_its_ends() {
    let duration = |src: &str| {
        let g = sampled(src).expect("should lower").graph;
        let line = g.nodes.iter().find(|n| n.kind == NodeKind::Line).expect("a Line");
        assert_eq!(line.inputs[0], Const(0.25), "the ends must not move");
        assert_eq!(line.inputs[1], Const(0.5));
        match line.inputs[2] {
            Const(d) => d,
            _ => panic!("the duration should be a constant"),
        }
    };

    assert_eq!(duration("slice(load(\"break.wav\"), 0.25, 0.5)\n"), 0.5);
    assert_eq!(duration("slice(load(\"break.wav\"), 0.25, 0.5, 2)\n"), 0.25);
    assert_eq!(duration("slice(load(\"break.wav\"), 0.25, 0.5, 0.5)\n"), 1.0);
}

/// And it really is a speed: at rate 2 the reader is through the portion in
/// half the time it took at rate 1.
#[test]
fn a_rate_reads_the_portion_faster() {
    let at = read_positions(
        "slice(load(\"break.wav\"), 0.25, 0.5, 2)\n", &[0.0, 0.125, 0.25, 0.5]);

    assert!((at[0] - 0.25).abs() < 0.01, "should start at 0.25, got {}", at[0]);
    assert!((at[1] - 0.375).abs() < 0.01, "halfway by 0.125 s, got {}", at[1]);
    assert!((at[2] - 0.5).abs() < 0.01, "and done by 0.25 s, got {}", at[2]);
    assert!((at[3] - 0.5).abs() < 0.01, "then holds, got {}", at[3]);
}

/// A rate applies to a reversed slice the same way — it is the speed of the
/// read, not the direction of it.
#[test]
fn a_rate_applies_to_a_backwards_slice_too() {
    let at = read_positions(
        "slice(load(\"break.wav\"), 0.5, 0.25, 2)\n", &[0.0, 0.125, 0.25]);

    assert!((at[0] - 0.5).abs() < 0.01, "should start at 0.5, got {}", at[0]);
    assert!((at[1] - 0.375).abs() < 0.01, "halfway by 0.125 s, got {}", at[1]);
    assert!((at[2] - 0.25).abs() < 0.01, "and done by 0.25 s, got {}", at[2]);
}

/// At zero the reader would stop rather than slow, which is the DC offset the
/// whole builtin exists to keep anyone from writing by accident.
#[test]
fn a_rate_of_zero_is_refused() {
    let e = sample_err("slice(load(\"break.wav\"), 0, 0.25, 0)\n");
    assert!(e.contains("above zero"), "got: {e}");
}

/// Backwards is the ends the other way round. A negative rate would be a second
/// spelling of it, and the message has to say which one is the real one.
#[test]
fn a_negative_rate_points_at_writing_the_ends_backwards() {
    let e = sample_err("slice(load(\"break.wav\"), 0, 0.25, -1)\n");
    assert!(e.contains("above zero"), "got: {e}");
    assert!(e.contains("0.5, 0.25"), "should show the backwards spelling, got: {e}");
}

#[test]
fn a_rate_that_is_a_signal_says_to_use_sample() {
    let e = sample_err("slice(load(\"break.wav\"), 0, 0.25, ramp(1))\n");
    assert!(e.contains("compile-time number"), "got: {e}");
    assert!(e.contains("`sample`"), "should name the alternative, got: {e}");
}

#[test]
fn a_slice_takes_no_more_than_five_arguments() {
    let e = sample_err("slice(load(\"break.wav\"), 0, 0.25, 1, 0, 9)\n");
    assert!(e.contains("slice(buffer, start, end)"), "got: {e}");
}

/// A slice is written down in the source at both ends, so an end outside the
/// buffer is a typo rather than a position that happens to be silent — which is
/// the one place it may differ from `sample`, where anything at all is fair.
#[test]
fn an_end_outside_the_buffer_is_refused() {
    for src in [
        "slice(load(\"break.wav\"), 0, 1.5)\n",
        "slice(load(\"break.wav\"), -0.2, 0.5)\n",
    ] {
        let e = sample_err(src);
        assert!(e.contains("between 0 and 1"), "{src} gave: {e}");
    }
}

/// The ends are spent on a line built once, so neither can be modulated. The
/// message has to point at the name that can be.
#[test]
fn an_end_that_is_a_signal_says_to_use_sample() {
    let e = sample_err("slice(load(\"break.wav\"), 0, ramp(1))\n");
    assert!(e.contains("compile-time number"), "got: {e}");
    assert!(e.contains("`sample`"), "should name the alternative, got: {e}");
}

/// Which end is wrong, rather than that one of them is.
#[test]
fn a_refused_end_says_which_one_it_was() {
    assert!(sample_err("slice(load(\"break.wav\"), 4, 0.5)\n").contains("start"));
    assert!(sample_err("slice(load(\"break.wav\"), 0.5, 4)\n").contains("end"));
}

#[test]
fn a_slice_needs_both_of_its_ends() {
    let e = sample_err("slice(load(\"break.wav\"), 0.25)\n");
    assert!(e.contains("slice(buffer, start, end)"), "got: {e}");
}

#[test]
fn slicing_something_that_is_not_a_buffer_says_so() {
    let e = sample_err("slice(220, 0, 1)\n");
    assert!(e.contains("must be a buffer"), "got: {e}");
}

// ---- what a buffer is not ----

#[test]
fn a_buffer_is_not_a_signal() {
    let e = sample_err("sin(load(\"break.wav\"))\n");
    assert!(e.contains("sample(buffer, position)"), "got: {e}");
}

#[test]
fn a_buffer_is_not_a_pattern() {
    let e = sample_err("fn v(n) = sin(n)\nplay([load(\"break.wav\")], v)\n");
    assert!(e.contains("cannot contain a buffer"), "got: {e}");
}

#[test]
fn reading_something_that_is_not_a_buffer_says_so() {
    let e = sample_err("sample(220, ramp(1))\n");
    assert!(e.contains("must be a buffer"), "got: {e}");
}

/// The channel picks which reader is built, so it cannot be modulated.
#[test]
fn a_channel_must_be_a_compile_time_number() {
    let e = sample_err("sample(load(\"break.wav\"), ramp(1), sin(2))\n");
    assert!(e.contains("compile-time number"), "got: {e}");
}

// ---- load ----

/// The path has to be findable by the walk that loads it, which means written
/// out rather than computed.
#[test]
fn a_path_must_be_written_out() {
    let e = sample_err("let p = 1\nsample(load(p), ramp(1))\n");
    assert!(e.contains("written out as a string"), "got: {e}");
}

#[test]
fn a_string_anywhere_else_is_refused() {
    let e = sample_err("sin(\"break.wav\")\n");
    assert!(e.contains("only meaningful as the path"), "got: {e}");
}

/// A program naming a file nothing loaded is a bug in the walk rather than
/// anything a program did — but it must still be an error, not a panic.
#[test]
fn a_buffer_that_was_never_loaded_is_an_error() {
    let e = sample_err("sample(load(\"missing.wav\"), ramp(1))\n");
    assert!(e.contains("was not loaded"), "got: {e}");
}

#[test]
fn a_path_is_not_something_to_chain_into() {
    let e = sample_err("1 >> load(\"break.wav\")\n");
    assert!(e.contains("not something to chain into"), "got: {e}");
}

/// The sampling examples in the README, compiled.
///
/// Documentation that does not run is worse than none, and these are the whole
/// explanation of the feature. Kept as the source text the reference shows, so
/// a change to one has to be a change to both.
#[test]
fn the_readme_sampling_examples_compile() {
    // `breaks/amen.wav` and `pad.wav` as the reference writes them.
    fn readme_samples() -> crate::samples::Samples {
        use std::sync::Arc;
        let mut wave = fundsp::wave::Wave::new(2, 1000.0);
        for i in 0..2000 {
            wave.push((i as f32 / 1000.0 - 1.0, i as f32 / 1000.0 - 1.0));
        }
        let wave = Arc::new(wave);
        crate::samples::Samples::from_pairs([
            ("breaks/amen.wav".to_string(), wave.clone()),
            ("pad.wav".to_string(), wave.clone()),
            ("door.wav".to_string(), wave),
        ])
    }

    let examples: &[&str] = &[
        "let amen = load(\"breaks/amen.wav\")\nsample(amen, ramp(1 / amen.secs))\n",
        "let amen = load(\"breaks/amen.wav\")\nsample(amen, 1 - ramp(1 / amen.secs))\n",
        "let amen = load(\"breaks/amen.wav\")\nsample(amen, ramp(2 / amen.secs))\n",
        "let amen = load(\"breaks/amen.wav\")\nsample(amen, ramp(0.5 / amen.secs))\n",
        "let amen = load(\"breaks/amen.wav\")\nsample(amen, ramp(4 / amen.secs) * 0.25)\n",
        "let amen = load(\"breaks/amen.wav\")\nsample(amen, 0.5 + ramp(4 / amen.secs) * 0.25)\n",
        "let amen = load(\"breaks/amen.wav\")\nsample(amen, ramp(1 / amen.secs) >> hold(16, 0))\n",
        // The `slice` examples, and the equivalence the section claims.
        "let amen = load(\"breaks/amen.wav\")\nslice(amen, 0, 0.25)\n",
        "let amen = load(\"breaks/amen.wav\")\nslice(amen, 0.5, 0.75)\n",
        "let amen = load(\"breaks/amen.wav\")\nslice(amen, 0.5, 0.25)\n",
        "let amen = load(\"breaks/amen.wav\")\nslice(amen, 0, 1)\n",
        // The rates, and both equivalences the section claims.
        "let amen = load(\"breaks/amen.wav\")\nslice(amen, 0, 0.25, 2)\n",
        "let amen = load(\"breaks/amen.wav\")\nslice(amen, 0, 0.25, 0.5)\n",
        "let amen = load(\"breaks/amen.wav\")\nslice(amen, 0.5, 0.25, 2)\n",
        "let amen = load(\"breaks/amen.wav\")\nslice(amen, 0.25, 0.5)\n\
         sample(amen, line(0.25, 0.5, 0.25 * amen.secs))\n",
        "let amen = load(\"breaks/amen.wav\")\nslice(amen, 0.25, 0.5, 2)\n\
         sample(amen, line(0.25, 0.5, 0.25 * amen.secs / 2))\n",
        "fn slam() = slice(load(\"door.wav\"), 0, 0.15) * perc(0.001, 0.2)\nplay([\\], slam)\n",
        // The chopping example, entire, and the `slice` spelling beside it.
        "fn chop(n, at = 0) =\n  sample(load(\"breaks/amen.wav\"), at + ramp(n) * 0.0625) * perc(0.001, 0.2)\n\
         play([\\, \\, \\, \\, \\, \\, \\, \\], chop, 1,\n     at: [0, 0.25, 0.5, 0.0625, 0.75, 0.5, 0.125, 0.875])\n",
        "fn chop(n, at = 0) =\n  slice(load(\"breaks/amen.wav\"), at, at + 0.0625) * perc(0.001, 0.2)\n\
         play([\\, \\, \\, \\, \\, \\, \\, \\], chop, 1,\n     at: [0, 0.25, 0.5, 0.0625, 0.75, 0.5, 0.125, 0.875])\n",
        // The stereo one.
        "let stereo = load(\"pad.wav\")\nlet pos = ramp(1 / stereo.secs)\n\
         (sample(stereo, pos, 0) + sample(stereo, pos, 1)) * 0.5\n",
    ];

    for src in examples {
        let items = parse(src.to_string())
            .unwrap_or_else(|e| panic!("README example failed to parse: {e}\n{src}"));
        let lowered = crate::lowerer::lower::lower_with_samples(&items, readme_samples())
            .unwrap_or_else(|e| panic!("README example failed to lower: {e}\n{src}"));
        crate::swync_graph::realizer::realize(&lowered.graph)
            .unwrap_or_else(|e| panic!("README example failed to realize: {e}\n{src}"));
    }
}

/// The beat example from [Syncing to the beat], as the reference writes it.
///
/// It is an instrument, so lowering the program is not enough — the shapes
/// written with `qvs` and `qvh` only exist once a note asks for them, and this
/// is where an unbound name would show up.
#[test]
fn the_readme_beat_example_builds_a_voice() {
    let src = "fn stab(n) = lowpass(saw(n.m2h), 400 + 3000 * sin(qvh * 2), 3) * perc(0, qvs / 2)\n\
               play([60, 63], stab)\nplay([60, 63], stab, 2)\n";
    let items = parse(src.to_string()).expect("README example failed to parse");
    lower(&items).unwrap_or_else(|e| panic!("README example failed to lower: {e}"));

    let ins = crate::scheduler::voice::Instruments::from_program(&items);
    crate::scheduler::voice::build_voice(&ins, "stab", 60.0, &[], 0.5, DEFAULT_BEAT_SECS, Default::default())
        .unwrap_or_else(|e| panic!("README example failed to build a voice: {e}"));
}

/// And the chopping example really builds a voice, which is the half that
/// lowering the program does not reach.
#[test]
fn the_readme_chop_instrument_builds_a_voice() {
    use std::sync::Arc;
    let mut wave = fundsp::wave::Wave::new(1, 1000.0);
    for i in 0..1000 {
        wave.push(i as f32 / 500.0 - 1.0);
    }
    let samples = crate::samples::Samples::from_pairs(
        [("breaks/amen.wav".to_string(), Arc::new(wave))]);

    let src = "fn chop(n, at = 0) =\n  \
               sample(load(\"breaks/amen.wav\"), at + ramp(n) * 0.0625) * perc(0.001, 0.2)\n";
    let items = parse(src.to_string()).expect("should parse");
    let ins = crate::scheduler::voice::Instruments::from_program(&items).with_samples(samples);

    let lanes = vec![("at".to_string(), crate::pattern::patterns::LaneArg::Num(0.75))];
    assert!(crate::scheduler::voice::build_voice(&ins, "chop", 1.0, &lanes, 0.25, DEFAULT_BEAT_SECS, Default::default())
        .is_ok());
}

/// `;` end to end: what someone types, as the rhythm it turns into.
///
/// The timing arithmetic is `pattern.rs`'s to test; these are about the trip
/// from source text to a bound pattern — that the token parses where a step
/// belongs, that the number reaches the slot, and that the two positions a list
/// can be read in give it the two meanings they should.
#[cfg(test)]
mod length_tests {
    use super::*;
    use crate::pattern::pattern::{Slot, Span, UNIT};
    use crate::scheduler::clock::Meter;

    const TONE: &str = "fn tone(n, cut = 800) = saw(n)\n";

    fn pattern_of(src: &str) -> Pattern {
        bindings_of(src).into_iter().next().expect("a binding").source.pattern().cloned().expect("a written pattern")
    }

    fn slots(p: &Pattern) -> Vec<Slot> {
        match p {
            Pattern::Steps(slots) => slots.clone(),
            other => panic!("expected a sequence, got {other:?}"),
        }
    }

    /// A metrical pass that does not fill its bar arrives wrapped in the `Fast`
    /// that rotates it against the grid — see `to_pattern_timed`. Tests about
    /// what is *in* a pass look through it rather than restating the meter.
    fn pass_slots(p: &Pattern) -> Vec<Slot> {
        match p {
            Pattern::Fast(_, inner) => slots(inner),
            other => slots(other),
        }
    }

    /// The token parses, and the number lands on the slot it followed.
    #[test]
    fn a_length_reaches_its_slot() {
        let got = slots(&pattern_of(&format!("{TONE}play([220;2, 330, 440], tone)\n")));
        assert_eq!(
            got,
            vec![
                Slot::sized(Step::Value(220.0), 2.0),
                Slot::new(Step::Value(330.0)),
                Slot::new(Step::Value(440.0)),
            ],
        );
    }

    /// A pattern with no `;` in it is untouched — the whole compatibility
    /// promise, checked at the level someone actually writes.
    #[test]
    fn a_pattern_without_lengths_is_unchanged() {
        let got = slots(&pattern_of(&format!("{TONE}play([220, 330], tone)\n")));
        assert!(got.iter().all(|s| s.length == UNIT), "got {got:?}");
    }

    /// Rests and triggers take lengths too, which is what lets a bar end in
    /// silence without padding it out with single cells.
    #[test]
    fn rests_and_triggers_take_lengths() {
        let got = slots(&pattern_of(&format!(
            "fn hit() = sin(50)\nplay([\\;3, `;5], hit)\n")));
        assert_eq!(got[0].length, 3.0);
        assert_eq!(got[1].length, 5.0);
        assert_eq!(got[1].step, Step::Rest);
    }

    /// The motivating rhythm, written the way it would be: a quarter, two
    /// eighths, and a half of silence, in one bar.
    #[test]
    fn a_quarter_and_two_eighths_from_source() {
        let p = pattern_of(&format!("{TONE}play([220;2, 330, 440, `;4], tone)\n"));
        let evs = p.query(Span::new(0.0, 1.0));
        let onsets: Vec<f64> = evs.iter().map(|e| e.begin).collect();
        assert_eq!(onsets, vec![0.0, 0.25, 0.375]);
        assert_eq!(evs[0].duration(), 0.25, "the quarter lasts a beat");
    }

    /// The motivating melody: an octave written once, at the front.
    #[test]
    fn an_octave_carries_across_a_played_sequence() {
        let got = slots(&pattern_of(&format!("{TONE}play([a1;q, a, a, a], tone)\n")));
        assert_eq!(got.iter().map(|s| s.step.clone()).collect::<Vec<_>>(),
                   vec![Step::Value(33.0); 4]);
        assert!(got.iter().all(|s| s.length == 1.0), "got {got:?}");
    }

    /// A tuplet takes the line's octave in with it and hands it back, exactly
    /// as it does with the written value in force.
    #[test]
    fn a_tuplet_inherits_the_octave_and_returns_it() {
        let got = slots(&pattern_of(&format!(
            "{TONE}play([c4;q, [g, b, d5];t, g], tone)\n")));
        let Step::Group(inner) = &got[1].step else {
            panic!("expected a tuplet group, got {:?}", got[1].step);
        };
        assert_eq!(slots(inner).iter().map(|s| s.step.clone()).collect::<Vec<_>>(),
                   vec![Step::Value(67.0), Step::Value(71.0), Step::Value(74.0)]);
        // The group ended on d5, and the step after it is g4 all the same.
        assert_eq!(got[2].step, Step::Value(67.0));
    }

    /// With no octave in force `e` is the eighth it has always been, so a
    /// rhythm written in bare values is untouched by the register existing.
    #[test]
    fn bare_written_values_still_read_as_a_rhythm() {
        let got = pass_slots(&pattern_of(&format!("fn hit() = sin(50)\nplay([q, e, e], hit)\n")));
        assert_eq!(got.iter().map(|s| s.length).collect::<Vec<_>>(),
                   vec![1.0, 0.5, 0.5]);
        assert!(got.iter().all(|s| s.step == Step::Value(1.0)), "got {got:?}");
    }

    /// A hit of a given length is still writable in a sequence that is in an
    /// octave — it is `\;e`, which the ambiguity error points at.
    #[test]
    fn a_trigger_gives_a_pitchless_hit_inside_a_melody() {
        let got = pass_slots(&pattern_of(&format!("{TONE}play([c4;q, \\;e, g], tone)\n")));
        assert_eq!(got[1].step, Step::Value(1.0));
        assert_eq!(got[1].length, 0.5);
        // And the octave carried across it: the trigger is not a note and has
        // no register of its own to set.
        assert_eq!(got[2].step, Step::Value(67.0));
    }

    /// A length may be any expression that folds to a number, like `rate`.
    #[test]
    fn a_length_may_be_a_bound_name() {
        let src = format!("{TONE}let beats = 3\nplay([220;beats, 330], tone)\n");
        assert_eq!(slots(&pattern_of(&src))[0].length, 3.0);
    }

    /// Nesting and `;` compose: a group takes its share and divides it.
    #[test]
    fn a_group_takes_a_length() {
        let got = slots(&pattern_of(&format!("{TONE}play([220;3, [330, 440]], tone)\n")));
        assert_eq!(got[0].length, 3.0);
        assert!(matches!(got[1].step, Step::Group(_)), "got {:?}", got[1].step);
    }

    /// A lane is indexed by note, so a length there is a count of notes.
    #[test]
    fn a_lane_length_holds_a_value_across_notes() {
        let bs = bindings_of(&format!(
            "{TONE}play([220, 330, 440], tone, cut: [400;2, 2000])\n"));
        assert_eq!(bs[0].lanes[0].steps().values(),
                   vec![Some(400.0), Some(400.0), Some(2000.0)]);
    }

    /// And so a fraction there is not a place. Rejected rather than rounded,
    /// because the mistake is a misunderstanding of what the lane is.
    #[test]
    fn a_fractional_lane_length_is_refused() {
        let err = play_err(&format!("{TONE}play([220], tone, cut: [400;1.5, 2000])\n"));
        assert!(err.contains("whole number"), "got: {err}");
        assert!(err.contains("cut"), "the message should name the lane: {err}");
    }

    /// A fraction in the pattern itself is fine — that is a dotted note.
    #[test]
    fn a_fractional_pattern_length_is_allowed() {
        let got = slots(&pattern_of(&format!("{TONE}play([220;1.5, 330;0.5], tone)\n")));
        assert_eq!(got[0].length, 1.5);
    }

    /// A list being used as data has no extent for a length to be a share of,
    /// so `len` counts what was written. The alternative — expanding `[1;3, 2]`
    /// to four elements — would give one literal two different lengths
    /// depending on which side of a `play` it was read from.
    #[test]
    fn plain_list_operations_ignore_lengths() {
        let g = lower_src("sin(len([1;3, 2]))\n").unwrap();
        assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(2.0)])]);
    }

    /// Indexing gives the element, not the element with its length stuck to it.
    #[test]
    fn indexing_yields_the_bare_value() {
        let g = lower_src("sin([220;4, 330][0])\n").unwrap();
        assert_eq!(g.nodes, vec![node(NodeKind::Sin, vec![Const(220.0)])]);
    }

    /// Rearranging a list keeps each element's length with it — `rev` moves
    /// notes around, it does not restripe the rhythm.
    #[test]
    fn reversing_carries_lengths_along() {
        let got = slots(&pattern_of(&format!("{TONE}play(rev([220;3, 330]), tone)\n")));
        assert_eq!(got[0].length, UNIT, "330 was unweighted and still is");
        assert_eq!(got[1].length, 3.0, "220 kept its length across the reverse");
    }

    /// Zero and negative lengths are not rhythms, and are caught where the
    /// number is folded rather than left to divide something by nothing.
    #[test]
    fn a_length_must_be_positive() {
        for bad in ["0", "-2"] {
            let err = play_err(&format!("{TONE}play([220;{bad}, 330], tone)\n"));
            assert!(err.contains("positive"), "length {bad} gave: {err}");
        }
    }
}

/// `stack` — patterns that sound at once.
///
/// The piano roll's primitive: overlapping notes are decomposed into
/// monophonic voices and layered, and a chord is the case where the voices
/// happen to line up.
#[cfg(test)]
mod stack_tests {
    use super::*;
    use crate::pattern::pattern::Span;

    const TONE: &str = "fn tone(n, cut = 800) = saw(n)\n";

    fn pattern_of(src: &str) -> Pattern {
        bindings_of(src).into_iter().next().expect("a binding").source.pattern().cloned().expect("a written pattern")
    }

    fn onsets(p: &Pattern, span: Span) -> Vec<f64> {
        let mut got: Vec<f64> = p.query(span).iter().map(|e| e.begin).collect();
        got.sort_by(|a, b| a.partial_cmp(b).unwrap());
        got
    }

    /// A chord: three notes struck together, all lasting the whole bar.
    #[test]
    fn a_chord_sounds_all_its_notes_at_once() {
        let p = pattern_of(&format!("{TONE}play(stack([c4], [e4], [g4]), tone)\n"));
        let evs = p.query(Span::new(0.0, 1.0));
        assert_eq!(evs.len(), 3);
        assert!(evs.iter().all(|e| e.begin == 0.0), "all together: {evs:?}");

        let mut values: Vec<f64> = evs.iter().map(|e| e.value).collect();
        values.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(values, vec![60.0, 64.0, 67.0]);
    }

    /// Layers keep their own division, which is the whole reason this is not
    /// just a longer list: three against four needs both to be true at once.
    #[test]
    fn layers_divide_independently() {
        let p = pattern_of(&format!(
            "{TONE}play(stack([220, 330, 440], [55, 110]), tone)\n"));
        // Thirds from one layer, halves from the other.
        assert_eq!(
            onsets(&p, Span::new(0.0, 1.0)),
            vec![0.0, 0.0, 1.0 / 3.0, 0.5, 2.0 / 3.0],
        );
    }

    /// Lengths and layers compose: this is what a drawn part turns into — one
    /// voice holding while another moves under it.
    #[test]
    fn a_held_note_under_a_moving_line() {
        let p = pattern_of(&format!(
            "{TONE}play(stack([c4;2, e4;2], [g4;4]), tone)\n"));
        let evs = p.query(Span::new(0.0, 1.0));
        assert_eq!(evs.len(), 3);

        let held = evs.iter().find(|e| e.value == 67.0).expect("the g");
        assert_eq!(held.duration(), 1.0, "the held note spans the bar");
        let moving: Vec<f64> = evs.iter().filter(|e| e.value != 67.0).map(|e| e.begin).collect();
        assert_eq!(moving, vec![0.0, 0.5]);
    }

    /// A stack inside a step is a chord at one point of a longer line, rather
    /// than a layer over the whole of it.
    #[test]
    fn a_stack_may_fill_one_step() {
        let p = pattern_of(&format!(
            "{TONE}play([c4, stack([e4], [g4])], tone)\n"));
        let evs = p.query(Span::new(0.0, 1.0));
        assert_eq!(onsets(&p, Span::new(0.0, 1.0)), vec![0.0, 0.5, 0.5]);
        assert_eq!(evs.iter().filter(|e| e.begin == 0.5).count(), 2);
    }

    /// Stacks nest, so a voice may itself be layered.
    #[test]
    fn stacks_nest() {
        let p = pattern_of(&format!(
            "{TONE}play(stack([c4], stack([e4], [g4])), tone)\n"));
        assert_eq!(p.query(Span::new(0.0, 1.0)).len(), 3);
    }

    /// A lane reads a stack as all its values, layer by layer — the same
    /// positional flattening a nested list gets.
    #[test]
    fn a_lane_reads_a_stack_in_order() {
        let bs = bindings_of(&format!(
            "{TONE}play([220, 330], tone, cut: stack([400], [2000]))\n"));
        assert_eq!(bs[0].lanes[0].steps().values(), vec![Some(400.0), Some(2000.0)]);
    }

    /// An empty stack is a mistake worth naming rather than silence.
    #[test]
    fn an_empty_stack_is_an_error() {
        let err = play_err(&format!("{TONE}play(stack(), tone)\n"));
        assert!(err.contains("at least one"), "got: {err}");
    }

    /// A stack is layered patterns, not audio, and saying so beats a type name.
    #[test]
    fn a_stack_is_not_a_signal() {
        // `Lowered` has no Debug, so `expect_err` is unavailable here.
        let err = play_err("sin(stack([220], [330]))\n");
        assert!(err.contains("not audio"), "got: {err}");
    }}


// ---- arrangement combinators ----

/// Sections with lengths, which everything that sequences them needs. The
/// `SECTIONS` fixture's `chorus` is a plain `play` on purpose, so it is the
/// wrong shape for anything but `.then`.
const ARR: &str = "\
fn lead(n) = sin(n)
fn bass(n, cut = 400) = lowpass(saw(n), cut, 1)
fn hat(n) = noise() * n
fn one() = play_once([c4], lead)
fn two() = playn([c5], lead, 2)
fn faster(x) = playn([c4], lead, x)
fn endless() = play([c4], lead)
";

/// A gap really is silence: the next section opens later than it otherwise
/// would, and nothing is written to fill the space.
#[test]
fn then_after_leaves_a_gap() {
    let bs = bindings_of(&format!("{ARR}playn([c3], bass, 4).then_after(2, one)\n"));
    assert_eq!(bs.len(), 2);
    assert_eq!(bs[0].bars, Some(4.0));
    assert_eq!(bs[1].start, 6.0);
}

/// The two sections really do sound together over the join.
#[test]
fn overlap_starts_the_next_section_early() {
    let bs = bindings_of(&format!("{ARR}playn([c3], bass, 4).overlap(1, two)\n"));
    assert_eq!(bs[0].bars, Some(4.0));
    assert_eq!(bs[1].start, 3.0, "one bar before the first ended");
    // The chain carries on from the later of the two: 3 + 2 beats 4.
    let bs = bindings_of(&format!("{ARR}playn([c3], bass, 4).overlap(1, two).then(one)\n"));
    assert_eq!(bs[2].start, 5.0);
}

/// Overlapping by more than the section's whole length stops at its start
/// rather than placing the newcomer in front of it.
#[test]
fn overlap_never_runs_backwards_past_the_start() {
    let bs = bindings_of(&format!("{ARR}playn([c3], bass, 2).overlap(99, one)\n"));
    assert_eq!(bs[1].start, 0.0);
}

/// `with` lays a section alongside from where the receiver *began*, which is
/// what separates it from `then`.
#[test]
fn with_runs_alongside_from_the_start() {
    let bs = bindings_of(&format!(
        "{ARR}playn([c3], bass, 4).then(two).with(one)\n"));
    assert_eq!(bs.len(), 3);
    assert_eq!(bs[1].start, 4.0, "two, after the bass");
    assert_eq!(bs[2].start, 4.0, "one, alongside it rather than after it");
}

/// And the pair finishes when the later of them does, so a `.then` clears both.
#[test]
fn with_finishes_when_the_later_one_does() {
    let bs = bindings_of(&format!("{ARR}one().with(two).then(one)\n"));
    assert_eq!(bs[2].start, 2.0, "two is the longer of the pair");
}

#[test]
fn at_places_a_section_absolutely() {
    let bs = bindings_of(&format!("{ARR}at(8, one)\n"));
    assert_eq!(bs[0].start, 8.0);
}

#[test]
fn seq_runs_sections_in_order() {
    let bs = bindings_of(&format!("{ARR}seq(one, two, one)\n"));
    assert_eq!(bs.len(), 3);
    assert_eq!(bs[0].start, 0.0);
    assert_eq!(bs[1].start, 1.0);
    assert_eq!(bs[2].start, 3.0);
}

#[test]
fn then_n_repeats_a_section() {
    let bs = bindings_of(&format!("{ARR}one().then_n(two, 3)\n"));
    assert_eq!(bs.len(), 4);
    assert_eq!(bs[1].start, 1.0);
    assert_eq!(bs[2].start, 3.0);
    assert_eq!(bs[3].start, 5.0);
}

/// Each pass is inlined afresh, so the chain after them all is where the last
/// one really ended.
#[test]
fn then_n_hands_over_after_the_last_pass() {
    let bs = bindings_of(&format!("{ARR}one().then_n(two, 3).then(one)\n"));
    assert_eq!(bs[4].start, 7.0);
}

/// The element reaches the section, which is the whole point of `then_each`.
#[test]
fn then_each_passes_the_element() {
    let bs = bindings_of(&format!("{ARR}one().then_each([1, 2, 4], faster)\n"));
    assert_eq!(bs.len(), 4);
    assert_eq!(bs[1].bars, Some(1.0));
    assert_eq!(bs[2].bars, Some(2.0));
    assert_eq!(bs[3].bars, Some(4.0));
    // And they follow one another rather than piling up at the same bar.
    assert_eq!(bs[1].start, 1.0);
    assert_eq!(bs[2].start, 2.0);
    assert_eq!(bs[3].start, 4.0);
}

/// A fill is played by the instrument it follows, with that instrument's lanes.
#[test]
fn then_fill_inherits_the_instrument_and_its_lanes() {
    let bs = bindings_of(&format!(
        "{ARR}playn([c3], bass, 4, cut: [500, 900]).then_fill([c2, c2, c2])\n"));
    assert_eq!(bs.len(), 2);
    assert_eq!(bs[1].target, "bass");
    assert_eq!(bs[1].start, 4.0);
    assert_eq!(bs[1].bars, Some(1.0), "one pass");
    assert_eq!(bs[1].lanes, bs[0].lanes, "the lanes came with it");
    assert_ne!(bs[1].source.pattern().cloned().expect("a written pattern"), bs[0].source.pattern().cloned().expect("a written pattern"), "only the pattern is new");
}

#[test]
fn then_fill_hands_the_chain_on() {
    let bs = bindings_of(&format!(
        "{ARR}playn([c3], bass, 4).then_fill([c2]).then(one)\n"));
    assert_eq!(bs[2].start, 5.0);
}

/// A group of plays has no one instrument, so there is nothing to fill for.
#[test]
fn then_fill_refuses_a_group() {
    let e = play_err(&format!(
        "{ARR}play_all(play_once([c3], bass), play_once([c4], lead)).then_fill([c2])\n"));
    assert!(e.contains("single `play`"), "got: {e}");
}

/// What `loop` is for: a fill on its own stops, and the pair of them is what
/// wants to come round again.
#[test]
fn loop_repeats_the_whole_chain_and_not_just_the_last_link() {
    let bs = bindings_of(&format!(
        "{ARR}playn([c3], bass, 3).then_fill([c2, c2]).loop(3)\n"));
    assert_eq!(bs.len(), 6, "groove and fill, three times over");
    // Pass one is the original pair; every later pass is it, four bars on.
    let starts: Vec<f64> = bs.iter().map(|b| b.start).collect();
    assert_eq!(starts, vec![0.0, 3.0, 4.0, 7.0, 8.0, 11.0]);
    assert_eq!(bs[2].target, "bass", "a copy plays what it copied");
    assert_eq!(bs[2].source.pattern().cloned().expect("a written pattern"), bs[0].source.pattern().cloned().expect("a written pattern"));
    assert_eq!(bs[3].source.pattern().cloned().expect("a written pattern"), bs[1].source.pattern().cloned().expect("a written pattern"));
    assert_eq!(bs[2].bars, Some(3.0), "and for as long");
}

/// The chain carries on from the end of the last pass, not the first.
#[test]
fn loop_hands_the_chain_on_after_every_pass() {
    let bs = bindings_of(&format!(
        "{ARR}playn([c3], bass, 3).then_fill([c2]).loop(2).then(one)\n"));
    assert_eq!(bs.last().unwrap().start, 8.0);
}

/// Lanes and rates ride along, because a pass is a copy of the binding and not
/// a re-reading of the source.
#[test]
fn loop_copies_lanes_and_rate() {
    let bs = bindings_of(&format!(
        "{ARR}playn([c3], bass, 3, 2, cut: [500, 900]).loop(2)\n"));
    assert_eq!(bs.len(), 2);
    assert_eq!(bs[1].start, 1.5, "1.5 bars at rate 2");
    assert_eq!(bs[1].lanes, bs[0].lanes);
    assert_eq!(bs[1].source.pattern().cloned().expect("a written pattern"), bs[0].source.pattern().cloned().expect("a written pattern"));
}

/// A `with` is part of the chain too, so both parts come round together.
#[test]
fn loop_repeats_everything_laid_alongside() {
    let bs = bindings_of(&format!("{ARR}playn([c3], bass, 2).with(two).loop(2)\n"));
    assert_eq!(bs.len(), 4);
    let starts: Vec<f64> = bs.iter().map(|b| b.start).collect();
    assert_eq!(starts, vec![0.0, 0.0, 2.0, 2.0]);
}

/// One pass is the chain itself, untouched.
#[test]
fn loop_of_one_writes_nothing_new() {
    let bs = bindings_of(&format!("{ARR}playn([c3], bass, 3).loop(1).then(one)\n"));
    assert_eq!(bs.len(), 2, "the play, and what followed it");
    assert_eq!(bs[1].start, 3.0);
}

/// A second pass over something that never ends would play over the first.
#[test]
fn loop_refuses_an_endless_chain() {
    let e = play_err(&format!("{ARR}play([c3], bass).loop(2)\n"));
    assert!(e.contains("never finishes"), "got: {e}");
}

/// `take` is the way out of that, and it bounds the loop as well.
#[test]
fn loop_takes_a_chain_that_take_has_bounded() {
    let bs = bindings_of(&format!("{ARR}play([c3], bass).take(2).loop(3)\n"));
    assert_eq!(bs.len(), 3);
    assert_eq!(bs[2].start, 4.0);
    assert_eq!(bs[2].bars, Some(2.0), "the cut came with the copy");
}

/// A choice already comes back around on a period of its own, and two
/// repetitions of one binding is one too many.
#[test]
fn loop_refuses_a_chain_containing_a_choice() {
    let e = play_err(&format!("{ARR}playn([c3], bass, 2).rthen([one, two]).take(4).loop(2)\n"));
    assert!(e.contains("choice"), "got: {e}");
}

#[test]
fn loop_refuses_a_count_that_is_not_a_whole_number() {
    let e = play_err(&format!("{ARR}playn([c3], bass, 2).loop(2.5)\n"));
    assert!(e.contains("whole number"), "got: {e}");
}

/// Each pass is real bindings, so an absurd count is a memory problem.
#[test]
fn loop_refuses_an_absurd_count() {
    let e = play_err(&format!("{ARR}playn([c3], bass, 2).loop(100000)\n"));
    assert!(e.contains("a typo?"), "got: {e}");
}

/// A loop is a chain in its own right: what follows chains onto all of it.
#[test]
fn a_loop_can_be_looped() {
    let bs = bindings_of(&format!("{ARR}playn([c3], bass, 2).loop(2).loop(2)\n"));
    assert_eq!(bs.len(), 4);
    let starts: Vec<f64> = bs.iter().map(|b| b.start).collect();
    assert_eq!(starts, vec![0.0, 2.0, 4.0, 6.0]);
}

/// The hazard `quantize` exists for: `rate` divides into the count, so this
/// section is 1.5 bars long and everything after it would be off the beat.
#[test]
fn quantize_rounds_a_fractional_section_up() {
    let bs = bindings_of(&format!("{ARR}playn([c3], bass, 3, 2).then(one)\n"));
    assert_eq!(bs[1].start, 1.5, "unquantized, and off the downbeat");

    let bs = bindings_of(&format!("{ARR}playn([c3], bass, 3, 2).quantize().then(one)\n"));
    assert_eq!(bs[1].start, 2.0);
}

/// A section already on the grid is left where it is rather than pushed out.
#[test]
fn quantize_leaves_an_exact_multiple_alone() {
    let bs = bindings_of(&format!("{ARR}playn([c3], bass, 4).quantize(4).then(one)\n"));
    assert_eq!(bs[1].start, 4.0);
}

#[test]
fn quantize_takes_a_grid() {
    let bs = bindings_of(&format!("{ARR}playn([c3], bass, 5).quantize(4).then(one)\n"));
    assert_eq!(bs[1].start, 8.0);
}

/// Quantizing moves where the *next* section starts without shortening what is
/// already playing.
#[test]
fn quantize_does_not_cut_what_is_playing() {
    let bs = bindings_of(&format!("{ARR}playn([c3], bass, 3, 2).quantize().then(one)\n"));
    assert_eq!(bs[0].bars, Some(1.5));
}

/// The rest a section quantized to is part of the section, so naming it as a
/// `fn` keeps it. Read off the bindings instead, the padding is invisible — no
/// note was written into it — and the section would silently shorten to its
/// last note the moment it was given a name.
#[test]
fn a_named_section_keeps_the_rest_it_quantized_to() {
    let bs = bindings_of(&format!(
        "{ARR}fn rounded() = playn([c3], bass, 3, 2).quantize()\n\
         rounded().then(one)\n"));
    assert_eq!(bs[0].bars, Some(1.5), "the notes are unchanged");
    assert_eq!(bs[1].start, 2.0, "and what follows starts at the quantized end");
}

/// And `.loop` repeats the rest along with the notes. Without it a loop comes
/// back early by exactly the padding that was asked for — the section is over,
/// on paper, at its last note.
#[test]
fn a_loop_repeats_the_rest_a_section_quantized_to() {
    let bs = bindings_of(&format!(
        "{ARR}fn rounded() = playn([c3], bass, 3, 2).quantize()\n\
         rounded().then(rounded).loop(2)\n"));
    let starts: Vec<f64> = bs.iter().map(|b| b.start).collect();
    assert_eq!(starts, vec![0.0, 2.0, 4.0, 6.0], "two-bar slots throughout");
}

/// `take` says how long a section is, which is a length and not only a cut:
/// asking for more than the notes fill leaves a rest at the end, and that rest
/// survives being named too.
#[test]
fn a_named_section_keeps_the_length_it_was_taken_to() {
    let bs = bindings_of(&format!(
        "{ARR}fn padded() = play_once([c4], lead).take(4)\n\
         padded().then(one)\n"));
    assert_eq!(bs[0].bars, Some(1.0), "a cut is a ceiling, so the note is untouched");
    assert_eq!(bs[1].start, 4.0);
}

/// `take` is what gives a plain `play` an end, so a chain can follow one.
#[test]
fn take_bounds_an_endless_play() {
    let e = play_err(&format!("{ARR}play([c3], bass).then(one)\n"));
    assert!(e.contains("never finishes"), "got: {e}");

    let bs = bindings_of(&format!("{ARR}play([c3], bass).take(8).then(one)\n"));
    assert_eq!(bs[0].bars, Some(8.0));
    assert_eq!(bs[1].start, 8.0);
}

/// A cut is a ceiling, not a length: a part that already stops sooner is left
/// exactly as it was.
#[test]
fn take_leaves_a_shorter_part_alone() {
    let bs = bindings_of(&format!(
        "{ARR}play_all(play([c3], bass), playn([c4], lead, 2)).take(8)\n"));
    assert_eq!(bs[0].bars, Some(8.0), "the endless one was cut");
    assert_eq!(bs[1].bars, Some(2.0), "the short one was not lengthened");
}

/// One counted pattern as the trigger to stop the endless ones around it.
#[test]
fn stop_cuts_the_endless_parts_where_the_counted_one_runs_out() {
    let bs = bindings_of(&format!(
        "{ARR}play_all(play([c3], bass), play([c5], hat), playn([c4], lead, 8)).stop()\n"));
    assert_eq!(bs[0].bars, Some(8.0));
    assert_eq!(bs[1].bars, Some(8.0));
    assert_eq!(bs[2].bars, Some(8.0));
}

/// And having stopped, the chain has an end, so something may follow it.
#[test]
fn stop_gives_the_chain_an_end() {
    let bs = bindings_of(&format!(
        "{ARR}play_all(play([c3], bass), playn([c4], lead, 4)).stop().then(one)\n"));
    assert_eq!(bs[2].start, 4.0);
}

#[test]
fn stop_refuses_a_section_that_never_finishes() {
    let e = play_err(&format!("{ARR}play_all(play([c3], bass), play([c5], hat)).stop()\n"));
    assert!(e.contains("no moment to stop at"), "got: {e}");
}

// ---- choice ----

/// Every arm is written, all starting at the same bar, all marked as arms of
/// one choice — the scheduler is what picks between them.
#[test]
fn wthen_writes_every_arm_as_one_choice() {
    let src = format!("{ARR}playn([c3], bass, 2).wthen([one, two], [0.7, 0.3])\n");
    let items = parse(src).expect("parse failed");
    let l = lower_full(&items).expect("lower failed");

    assert_eq!(l.bindings.len(), 3);
    assert_eq!(l.choices.len(), 1);
    assert_eq!(l.choices[0].weights, vec![0.7, 0.3]);

    let arms = &l.bindings[1..];
    assert!(arms.iter().all(|b| b.start == 2.0), "every arm opens at the same bar");
    assert_eq!(arms[0].choice.map(|c| c.arm), Some(0));
    assert_eq!(arms[1].choice.map(|c| c.arm), Some(1));
    // The period is the longest arm, so the short one leaves silence rather
    // than dragging the next repetition early.
    assert!(arms.iter().all(|b| b.repeat == Some(2.0)));
}

/// A choice repeats forever, so nothing may be scheduled after it until it has
/// been given a length.
#[test]
fn wthen_never_finishes_on_its_own() {
    let e = play_err(&format!(
        "{ARR}playn([c3], bass, 2).wthen([one, two], [1, 1]).then(one)\n"));
    assert!(e.contains("never finishes"), "got: {e}");

    let bs = bindings_of(&format!(
        "{ARR}playn([c3], bass, 2).wthen([one, two], [1, 1]).take(8).then(one)\n"));
    assert_eq!(bs[3].start, 10.0, "eight bars after the choice opened at 2");
}

#[test]
fn rthen_weights_every_arm_alike() {
    let src = format!("{ARR}playn([c3], bass, 2).rthen([one, two, one])\n");
    let items = parse(src).expect("parse failed");
    let l = lower_full(&items).expect("lower failed");
    assert_eq!(l.choices[0].weights, vec![1.0, 1.0, 1.0]);
}

/// `maybe` is a choice whose other arm owns no bindings, so drawing it is
/// simply silence.
#[test]
fn maybe_is_a_choice_against_silence() {
    let src = format!("{ARR}playn([c3], bass, 4).maybe(0.25, one)\n");
    let items = parse(src).expect("parse failed");
    let l = lower_full(&items).expect("lower failed");

    assert_eq!(l.bindings.len(), 2);
    assert_eq!(l.choices.len(), 1);
    assert_eq!(l.choices[0].weights, vec![0.25, 0.75]);
    assert_eq!(l.bindings[1].choice.map(|c| c.arm), Some(0));
    assert_eq!(l.bindings[1].repeat, Some(1.0));
}

/// Unlike a weighted choice, this plays all of them — the order is what varies.
#[test]
fn shuffle_then_plays_every_section_once() {
    let bs = bindings_of(&format!("{ARR}one().shuffle_then([two, two, two])\n"));
    assert_eq!(bs.len(), 4);
    assert_eq!(bs[1].start, 1.0);
    assert_eq!(bs[2].start, 3.0);
    assert_eq!(bs[3].start, 5.0);
    // It settles at eval time, so it has a length and no choice was recorded.
    let items = parse(format!("{ARR}one().shuffle_then([two, two])\n")).unwrap();
    assert!(lower_full(&items).unwrap().choices.is_empty());
}

/// Every arm has to come back to the same place, so an endless one is refused
/// rather than silently taking the choice over.
#[test]
fn a_choice_refuses_an_endless_arm() {
    let e = play_err(&format!("{ARR}one().rthen([one, endless])\n"));
    assert!(e.contains("never finishes"), "got: {e}");
}

#[test]
fn wthen_needs_one_weight_per_section() {
    let e = play_err(&format!("{ARR}one().wthen([one, two], [1])\n"));
    assert!(e.contains("2 sections but 1 weights"), "got: {e}");
}

#[test]
fn wthen_refuses_weights_that_are_all_zero() {
    let e = play_err(&format!("{ARR}one().wthen([one, two], [0, 0])\n"));
    assert!(e.contains("every weight is zero"), "got: {e}");
}

#[test]
fn maybe_refuses_a_chance_outside_zero_to_one() {
    let e = play_err(&format!("{ARR}one().maybe(1.5, one)\n"));
    assert!(e.contains("between 0 and 1"), "got: {e}");
}

/// A nested choice keeps its own group rather than being swallowed by the
/// outer one.
#[test]
fn a_choice_inside_an_arm_gets_its_own_group() {
    let src = format!(
        "{ARR}fn inner() = one().maybe(0.5, one)\n\
         playn([c3], bass, 2).rthen([one, two])\n");
    let items = parse(src).expect("parse failed");
    let l = lower_full(&items).expect("lower failed");
    // `inner` is never called, so only the one choice exists; what matters is
    // that indices are handed out before an arm is lowered, which the nested
    // case below actually exercises.
    assert_eq!(l.choices.len(), 1);

    let src = format!(
        "{ARR}fn inner() = playn([c3], bass, 2).maybe(0.5, one).take(4)\n\
         one().rthen([inner, two])\n");
    let items = parse(src).expect("parse failed");
    let l = lower_full(&items).expect("lower failed");
    assert_eq!(l.choices.len(), 2, "the outer choice and the nested one");
    let groups: Vec<_> = l.bindings.iter().filter_map(|b| b.choice.map(|c| c.group)).collect();
    assert!(groups.contains(&0) && groups.contains(&1), "got: {groups:?}");
}

/// One arrangement using most of the family at once, as a guard against the
/// combinators composing in principle but not in practice.
#[test]
fn a_whole_arrangement_lowers() {
    let src = format!(
        "{ARR}\
fn groove() = playn([c3, `, c3, c3], bass, 4)
fn verse() = playn([c4, e4, g4], lead, 2).with(groove)
fn bridge() = playn([g3, b3], lead, 2)
play_once([c2], bass)
  .then_n(verse, 2)
  .then_each([1, 2], faster)
  .then(groove)
  .then_fill([c2, c2, c2, c2])
  .quantize(4)
  .wthen([verse, bridge], [0.7, 0.3])
  .take(16)
  .then(one)
");
    let items = parse(src).expect("parse failed");
    let l = lower_full(&items).expect("lower failed");

    assert_eq!(l.choices.len(), 1, "one choice, from the wthen");
    // Nothing may be left unbounded: `take` closed the choice, and every
    // section before it was counted.
    assert!(l.bindings.iter().all(|b| b.bars.is_some()));
    // The arms of the choice repeat; nothing before them does.
    let repeating = l.bindings.iter().filter(|b| b.repeat.is_some()).count();
    assert!(repeating > 0, "the choice's arms should repeat");
    assert!(repeating < l.bindings.len(), "only the arms should repeat");

    // And it realizes, so every instrument the arrangement named really builds.
    assert!(crate::swync_graph::realizer::realize(&l.graph).is_ok());
}


/// The arrangement examples in the README, compiled.
///
/// Written after one of them turned out not to work: `.with(drums)` makes the
/// handle cover two plays, so the `.then_fill` that followed it in the first
/// draft could not find the one instrument a fill is played by. A snippet
/// nobody compiles is a snippet that drifts.
#[test]
fn the_readme_arrangement_examples_compile() {
    const NAMES: &str = "\
fn lead(n) = sin(n)
fn inst(n) = saw(n)
let riff = [c4, e4, g4]
let pat = [c3, e3]
fn drums() = playn([c2, `, c2, c2], inst, 4)
fn chorus() = playn([g4, b4], lead, 2)
fn verse() = playn([c4, e4], lead, 2)
fn outro() = play_once([c2], inst)
";
    for example in [
        "playn(riff, lead, 4)\n  .then_fill([1, 1, 1, 1])\n  .then_n(chorus, 2)\n  .then(outro)",
        "playn(riff, lead, 3)\n  .then_fill([1, 1, 1, 1])\n  .loop(4)\n  .then(chorus)",
        "playn(riff, lead, 4).with(drums).then(chorus)",
        "playn(pat, inst, 3, 2).then(chorus)",
        "playn(pat, inst, 3, 2).quantize().then(chorus)",
        "playn(riff, lead, 2)\n  .wthen([verse, chorus], [0.7, 0.3])\n  .take(32)\n  .then(outro)",
    ] {
        let src = format!("{NAMES}{example}\n");
        let items = parse(src.clone())
            .unwrap_or_else(|e| panic!("{example}\n  failed to parse: {e}"));
        lower_full(&items)
            .unwrap_or_else(|e| panic!("{example}\n  failed to lower: {e}"));
    }
}

/// Written note values end to end: what someone types, as the rhythm it turns
/// into.
///
/// Two things are being pinned. One is the arithmetic — that a bar is as long
/// as its values add up to, and that a tuplet is played in the span its
/// contents imply. The other is the boundary: written values and shares are
/// alternative readings of the same brackets, and every way of writing both at
/// once has to be refused rather than resolved in one of their favours.
#[cfg(test)]
mod metrical_tests {
    use super::*;
    use crate::pattern::pattern::{Slot, Span, UNIT};
    use crate::scheduler::clock::Meter;

    const TONE: &str = "fn tone(n, cut = 800) = saw(n)\n";

    fn pattern_of(src: &str) -> Pattern {
        bindings_of(src).into_iter().next().expect("a binding").source.pattern().cloned().expect("a written pattern")
    }

    /// Onsets and durations in bars, which is the only thing about a pattern
    /// that a written value is claiming to control.
    fn timing(p: &Pattern, bars: f64) -> (Vec<f64>, Vec<f64>) {
        let evs = p.query(Span::new(0.0, bars));
        (evs.iter().map(|e| e.begin).collect(), evs.iter().map(|e| e.duration()).collect())
    }

    fn binding_bars(src: &str) -> Option<f64> {
        bindings_of(src).into_iter().next().expect("a binding").bars
    }

    // ---- compatibility ----

    /// The promise the whole design rests on. Four quarters are four beats are
    /// one bar, so the sequence is already the length a bare sequence is and
    /// nothing is wrapped around it — the pattern is the same value a list of
    /// four plain steps has always lowered to.
    #[test]
    fn a_full_bar_of_quarter_notes_is_the_plain_even_division() {
        let written = pattern_of(&format!("{TONE}play([220;q, 330, 440, 550], tone)\n"));
        let plain = pattern_of(&format!("{TONE}play([220, 330, 440, 550], tone)\n"));
        assert_eq!(written, plain);
    }

    /// A list with no written value in it takes the other reading entirely,
    /// groups included — this is every pattern that exists today and everything
    /// the drawn-pattern panel writes.
    #[test]
    fn a_group_still_takes_a_share_in_a_sequence_of_shares() {
        let p = pattern_of(&format!("{TONE}play([220;3, [330, 440]], tone)\n"));
        let slots = match &p {
            Pattern::Steps(s) => s.clone(),
            other => panic!("expected a bare sequence, got {other:?}"),
        };
        assert_eq!(slots[0].length, 3.0);
        assert_eq!(slots[1].length, UNIT);
        assert!(matches!(slots[1].step, Step::Group(_)));
    }

    // ---- division ----

    /// Polymeter, and the reason a bare metrical list needs no marker: three
    /// quarters is three beats, and in 4/4 that is three quarters of a bar.
    ///
    /// This is the case that makes *pass* and *bar* two words rather than one.
    /// What is written here is a three-beat phrase — a musician would call it
    /// a 3/4 bar — but the grid it is played against is still four beats, so
    /// it does not fill a bar and does not claim to. Write it in 3/4 and it
    /// does; see `a_three_beat_pass_fills_a_three_four_bar`.
    #[test]
    fn a_three_beat_pass_takes_three_quarters_of_a_four_four_bar() {
        let p = pattern_of(&format!("{TONE}play([220;q, 330, 440], tone)\n"));
        let (onsets, durations) = timing(&p, 0.75);
        assert_eq!(onsets, vec![0.0, 0.25, 0.5]);
        assert_eq!(durations, vec![0.25, 0.25, 0.25], "each is a quarter of a bar");
        // The pass comes round at 0.75 rather than at 1, so two of them are one
        // and a half bars — and the second straddles the bar line, which is
        // what rotating against the grid means.
        assert_eq!(p.query(Span::new(0.0, 1.5)).len(), 6);
    }

    /// A value carries to the steps after it, so a bar states what it is once.
    #[test]
    fn a_duration_carries_to_the_notes_after_it() {
        let p = pattern_of(&format!("{TONE}play([220;e, 330, 440;q, 550], tone)\n"));
        let (onsets, durations) = timing(&p, 0.75);
        // An eighth, an eighth, a quarter, a quarter: three beats in all.
        assert_eq!(durations, vec![0.125, 0.125, 0.25, 0.25]);
        assert_eq!(onsets, vec![0.0, 0.125, 0.25, 0.5]);
    }

    /// A bare `q` is a hit of that length — a written value carries no pitch,
    /// which is what a trigger already means.
    #[test]
    fn a_bare_written_value_is_a_hit_of_that_length() {
        let p = pattern_of("fn hit() = sin(50)\nplay([q, q, q], hit)\n");
        let (onsets, durations) = timing(&p, 0.75);
        assert_eq!(onsets, vec![0.0, 0.25, 0.5]);
        assert_eq!(durations, vec![0.25, 0.25, 0.25]);
    }

    /// Rests take the value in force, which is how a bar ends in silence.
    #[test]
    fn a_rest_takes_the_value_in_force() {
        let p = pattern_of(&format!("{TONE}play([220;h, `], tone)\n"));
        let (onsets, durations) = timing(&p, 1.0);
        assert_eq!(onsets, vec![0.0], "the rest sounds nothing");
        assert_eq!(durations, vec![0.5], "but it still takes its half of the bar");
    }

    // ---- tuplets ----

    /// The case that started this: three quarters played in the time of two.
    #[test]
    fn a_quarter_triplet_fills_a_half_note() {
        let p = pattern_of(&format!("{TONE}play([[220;q, 330, 440];t, 550;q], tone)\n"));
        let (onsets, durations) = timing(&p, 1.0);
        // Three beats in the bar: the triplet owns two of them, the quarter one.
        assert_eq!(durations[3], 0.25, "the quarter after it is untouched");
        assert!((durations[0] - 1.0 / 6.0).abs() < 1e-9, "got {}", durations[0]);
        assert!((onsets[3] - 0.5).abs() < 1e-9, "the triplet ends on the third beat");
    }

    /// The same span reached by a different count, which is what makes the
    /// notation's silence about "3" or "5" honest.
    #[test]
    fn an_eighth_quintuplet_also_fills_a_half_note() {
        let src = format!("{TONE}play([[220;e, 330, 440, 550, 660];t, 220;q, 330], tone)\n");
        let p = pattern_of(&src);
        let (onsets, _) = timing(&p, 1.0);
        assert_eq!(onsets.len(), 7);
        // Five eighths in the time of four: a half note, then two quarters.
        assert!((onsets[5] - 0.5).abs() < 1e-9, "got {}", onsets[5]);
    }

    /// The unit follows the contents, so the same bracket at another scale
    /// gives the tuplet at that scale.
    #[test]
    fn an_eighth_triplet_fills_a_quarter() {
        let p = pattern_of(&format!("{TONE}play([[220;e, 330, 440];t, 550;q], tone)\n"));
        let (onsets, durations) = timing(&p, 1.0);
        assert!((durations[0] - 1.0 / 12.0).abs() < 1e-9, "got {}", durations[0]);
        assert!((onsets[3] - 0.25).abs() < 1e-9, "the triplet took one beat");
    }

    /// Uneven contents are legal when they fill the division: a half and a
    /// quarter are an ordinary quarter triplet with its first two tied. Reading
    /// the unit off the first value would make the count 3/2 and refuse this.
    #[test]
    fn a_half_and_a_quarter_are_a_legal_triplet() {
        // The triplet is the whole bar, so the bar is the half note it is
        // played in — half a bar.
        let p = pattern_of(&format!("{TONE}play([[220;h, 330;q];t], tone)\n"));
        let (onsets, durations) = timing(&p, 0.5);
        assert_eq!(onsets.len(), 2);
        // The whole bar is the triplet's span — a half note.
        assert!((durations[0] - 1.0 / 3.0).abs() < 1e-9, "got {}", durations[0]);
        assert!((durations[1] - 1.0 / 6.0).abs() < 1e-9, "got {}", durations[1]);
    }

    /// The other shape of the same rule: an opening note worth three of the
    /// six slots it is counted in.
    #[test]
    fn a_tuplet_may_open_with_a_note_that_fills_several_of_its_slots() {
        let src = format!("{TONE}play([[220;q, 330;e, 440, 550, 660];t], tone)\n");
        let p = pattern_of(&src);
        let (_, durations) = timing(&p, 0.5);
        // Six eighths played in four: the bar is a half note, and the opening
        // quarter fills two of the six slots — a third of the group.
        assert_eq!(durations.len(), 5);
        // A third of a half note: two of the six slots, in a group half a bar long.
        assert!((durations[0] - 1.0 / 6.0).abs() < 1e-9, "got {}", durations[0]);
    }

    /// The property that makes the missing number on `;t` sound: the two ways
    /// of reading a group agree about how long it is.
    #[test]
    fn a_sextuplet_and_a_triplet_reading_agree() {
        // Six eighths, and the three quarters they tie into.
        let six = pattern_of(&format!(
            "{TONE}play([[220;e, 220, 220, 220, 220, 220];t], tone)\n"));
        let three = pattern_of(&format!("{TONE}play([[220;q, 220, 220];t], tone)\n"));
        let end = |p: &Pattern| {
            let evs = p.query(Span::new(0.0, 4.0));
            let last = evs.last().expect("events");
            last.end
        };
        assert!((end(&six) - end(&three)).abs() < 1e-9,
                "both are played in a half note: {} vs {}", end(&six), end(&three));
    }

    /// A value written inside a tuplet describes that tuplet and stops there,
    /// so a line's meaning does not turn on a bracket several tokens back.
    #[test]
    fn a_duration_change_inside_a_group_does_not_leak_out() {
        let leaky = pattern_of(&format!("{TONE}play([220;q, [330;e, 440, 550];t, 660], tone)\n"));
        let spelt = pattern_of(&format!("{TONE}play([220;q, [330;e, 440, 550];t, 660;q], tone)\n"));
        assert_eq!(leaky, spelt, "the step after the group is still a quarter");
    }

    // ---- refusals ----

    /// The case the whole tuplet rule exists for. Four eighths are a half note
    /// however they are bracketed, so there is no compression to mark.
    #[test]
    fn a_quarter_and_two_eighths_are_not_a_tuplet() {
        let err = play_err(&format!("{TONE}play([[220;q, 330;e, 440];t], tone)\n"));
        assert!(err.contains("nothing to compress"), "got: {err}");
        assert!(err.contains("h"), "the message should name what it came to: {err}");
    }

    /// Same check, reached the obvious way.
    #[test]
    fn a_tuplet_whose_count_is_a_power_of_two_is_refused() {
        for group in ["[220;q, 330]", "[220;q, 330, 440, 550]"] {
            let err = play_err(&format!("{TONE}play([{group};t], tone)\n"));
            assert!(err.contains("nothing to compress"), "{group} gave: {err}");
        }
    }

    /// A group in a metrical sequence is a tuplet or it is a mistake — three
    /// quarters inside a bar could be a bar of their own or a triplet, and
    /// nothing about the brackets says which.
    #[test]
    fn a_group_in_a_metrical_sequence_must_be_a_tuplet() {
        let err = play_err(&format!("{TONE}play([220;q, [330, 440]], tone)\n"));
        assert!(err.contains(";t"), "got: {err}");
    }

    /// The span comes from the contents, so there is no length to give — and
    /// giving one is how `;e` on three quarters would get written.
    #[test]
    fn a_group_in_a_metrical_sequence_cannot_be_given_a_length() {
        let err = play_err(&format!("{TONE}play([220;q, [330;q, 440, 550];e], tone)\n"));
        assert!(err.contains(";t") || err.contains("share"), "got: {err}");
    }

    /// The two readings of a `;` cannot share a sequence.
    #[test]
    fn a_duration_and_a_share_cannot_share_a_sequence() {
        let err = play_err(&format!("{TONE}play([220;q, 330;2], tone)\n"));
        assert!(err.contains("both"), "got: {err}");
    }

    /// Inheriting backwards from a later note is not something anyone should
    /// have to reason about, so the first step has to say what it is.
    #[test]
    fn a_metrical_sequence_must_state_its_first_length() {
        let err = play_err(&format!("{TONE}play([220, 330;q], tone)\n"));
        assert!(err.contains("first step"), "got: {err}");
    }

    /// A lane is indexed by which note is asking, so a length of *time* is not
    /// a thing it can hold. Caught before the fractional check, which `q` would
    /// otherwise pass by reading as the number 1.
    #[test]
    fn a_note_value_in_a_lane_is_refused() {
        let err = play_err(&format!("{TONE}play([220, 330], tone, cut: [400;q, 2000])\n"));
        assert!(err.contains("read by note"), "got: {err}");
        assert!(err.contains("cut"), "the message should name the lane: {err}");
    }

    /// `;t` is a mark on a group, not a length in its own right.
    #[test]
    fn a_tuplet_marker_needs_a_group() {
        let err = play_err(&format!("{TONE}play([220;t, 330], tone)\n"));
        assert!(err.contains("group"), "got: {err}");
    }

    /// A written value shadows like a note name does, so a program that used
    /// `e` as a name before any of this existed still means what it meant.
    #[test]
    fn a_binding_shadows_a_written_value() {
        let p = pattern_of(&format!("{TONE}let e = 330\nplay([220, e], tone)\n"));
        let (onsets, _) = timing(&p, 1.0);
        assert_eq!(onsets, vec![0.0, 0.5], "two plain steps, evenly divided");
    }

    // ---- what a pass is worth ----

    /// `playn` counts passes and binds in bars, and a metrical pass is not a
    /// bar. Four passes of a three-beat bar are three bars.
    #[test]
    fn playn_counts_passes_not_bars_on_a_metrical_pattern() {
        let bars = binding_bars(&format!("{TONE}playn([220;q, 330, 440], tone, 4)\n"));
        assert_eq!(bars, Some(3.0));
    }

    /// And the conversion composes with `rate`, which is the other thing
    /// standing between a pass and a bar.
    #[test]
    fn rate_still_compounds_with_a_metrical_sequence() {
        let bars = binding_bars(&format!("{TONE}playn([220;q, 330, 440], tone, 4, 2)\n"));
        assert_eq!(bars, Some(1.5), "same four passes, twice as fast");
    }

    /// A pattern written in shares is untouched by any of it.
    #[test]
    fn playn_on_a_sequence_of_shares_is_unchanged() {
        let bars = binding_bars(&format!("{TONE}playn([220, 330, 440], tone, 4)\n"));
        assert_eq!(bars, Some(4.0));
    }

    // ---- dots and ties ----

    /// A dotted quarter is a quarter and an eighth, which is what makes
    /// `[c4;q.dot, e4;e]` a two-beat bar rather than a two-and-a-half one.
    #[test]
    fn a_dotted_quarter_is_a_quarter_and_an_eighth() {
        let dotted = pattern_of(&format!("{TONE}play([220;q.dot, 330;e], tone)\n"));
        let spelt = pattern_of(&format!("{TONE}play([220;e, 220, 220, 330], tone)\n"));
        // Three eighths against one: the same division, reached two ways — the
        // dotted one holding its three as a single note.
        let (onsets, durations) = timing(&dotted, 0.5);
        assert_eq!(onsets, vec![0.0, 0.375]);
        assert_eq!(durations, vec![0.375, 0.125]);
        assert_eq!(timing(&spelt, 0.5).0.len(), 4, "the undotted spelling strikes four");
    }

    /// Each dot adds half of the note, not half of what the last one left — so
    /// the count is a parameter and `q.dot(2)` is 7/4 rather than 9/4.
    #[test]
    fn a_double_dot_adds_half_and_then_a_quarter() {
        let double = pattern_of(&format!("{TONE}play([220;q.dot(2), 330;s], tone)\n"));
        // 7/4 + 1/4 beats is two beats, which is half a bar.
        let (_, durations) = timing(&double, 0.5);
        assert!((durations[0] - 0.4375).abs() < 1e-9, "got {}", durations[0]);
        assert!((durations[1] - 0.0625).abs() < 1e-9, "got {}", durations[1]);
    }

    /// A tie is addition, and reads as one held note rather than two struck.
    #[test]
    fn adding_two_values_ties_them() {
        let tied = pattern_of(&format!("{TONE}play([220;h + e, 330;e], tone)\n"));
        let (onsets, durations) = timing(&tied, 0.75);
        assert_eq!(onsets.len(), 2, "a tie is one note, not two");
        assert!((durations[0] - 0.625).abs() < 1e-9, "got {}", durations[0]);
    }

    /// And `q + e` is the dotted quarter, so the two spellings agree.
    #[test]
    fn a_tie_and_a_dot_can_reach_the_same_value() {
        let tied = pattern_of(&format!("{TONE}play([220;q + e, 330;e], tone)\n"));
        let dotted = pattern_of(&format!("{TONE}play([220;q.dot, 330;e], tone)\n"));
        assert_eq!(tied, dotted);
    }

    /// Only addition. Notation has no way to draw a written value divided by
    /// another, so the rest of the arithmetic is refused rather than folded.
    #[test]
    fn written_values_cannot_be_multiplied() {
        let err = play_err(&format!("{TONE}play([220;q * e, 330;e], tone)\n"));
        assert!(err.contains("added"), "got: {err}");
    }

    /// `dot` says what it wants, on the call that has the mistake in it.
    #[test]
    fn dotting_something_that_is_not_a_value_is_refused() {
        let err = play_err(&format!("{TONE}play([220;4.dot, 330], tone)\n"));
        assert!(err.contains("written note value"), "got: {err}");
    }

    /// A dotted value still counts into a tuplet exactly — the reason the type
    /// is a rational rather than an `f64`.
    #[test]
    fn a_dotted_value_counts_into_a_tuplet() {
        // A dotted quarter and three eighths is six eighths, played in four.
        let p = pattern_of(&format!(
            "{TONE}play([[220;q.dot, 330;e, 440, 550];t], tone)\n"));
        let (_, durations) = timing(&p, 0.5);
        assert_eq!(durations.len(), 4);
        // The dotted quarter is three of the six slots, so half the group.
        assert!((durations[0] - 0.25).abs() < 1e-9, "got {}", durations[0]);
    }

    /// The examples in the README's rhythm section, compiled. Prose that claims
    /// a bar is legal and prose that claims one is refused are both testable,
    /// and both go stale silently otherwise.
    #[test]
    fn the_readme_rhythm_examples_behave_as_written() {
        for good in [
            "play([c4;q, e4, g4], tone)",
            "play([c4;q, e4, g4, c5], tone)",
            "play([q, q, q], tone)",
            "play([[c4;q, e4, g4];t, c5;q], tone)",
            "play([[c4;e, d4, e4, f4, g4];t], tone)",
            "play([[c4;h, e4;q];t], tone)",
            "play([c4;q, [e4;e, f4, g4];t, c5], tone)",
            "play([c4;q.dot, e4;e], tone)",
            "play([c4;h + e, e4;e], tone)",
            "play(for i in 0..=4 { f4;e }, tone)",
            "play(for i in 0..=7 { 60 + i;s }, tone)",
            "play(for i in 1..=3 { 220 * i;i }, tone)",
        ] {
            let src = format!("{TONE}{good}\n");
            let items = parse(src).expect("parse failed");
            assert!(lower_full(&items).is_ok(), "{good} should compile");
        }
        // Four eighths is a half note however it is bracketed.
        let err = play_err(&format!("{TONE}play([[c4;q, e4;e, g4];t], tone)\n"));
        assert!(err.contains("nothing to compress"), "got: {err}");
    }

    /// The paradigms cannot be mixed across a nesting either. A group is given
    /// exactly one bar of slot-local time, so a bar half a bar long would
    /// sound its contents twice inside its own slot — a stutter, not a rhythm.
    #[test]
    fn a_metrical_group_cannot_sit_in_a_sequence_of_shares() {
        let err = play_err(&format!("{TONE}play([220;2, [330;q, 440]], tone)\n"));
        assert!(err.contains("note values"), "got: {err}");
    }

    /// But a group that does come to a bar is no different from the same
    /// steps written as shares, so there is nothing to refuse.
    #[test]
    fn a_group_of_exactly_one_bar_still_nests_in_shares() {
        let written = pattern_of(&format!(
            "{TONE}play([220;2, [330;q, 440, 550, 660]], tone)\n"));
        let plain = pattern_of(&format!("{TONE}play([220;2, [330, 440, 550, 660]], tone)\n"));
        assert_eq!(written, plain);
    }

    /// A lane advances by note whatever the bar is, so an odd bar walks it
    /// rather than locking it — the 3-against-4 the lane design is for.
    #[test]
    fn a_lane_drifts_against_an_odd_bar() {
        let bs = bindings_of(&format!(
            "{TONE}play([220;q, 330, 440], tone, cut: [400, 2000])\n"));
        assert_eq!(bs[0].lanes[0].steps().values(), vec![Some(400.0), Some(2000.0)]);
    }

    // ---- the signature ----
    //
    // Everything above is written in 4/4, where a bar is four beats. These pin
    // what the time signature changes, and — just as importantly — what it
    // does not.

    fn pattern_in(src: &str, meter: Meter) -> Pattern {
        let items = parse(src.to_string()).expect("parse failed");
        crate::lowerer::lower::lower_in_meter(&items, meter)
            .expect("lower failed")
            .bindings
            .into_iter()
            .next()
            .expect("a binding")
            .source.pattern().cloned().expect("a written pattern")
    }

    fn binding_bars_in(src: &str, meter: Meter) -> Option<f64> {
        let items = parse(src.to_string()).expect("parse failed");
        crate::lowerer::lower::lower_in_meter(&items, meter)
            .expect("lower failed")
            .bindings
            .into_iter()
            .next()
            .expect("a binding")
            .bars
    }

    const THREE_FOUR: Meter = Meter { top: 3, bottom: 4 };
    const SIX_EIGHT: Meter = Meter { top: 6, bottom: 8 };

    /// The question a musician asks first, and the reason the signature
    /// exists: three quarters written in 3/4 is *a bar*, not three quarters of
    /// one. The same three notes in 4/4 come round at 0.75 and rotate against
    /// the grid — see `a_three_beat_pass_takes_three_quarters_of_a_four_four_bar`, which
    /// pins exactly that. Both readings are right; the signature is what says
    /// which one was meant.
    #[test]
    fn a_three_beat_pass_fills_a_three_four_bar() {
        let p = pattern_in(&format!("{TONE}play([220;q, 330, 440], tone)\n"), THREE_FOUR);
        let (onsets, durations) = timing(&p, 1.0);
        assert_eq!(onsets, vec![0.0, 1.0 / 3.0, 2.0 / 3.0], "three even beats, filling the bar");
        assert_eq!(durations.len(), 3);
        // One pass to a bar, so a bar's query holds exactly one of each note —
        // where in 4/4 the same query catches the first note of the next pass.
        assert_eq!(p.query(Span::new(0.0, 1.0)).len(), 3);
        assert_eq!(p.query(Span::new(0.0, 2.0)).len(), 6);
    }

    /// And a bare list of shares fills the bar in any signature, which is the
    /// property that makes "a list is a bar" true rather than nearly true. It
    /// is also why nothing here needed a conversion: shares were never counted
    /// in beats.
    #[test]
    fn a_bare_list_fills_the_bar_in_any_signature() {
        for meter in [Meter::default(), THREE_FOUR, SIX_EIGHT] {
            let p = pattern_in(&format!("{TONE}play([220, 330, 440], tone)\n"), meter);
            let (onsets, _) = timing(&p, 1.0);
            assert_eq!(onsets, vec![0.0, 1.0 / 3.0, 2.0 / 3.0], "in {meter}");
        }
    }

    /// Six eighths and three quarters are the same length, so a bar of 6/8
    /// takes the same pass a bar of 3/4 does. What differs between the two is
    /// where the accents fall, which is the writer's business and not the
    /// clock's.
    #[test]
    fn six_eight_is_as_long_as_three_four() {
        let six = pattern_in(&format!("{TONE}play([220;e, 330, 440, 550, 660, 770], tone)\n"),
                             SIX_EIGHT);
        let (onsets, _) = timing(&six, 1.0);
        assert_eq!(onsets.len(), 6, "six eighths fill the bar");
        assert_eq!(
            binding_bars_in(&format!("{TONE}playn([220;q, 330, 440], tone, 2)\n"), SIX_EIGHT),
            Some(2.0),
            "and three quarters still fill it, since 6/8 and 3/4 are one length",
        );
    }

    /// A counted section is as long as its passes, so the signature reaches
    /// arrangement through the same arithmetic it reaches rhythm through: four
    /// passes of a three-beat pattern are three bars in 4/4 and four in 3/4.
    /// That is the whole practical point — `take` and `then` stop counting a
    /// unit the music does not use.
    #[test]
    fn a_counted_section_follows_the_signature() {
        let src = format!("{TONE}playn([220;q, 330, 440], tone, 4)\n");
        assert_eq!(binding_bars_in(&src, Meter::default()), Some(3.0), "four three-beat passes in 4/4");
        assert_eq!(binding_bars_in(&src, THREE_FOUR), Some(4.0), "one pass to a bar in 3/4");
    }

    /// A pass that disagrees with the bar still rotates against it — the
    /// signature moves the bar line, it does not outlaw polymeter. Four beats
    /// written in 3/4 is the same relationship as three beats in 4/4, the
    /// other way up.
    #[test]
    fn a_four_beat_pass_rotates_against_a_three_four_bar() {
        let p = pattern_in(&format!("{TONE}play([220;q, 330, 440, 550], tone)\n"), THREE_FOUR);
        let (onsets, _) = timing(&p, 4.0 / 3.0);
        assert_eq!(onsets.len(), 4, "the pass is four thirds of a bar");
        assert_eq!(binding_bars_in(&format!("{TONE}playn([220;q, 330, 440, 550], tone, 3)\n"),
                                   THREE_FOUR),
                   Some(4.0),
                   "so three passes come back onto the bar line after four bars");
    }
}
















// ---- enums ----

/// The two halves of what an enum member is, in one program: `Tuning.a` is read
/// as 440 where a number is wanted, and is still its own tag when compared.
#[test]
fn a_member_holding_a_number_is_that_number_where_a_number_is_wanted() {
    assert_eq!(enum_num("Tuning.a"), 440.0);
    assert_eq!(enum_num("Tuning.a / 2"), 220.0);
}

/// The named-constant half: a member declared as a list is a list at every
/// place a list is read, so `scale` takes one without being told about enums.
#[test]
fn a_member_holding_a_list_is_that_list_where_a_list_is_wanted() {
    assert_eq!(enum_num("61.scale(Scale.major)"), 60.0);
    assert_eq!(enum_num("len(Scale.pentatonic)"), 4.0);
    assert_eq!(enum_num("Scale.pentatonic[2]"), 5.0);
}

/// The tag half: identity, not value. Both members here hold nothing, so there
/// is no value for this to be answering from.
#[test]
fn members_of_one_enum_compare_by_which_member_they_are() {
    assert_eq!(enum_num("if Section.verse == Section.verse { 1 } else { 0 }"), 1.0);
    assert_eq!(enum_num("if Section.verse == Section.chorus { 1 } else { 0 }"), 0.0);
    assert_eq!(enum_num("if Section.verse != Section.chorus { 1 } else { 0 }"), 1.0);
}

/// Two members given the same value are still two members. This is the case
/// that a value-based equality would get wrong, and the reason the member is
/// opaque rather than transparent.
#[test]
fn two_members_holding_the_same_value_are_not_equal() {
    let src = "enum Pair { a = 1, b = 1 }\n\
               sin(if Pair.a == Pair.b { 1 } else { 0 })\n";
    let g = lower_src(src).expect("lower failed");
    assert_eq!(g.nodes[0].inputs[0], Const(0.0));
}

/// Comparing across two enums is refused rather than answered `false`: no
/// program means to ask it, and `false` is the answer that lets it run forever.
#[test]
fn comparing_members_of_different_enums_is_an_error() {
    let err = enum_err("Section.verse == Scale.major");
    assert!(err.contains("different enums"), "got: {err}");
}

/// Members are a set, not a scale — the order they were written in is a fact
/// about the file, not about the music.
#[test]
fn members_cannot_be_ordered() {
    let err = enum_err("Section.verse < Section.chorus");
    assert!(err.contains("`==` and `!=`"), "got: {err}");
}

/// An enum member and a number have nothing to compare, and saying so beats
/// unwrapping one side and answering about the other.
#[test]
fn comparing_a_member_with_a_number_is_an_error() {
    let err = enum_err("Tuning.a == 440");
    assert!(err.contains("nothing to compare"), "got: {err}");
}

/// A misspelled member names the ones that exist: the enum is somewhere else in
/// the file, and this is the message that saves scrolling to it.
#[test]
fn an_unknown_member_lists_the_ones_there_are() {
    let err = enum_err("Scale.majr");
    assert!(err.contains("no member `majr`"), "got: {err}");
    assert!(err.contains("major"), "and offers the real ones: {err}");
}

/// A member reached for as data that does not hold that sort of data points at
/// the declaration, since the mention itself reads correctly.
#[test]
fn a_tag_used_as_data_is_reported_against_its_declaration() {
    let err = enum_err("Section.verse + 1");
    assert!(err.contains("holds nothing"), "got: {err}");
    assert!(err.contains("tag"), "and says what that means: {err}");
}

/// The enum itself is not one of its members.
#[test]
fn naming_the_enum_without_a_member_is_an_error() {
    let err = enum_err("Scale");
    assert!(err.contains("enum `Scale`"), "got: {err}");
}

/// A member is a value, so calling one is not a call with the wrong arity.
#[test]
fn a_member_cannot_be_called() {
    let err = enum_err("Tuning.a(2)");
    assert!(err.contains("rather than something to call"), "got: {err}");
}

/// Both halves of the collision rule, since the two arrive in whichever order
/// they were written.
#[test]
fn an_enum_and_another_definition_cannot_share_a_name() {
    let after = lower_src("let Scale = 4\nenum Scale { major }\n").unwrap_err();
    assert!(after.contains("already defined"), "got: {after}");

    let before = lower_src("enum Scale { major }\nlet Scale = 4\n").unwrap_err();
    assert!(before.contains("would take the name"), "got: {before}");

    let func = lower_src("enum Scale { major }\nfn Scale(x) = x\n").unwrap_err();
    assert!(func.contains("would take the name"), "got: {func}");
}

/// A member is evaluated once, where the enum is written — which is what makes
/// it a constant rather than an expression under a name.
#[test]
fn a_member_is_evaluated_once_at_its_declaration() {
    let src = "enum R { x = rand(0, 1000) }\n\
               sin(R.x - R.x)\n";
    let g = lower_src(src).expect("lower failed");
    assert_eq!(g.nodes[0].inputs[0], Const(0.0),
               "two mentions of one member are the same draw");
}

/// A signal cannot be a member: it is a node in a graph, and a member is read
/// wherever it is mentioned.
#[test]
fn a_member_cannot_hold_a_signal() {
    let err = lower_src("enum Bad { tone = sin(440) }\nsin(1)\n").unwrap_err();
    assert!(err.contains("is a signal"), "got: {err}");
}

/// Refused at the pattern boundary even when the member holds a number — see
/// `enum_not_a_step`. The message says what to write instead.
#[test]
fn a_member_cannot_be_a_pattern_step() {
    let src = "fn bass(n) = sin(n)\n\
               enum Tuning { a = 440 }\n\
               [Tuning.a] >> play(bass)\n";
    let err = lower_src(src).unwrap_err();
    assert!(err.contains("a pattern cannot contain `Tuning.a`"), "got: {err}");
    assert!(err.contains("Write `440` here"), "and says what to write: {err}");
}

/// An enum with no members is a declaration nothing could ever use.
#[test]
fn an_empty_enum_is_a_parse_error() {
    assert!(parse("enum Nothing { }\n".to_string()).is_err());
}

/// Two members of one name would make `E.x` mean either.
#[test]
fn a_repeated_member_name_is_a_parse_error() {
    assert!(parse("enum E { x = 1, x = 2 }\n".to_string()).is_err());
}

/// Members separate on a comma, a line break, or both — the lexer inserts a
/// terminator inside these braces exactly as it does inside a block.
#[test]
fn members_may_be_separated_by_lines_commas_or_both() {
    let one_line = "enum S { a = 1, b = 2, c = 3 }\n";
    let over_lines = "enum S {\n  a = 1\n  b = 2\n  c = 3\n}\n";
    let mixed = "enum S {\n  a = 1,\n  b = 2,\n  c = 3,\n}\n";

    for src in [one_line, over_lines, mixed] {
        let g = lower_src(&format!("{src}sin(S.a + S.b + S.c)\n"))
            .unwrap_or_else(|e| panic!("failed on {src:?}: {e}"));
        assert_eq!(g.nodes[0].inputs[0], Const(6.0), "for {src:?}");
    }
}

/// The declarations every enum test above is written against.
const ENUMS: &str = "enum Scale { major = [0, 2, 4, 5, 7, 9, 11], pentatonic = [0, 3, 5, 7] }\n\
                     enum Section { verse, chorus }\n\
                     enum Tuning { a = 440 }\n";

/// Lower an expression with those declarations in front of it, and read back
/// the constant it folded to.
fn enum_num(src: &str) -> f64 {
    let g = lower_src(&format!("{ENUMS}sin({src})\n")).expect("lower failed");
    match g.nodes[0].inputs[0] {
        Const(v) => v,
        _ => panic!("expected a folded constant"),
    }
}

/// The same, for an expression that should be refused.
fn enum_err(src: &str) -> String {
    lower_src(&format!("{ENUMS}sin({src})\n")).unwrap_err()
}

/// An enum has to be written before it is used, exactly as a `fn` does: items
/// are lowered in order, and this is the same rule the rest of the file obeys.
/// Pinned because "the enum is further down" is a plausible thing to try.
#[test]
fn an_enum_must_be_declared_before_it_is_used() {
    let after = lower_src("sin(Scale.major[0])\nenum Scale { major = [0, 2] }\n");
    assert!(after.is_err(), "an enum used before it is written is unbound");

    // And the same program the other way round is fine, which is what says the
    // rule is about order rather than about enums.
    let before = lower_src("enum Scale { major = [0, 2] }\nsin(Scale.major[1])\n");
    assert!(before.is_ok(), "got: {before:?}");
}

// ---- playing gear instead of an instrument ----

use crate::midi::out::{Destination, DEFAULT_VELOCITY};
use crate::midi::ports::Selector;
use crate::pattern::patterns::Target;

/// Where a program's one binding is sending its notes.
fn destination_of(src: &str) -> Destination {
    match &bindings_of(src)[0].target {
        Target::Midi(destination) => destination.clone(),
        Target::Instrument(name) => panic!("expected a MIDI destination, got `{name}`"),
    }
}

#[test]
fn a_pattern_can_be_played_to_a_named_port_instead_of_an_instrument() {
    let destination = destination_of("play([60, 64], midiout(\"deluge\"))\n");
    assert_eq!(destination.selector, Selector::Name("deluge".into()));
}

#[test]
fn a_port_can_be_named_by_its_number_instead() {
    assert_eq!(destination_of("play([60], midiout(2))\n").selector, Selector::Number(2));
}

/// Channel is written the way it is spoken and the way every synth's front
/// panel prints it, which is 1-16 rather than the 0-15 on the wire.
#[test]
fn a_destination_plays_channel_one_until_it_is_told_otherwise() {
    assert_eq!(destination_of("play([60], midiout(\"x\"))\n").channel, 1);
    assert_eq!(destination_of("play([60], midiout(\"x\", 10))\n").channel, 10);
}

#[test]
fn a_channel_outside_one_to_sixteen_is_refused() {
    assert!(play_err("play([60], midiout(\"x\", 0))\n").contains("channel"));
    assert!(play_err("play([60], midiout(\"x\", 17))\n").contains("channel"));
    assert!(play_err("play([60], midiout(\"x\", 1.5))\n").contains("channel"));
}

#[test]
fn a_port_number_must_be_a_whole_number_counted_from_zero() {
    assert!(play_err("play([60], midiout(-1))\n").contains("whole number"));
    assert!(play_err("play([60], midiout(1.5))\n").contains("whole number"));
}

/// The whole of why `midiout` is intercepted before its arguments are
/// evaluated: a string is not a value in this language, so a port's name has
/// to be read off the syntax exactly as `load`'s path is.
#[test]
fn a_port_has_to_be_written_out_at_the_call_rather_than_worked_out() {
    let err = play_err("play([60], midiout(1 + 1))\n");
    assert!(err.contains("written out"), "got: {err}");
}

/// Arrangement is above the target rather than beside it, so a section that
/// sends MIDI chains like any other.
#[test]
fn a_midi_binding_can_be_bounded_and_chained_like_any_other() {
    let bindings = bindings_of(
        "fn lead(n) = sin(n)\nfn part() = play([67], lead)\n\
         playn([60, 64], midiout(\"deluge\"), 4).then(part)\n");
    assert_eq!(bindings.len(), 2);
    assert!(matches!(bindings[0].target, Target::Midi(_)));
    assert_eq!(bindings[0].bars, Some(4.0), "four passes of a one-bar pattern");
    assert_eq!(bindings[1].start, 4.0, "and the next section opens where it stopped");
}

#[test]
fn a_midi_binding_takes_the_lanes_midi_understands() {
    let bindings = bindings_of(
        "play([60, 64], midiout(\"x\"), vel: [1, .5], chan: [1, 10], legato: .5)\n");
    let lanes: Vec<&str> = bindings[0].lanes.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(lanes, vec!["vel", "chan", "legato"]);
}

/// A lane aimed at a destination has no instrument to reach, so a misspelling
/// has nowhere to land. Refusing is what keeps it from being a dynamic nobody
/// can find by reading the program.
#[test]
fn a_lane_midi_does_not_understand_is_refused_rather_than_dropped() {
    let err = play_err("play([60], midiout(\"x\"), velocity: [1])\n");
    assert!(err.contains("velocity"), "got: {err}");
    assert!(err.contains("vel"), "and it should say what there is instead: {err}");
}

/// `pan` places a voice in a stereo field, and there is no voice here — the
/// note is two bytes going out of the machine.
#[test]
fn pan_is_not_one_of_them() {
    assert!(play_err("play([60], midiout(\"x\"), pan: [-1, 1])\n").contains("pan"));
}

/// The reserved names only mean anything when there is no instrument to mean
/// something else to them, which is why they are not in `RESERVED`: an
/// instrument with its own `vel` parameter is perfectly ordinary and has
/// nothing to do with MIDI.
#[test]
fn an_instrument_may_still_declare_a_parameter_called_vel() {
    let lanes = &bindings_of("fn lead(n, vel = 0.5) = sin(n) * vel\nplay([60], lead, vel: [1])\n")[0].lanes;
    assert_eq!(lanes[0].name, "vel");
}

#[test]
fn a_destination_is_not_something_a_pattern_can_contain() {
    let err = play_err("fn lead(n) = sin(n)\nplay([midiout(\"x\")], lead)\n");
    assert!(err.contains("midiout"), "got: {err}");
}

#[test]
fn a_destination_is_not_a_signal() {
    assert!(lower_src("midiout(\"x\") * 2\n").is_err());
}

// ---- what a note comes out as ----

use crate::midi::out::Note;
use crate::pattern::patterns::LaneArg;

fn note_of(value: f64, lanes: &[(&str, f64)]) -> Note {
    let lanes: Vec<(String, LaneArg)> =
        lanes.iter().map(|(n, v)| ((*n).to_string(), LaneArg::Num(*v))).collect();
    let destination =
        Destination { selector: Selector::Name("x".into()), channel: 1 };
    Note::from_event(&destination, value, &lanes, 0.0, 1.0)
}

/// The language already counts pitch in MIDI note numbers, so nothing is
/// converted: `c4` is 60 to an instrument and 60 on the wire.
#[test]
fn a_patterns_value_is_the_midi_note_number() {
    assert_eq!(note_of(60.0, &[]).note, 60);
}

#[test]
fn a_note_between_two_numbers_lands_on_the_nearer_one() {
    assert_eq!(note_of(60.4, &[]).note, 60);
    assert_eq!(note_of(60.6, &[]).note, 61);
}

/// Clamped rather than dropped, and the difference is whether anybody finds
/// out: an octave error piling a line onto note 127 points straight at itself,
/// while the same error playing nothing looks exactly like a program that
/// should sound.
#[test]
fn a_note_outside_what_the_wire_carries_is_clamped_rather_than_dropped() {
    assert_eq!(note_of(200.0, &[]).note, 127);
    assert_eq!(note_of(-5.0, &[]).note, 0);
}

#[test]
fn velocity_is_written_from_zero_to_one_and_sent_as_seven_bits() {
    assert_eq!(note_of(60.0, &[("vel", 1.0)]).velocity, 127);
    assert_eq!(note_of(60.0, &[("vel", 0.5)]).velocity, 64);
}

#[test]
fn a_note_with_nothing_to_say_about_velocity_is_sent_at_the_usual_one() {
    assert_eq!(note_of(60.0, &[]).velocity, DEFAULT_VELOCITY);
}

/// Velocity zero *is* a note-off on the wire, so a note written silent would
/// release itself the moment it started. It is still a note somebody asked
/// for.
#[test]
fn a_note_written_silent_is_still_sent_as_a_note() {
    assert_eq!(note_of(60.0, &[("vel", 0.0)]).velocity, 1);
}

#[test]
fn a_chan_lane_overrides_the_destinations_channel_for_that_note() {
    assert_eq!(note_of(60.0, &[]).channel, 1);
    assert_eq!(note_of(60.0, &[("chan", 10.0)]).channel, 10);
}

// ---- reading a controller, and playing a keyboard ----

use crate::midi::input::{self as midi_in, Control};
use crate::pattern::patterns::SourceOf;

/// The one node a program of a single `cc`/`bend`/`aftertouch` builds, with
/// its constant inputs — which is where the device, the channel and the range
/// all end up.
fn control_node(src: &str) -> (NodeKind, Vec<f64>) {
    let graph = lower_src(src).expect("should lower");
    let node = graph.nodes.last().expect("a node");
    let consts = node
        .inputs
        .iter()
        .map(|i| match i {
            NodeInput::Const(n) => *n,
            NodeInput::Node(_) => panic!("a control reads no signals"),
        })
        .collect();
    (node.kind, consts)
}

#[test]
fn a_controller_lowers_to_a_node_holding_its_slot_channel_and_number() {
    let _bus = midi_in::exclusive();
    let (kind, consts) = control_node("cc(\"push\", 74)\n");
    assert_eq!(kind, NodeKind::Cc);
    // slot, number, channel, lo, hi.
    assert_eq!(consts, vec![0.0, 74.0, 1.0, 0.0, 1.0]);
}

/// The controller number comes before the channel, because which knob is the
/// question and which channel is a detail most rigs never answer.
#[test]
fn a_controllers_channel_is_written_after_its_number() {
    let _bus = midi_in::exclusive();
    let (_, consts) = control_node("cc(\"push\", 74, 10)\n");
    assert_eq!(consts[1], 74.0, "the number");
    assert_eq!(consts[2], 10.0, "the channel");
}

#[test]
fn a_controller_can_be_mapped_onto_a_range_at_the_call() {
    let _bus = midi_in::exclusive();
    let (_, consts) = control_node("cc(\"push\", 74, 1, 200, 5000)\n");
    assert_eq!(&consts[3..], &[200.0, 5000.0]);
}

/// A wheel rests in the middle of its travel, so its range has to put zero
/// there — otherwise every program adding one to a note would have to subtract
/// a centre it should never have had to know about.
#[test]
fn a_pitch_wheels_range_is_centred_where_the_others_start() {
    let _bus = midi_in::exclusive();
    let (kind, consts) = control_node("bend(\"keys\")\n");
    assert_eq!(kind, NodeKind::Bend);
    assert_eq!(&consts[2..], &[-1.0, 1.0]);

    let (kind, consts) = control_node("aftertouch(\"keys\")\n");
    assert_eq!(kind, NodeKind::Aftertouch);
    assert_eq!(&consts[2..], &[0.0, 1.0], "pressure starts at nothing and goes up");
}

/// Two reads of the same port share a slot, and a different port does not —
/// which is what makes the slot a small integer a node can hold at all.
#[test]
fn two_reads_of_one_port_share_its_slot() {
    let _bus = midi_in::exclusive();
    let graph = lower_src("cc(\"push\", 74) + cc(\"push\", 75) + cc(\"other\", 1)\n")
        .expect("should lower");
    let slots: Vec<f64> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Cc)
        .map(|n| match n.inputs[0] {
            NodeInput::Const(slot) => slot,
            _ => panic!("a slot is a number"),
        })
        .collect();
    assert_eq!(slots, vec![0.0, 0.0, 1.0]);
}

#[test]
fn a_range_has_to_be_both_ends_or_neither() {
    let _bus = midi_in::exclusive();
    assert!(lower_src("cc(\"push\", 74, 1, 200)\n").is_err());
}

#[test]
fn a_controller_number_outside_seven_bits_is_refused() {
    let _bus = midi_in::exclusive();
    assert!(lower_src("cc(\"push\", 128)\n").is_err());
    assert!(lower_src("cc(\"push\", 74, 17)\n").is_err(), "channel 17 does not exist");
}

/// The same rule `midiout` follows, and the same reason: a string is not a
/// value in this language, so a port's name is read off the syntax.
#[test]
fn a_controllers_port_has_to_be_written_out_at_the_call() {
    let _bus = midi_in::exclusive();
    assert!(lower_src("cc(1 + 1, 74)\n").is_err());
}

// ---- the keyboard ----

/// The keyboard a program's one binding is playing.
fn live_source(src: &str) -> crate::swync_graph::environment::Source {
    bindings_of(src)[0].source.live().expect("expected a keyboard")
}

#[test]
fn a_keyboard_can_be_played_where_a_pattern_would_go() {
    let _bus = midi_in::exclusive();
    let source = live_source("fn lead(n) = sin(n.m2h)\nplay(midiin(\"keys\"), lead)\n");
    assert_eq!(source.slot, 0);
}

/// `a.f(b)` is `f(a, b)`, so putting the source in `play`'s first slot gives
/// the chained spelling for nothing. Both have to mean the same thing.
#[test]
fn a_keyboard_reads_the_same_chained_as_it_does_written_out() {
    let _bus = midi_in::exclusive();
    let written = bindings_of("fn lead(n) = sin(n.m2h)\nplay(midiin(\"keys\"), lead)\n");
    let chained = bindings_of("fn lead(n) = sin(n.m2h)\nmidiin(\"keys\").play(lead)\n");
    assert_eq!(written[0].source, chained[0].source);
    assert_eq!(written[0].target, chained[0].target);
}

/// Every channel, which is what a keyboard on its own means — a player who has
/// never thought about MIDI channels should not have to guess which one theirs
/// is set to.
#[test]
fn a_keyboard_listens_to_every_channel_until_it_is_told_one() {
    let _bus = midi_in::exclusive();
    assert_eq!(live_source("fn l(n) = sin(n)\nplay(midiin(\"keys\"), l)\n").channel, None);
    assert_eq!(live_source("fn l(n) = sin(n)\nplay(midiin(\"keys\", 2), l)\n").channel, Some(2));
}

#[test]
fn a_keyboard_takes_lanes_like_any_other_binding() {
    let _bus = midi_in::exclusive();
    let bindings = bindings_of(
        "fn lead(n, cut = 800) = lowpass(saw(n.m2h), cut, 1)\n\
         play(midiin(\"keys\"), lead, cut: [400, 2000])\n");
    assert_eq!(bindings[0].lanes[0].name, "cut");
}

/// A keyboard has no passes to count — there is no first note to start one and
/// none to end it — so a section written round it would have no length.
#[test]
fn a_keyboard_cannot_be_given_a_number_of_passes() {
    let _bus = midi_in::exclusive();
    let err = play_err("fn l(n) = sin(n)\nplayn(midiin(\"keys\"), l, 4)\n");
    assert!(err.contains("passes"), "got: {err}");
    assert!(play_err("fn l(n) = sin(n)\nplay_once(midiin(\"keys\"), l)\n").contains("passes"));
}

/// Same argument. A rate speeds a pattern up against the bar, and the notes
/// here arrive when they are played — so a rate would be a claim the program
/// makes and the music ignores.
#[test]
fn a_keyboard_cannot_be_given_a_rate() {
    let _bus = midi_in::exclusive();
    let err = play_err("fn l(n) = sin(n)\nplay(midiin(\"keys\"), l, 2)\n");
    assert!(err.contains("rate"), "got: {err}");
}

/// It is a `Binding` like any other, which is what keeps the rest of the
/// language working around it — but one with nothing to query, since the notes
/// have not happened yet.
#[test]
fn a_keyboard_binding_answers_no_events_to_a_query() {
    let _bus = midi_in::exclusive();
    let bindings = bindings_of("fn l(n) = sin(n)\nplay(midiin(\"keys\"), l)\n");
    let patterns = crate::pattern::patterns::Patterns {
        bindings,
        ..Default::default()
    };
    let events = patterns.query(crate::pattern::pattern::Span::new(0.0, 16.0));
    assert!(events.is_empty(), "a keyboard cannot be asked about the future");
}

#[test]
fn a_keyboard_is_not_a_step_inside_a_pattern() {
    let _bus = midi_in::exclusive();
    let err = play_err("fn l(n) = sin(n)\nplay([midiin(\"keys\"), 60], l)\n");
    assert!(err.contains("midiin"), "got: {err}");
}

#[test]
fn a_keyboard_is_not_a_signal() {
    let _bus = midi_in::exclusive();
    assert!(lower_src("midiin(\"keys\") * 2\n").is_err());
}

/// Notes in and notes straight back out is a thru, which nothing here does
/// yet. It lowers — both halves are ordinary — and the scheduler drops it,
/// which is worth pinning so that the day it *is* built, this test is what
/// changes.
#[test]
fn a_keyboard_cannot_yet_be_sent_straight_back_out() {
    let _bus = midi_in::exclusive();
    let bindings = bindings_of("play(midiin(\"keys\"), midiout(\"deluge\"))\n");
    assert!(bindings[0].source.live().is_some());
    assert!(bindings[0].target.instrument().is_none());
}

/// An instrument gets the velocity only if it asked for it by name, which is
/// what lets one written long before any of this still play from a keyboard.
#[test]
fn an_instrument_is_given_velocity_only_when_it_declares_it() {
    let _bus = midi_in::exclusive();
    let items = parse(
        "fn plain(n) = sin(n)\nfn touched(n, vel = 0.5) = sin(n) * vel\n".to_string(),
    ).expect("parse failed");
    let instruments = crate::scheduler::voice::Instruments::from_program(&items);
    assert!(instruments.declares("touched", "vel"));
    assert!(!instruments.declares("plain", "vel"));
}
