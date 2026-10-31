//! `slider(name, lo = 0, hi = 1, start = lo)` — a control in the panel, named
//! where it is used.
//!
//! Intercepted before its arguments are evaluated, for the reason `load` and
//! `midiout` are: the name is a string, and a string is not a value in this
//! language. `Expr::Str` is refused everywhere else precisely so that text
//! cannot be carried around and mistaken for something that sounds, so the
//! names that take one read it off the syntax instead.
//!
//! The other half of what happens here is that **nothing is decided here**.
//! The shape of a slider — its range, where it starts, which slot it has — is
//! settled by [`crate::controls::declare_in`] walking the program's syntax
//! before lowering begins, because an instrument's body is not lowered until a
//! note plays and the panel cannot wait for one. So this looks the name up and
//! agrees with what is already on the panel, and only falls back to declaring
//! one itself where no pass has run at all, which is a test lowering a program
//! directly.
//!
//! That is also what makes "the first declaration wins" true of the *sound*
//! and not merely of the warning: a second `slider("cutoff", 0, 1)` written
//! with a different range lowers to the range the first one asked for, because
//! what it reads is the table rather than its own arguments.

use crate::controls;
use crate::lowerer::lower::Lowerer;
use crate::parser::parser::Arg;
use crate::swync_graph::environment::Value;
use crate::swync_graph::ugen_nodes::{NodeInput, NodeKind};

impl Lowerer {
    /// True when this call declares a control in the panel.
    pub fn is_control(name: &str) -> bool {
        name == controls::SLIDER || name == controls::TOGGLE
    }

    /// True when this call declares a button that fires a section.
    ///
    /// Separate from [`is_control`] because the two are intercepted for two
    /// reasons: those read a name off the syntax, and this reads a *section*
    /// off it as well — which must not be evaluated on the way in, exactly as
    /// `.then(chorus)`'s must not. See `lowerer::sections`.
    pub fn is_trigger(name: &str) -> bool {
        name == controls::TRIGGER
    }

    /// `trigger("fill", fill)` — a button that starts a section when pressed.
    ///
    /// The section is lowered **here, now, in full**, at offset zero, and then
    /// marked as belonging to this button. Nothing waits for the press and
    /// nothing is compiled when it comes: the scheduler reads one number per
    /// button — the bar it was armed at — and the bindings it already has open
    /// from there.
    ///
    /// That is the same move `wthen` makes, and it is made for the same
    /// reason. Lowering at press time would put a compile on a UI event and a
    /// program's code on a thread with a deadline; leaving the section
    /// un-lowered would mean the scheduler had to run the language. Writing
    /// every binding up front and gating them is what keeps the audio side
    /// knowing nothing about buttons except a bar.
    ///
    /// **Offset zero** because there is no bar yet at which the section
    /// begins. `Binding::start` is measured from the press for these, which is
    /// the one place in the language a start is counted from something other
    /// than the origin.
    pub fn trigger(&mut self, args: &[Arg], piped: Option<Value>) -> Result<Value, String> {
        if piped.is_some() {
            return Err(format!(
                "{}: the name is written at the call rather than chained into it — \
                 {}(\"fill\", fill)",
                controls::TRIGGER, controls::TRIGGER));
        }

        let Some((first, rest)) = args.split_first() else {
            return Err(format!(
                "{} expects a name and a section: {}(\"fill\", fill)",
                controls::TRIGGER, controls::TRIGGER));
        };
        let decl = controls::parse(controls::Kind::Trigger, std::slice::from_ref(first))?;
        if rest.len() != 1 {
            return Err(format!(
                "{}(\"{}\") expects one section to fire: {}(\"{}\", fill), where `fill` is \
                 a `fn` naming what to play",
                controls::TRIGGER, decl.name, controls::TRIGGER, decl.name));
        }

        let slot = match controls::shape_of(&decl.name) {
            Some((slot, ..)) => slot,
            None => controls::declare(&decl.name, decl.kind, decl.lo, decl.hi, decl.start)
                .unwrap_or(controls::NO_SLOT),
        };

        // Lowered the way every other section is, so a `fn` named here and the
        // same `fn` named after a `.then` mean the same thing by the same
        // mechanism — including what it is refused for.
        let placed = self.place_trigger(&decl.name, rest)?;

        // Marked after the fact rather than during, as `wthen` marks its arms
        // and for the same reason: a `wthen` *inside* a fired section keeps its
        // own group and picks this up as well, so a choice still rerolls once
        // the button has opened the window it rerolls in.
        for b in &mut self.bindings[placed..] {
            if b.button.is_none() {
                b.button = Some(slot);
            }
        }

        // Nothing to chain from. A trigger is a statement about the panel, the
        // way `midiclock` is a statement about a port: what it says is already
        // said by writing it, and there is nothing to do with an answer.
        Ok(Value::Number(0.0))
    }

    /// `slider("cutoff", 200, 5000, 800)` and `toggle("mute", 1)`.
    ///
    /// One function for both because the two differ only in what the panel
    /// draws: the node holds the **slot** and nothing else, which is the same
    /// trick `cc` plays with a port and for the same reason — a node reads its
    /// control on the audio callback and cannot hash a name, so the name
    /// becomes a small integer here and never appears in the graph again.
    /// Unlike `cc` it carries no range either, since what the slot holds is
    /// already in the control's own units.
    pub fn control(&mut self, name: &str, args: &[Arg], piped: Option<Value>)
        -> Result<Value, String>
    {
        // A piped receiver is refused rather than folded in with the
        // arguments, and there is nothing it could have been: the first
        // parameter is the name, the name is written in quotes, and a string
        // is never a value — so whatever arrived here came from somewhere the
        // name cannot come from.
        if piped.is_some() {
            return Err(format!(
                "{name}: the name is written at the call rather than chained into it — \
                 {name}(\"cutoff\")"));
        }

        let kind = if name == controls::TOGGLE { controls::Kind::Toggle } else { controls::Kind::Slider };
        let decl = controls::parse(kind, args)?;

        // What the panel already has for this name, which is what the program
        // declared first. Only a program nothing has walked — a test lowering
        // directly — falls through to declaring it here.
        let slot = match controls::shape_of(&decl.name) {
            Some((slot, ..)) => slot,
            // Every slot taken, when this answers `None`. The pass has already
            // said so in the problems panel, and what is left to decide here
            // is what the program *sounds* like: silence at this one control
            // and everything else unchanged. Refusing to compile would take a
            // whole piece down over a sixty-fifth control.
            None => controls::declare(&decl.name, decl.kind, decl.lo, decl.hi, decl.start)
                .unwrap_or(controls::NO_SLOT),
        };

        // The range written **here** rather than the one the panel draws: the
        // slot holds a fraction, and this is the place that says what it is
        // worth. Two call sites with two ranges are two readings of one
        // control — see `crate::controls`.
        let node = self.push_node(NodeKind::Control, vec![
            NodeInput::Const(slot as f64),
            NodeInput::Const(decl.lo),
            NodeInput::Const(decl.hi),
        ]);
        Ok(Value::Control { node, slot, at: controls::value_at(slot, decl.lo, decl.hi) })
    }
}

#[cfg(test)]
mod tests {
    use crate::controls::{self, exclusive, set};
    use crate::lowerer::lower::lower;
    use crate::parser::parser::parse;
    use crate::pattern::rate::Rate;
    use crate::swync_graph::graph::SwyncGraph;
    use crate::swync_graph::ugen_nodes::{NodeInput, NodeKind};

    fn lower_src(src: &str) -> Result<SwyncGraph, String> {
        let items = parse(src.to_string()).expect("parse failed");
        // The pass a real run makes before lowering, so these read the way the
        // app does rather than the way only a test would.
        controls::declare_in(&items);
        lower(&items).map(|l| l.graph)
    }

    fn err(src: &str) -> String {
        lower_src(src).expect_err("this program should not have compiled")
    }

    fn slots(g: &SwyncGraph) -> Vec<NodeInput> {
        g.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Control)
            .map(|n| n.inputs[0].clone())
            .collect()
    }

    /// The ordinary use: a node in the graph, read at audio rate, so a drag is
    /// heard without anything being compiled again.
    #[test]
    fn a_slider_in_a_signal_position_is_a_node_the_graph_reads() {
        let _controls = exclusive();
        let g = lower_src("lowpass(saw(110), slider(\"cutoff\", 200, 5000, 800), 1)\n").unwrap();

        assert_eq!(slots(&g), vec![NodeInput::Const(0.0)]);
        assert!(!controls::baked_by_name("cutoff"), "a signal reading is not a baked one");
    }

    /// The whole point of the node: arithmetic on a slider builds a graph
    /// rather than folding to the number it happens to be standing at.
    #[test]
    fn arithmetic_on_a_slider_stays_in_the_graph() {
        let _controls = exclusive();
        let g = lower_src("sin(220) * slider(\"level\") * 0.5\n").unwrap();

        assert_eq!(slots(&g), vec![NodeInput::Const(0.0)]);
        assert_eq!(g.nodes.iter().filter(|n| n.kind == NodeKind::Mul).count(), 2);
        assert!(!controls::baked_by_name("level"));
    }

    /// A math builtin folds during lowering, so there is no node for a
    /// changing value to be part of and the position it stands at is the only
    /// answer available. What is owed is saying so, which is the mark.
    #[test]
    fn a_slider_where_only_a_number_will_do_is_the_position_it_stands_at() {
        let _controls = exclusive();
        let g = lower_src("sin(slider(\"pitch\", 40, 80, 69).m2h)\n").unwrap();

        assert_eq!(g.nodes.iter().filter(|n| n.kind == NodeKind::Sin).count(), 1);
        assert!(controls::baked_by_name("pitch"), "a number reading should be marked");
    }

    /// A `match` chooses its arm while the program compiles, so a slider it is
    /// matched on is read as the position it was standing at — the same answer
    /// `if` gets, and marked as baked for the same reason. A plain signal has
    /// no such answer and is refused.
    #[test]
    fn a_slider_matched_on_is_the_position_it_was_standing_at() {
        let _controls = exclusive();
        let g = lower_src(
            "sin(match slider(\"pick\", 0, 100, 70) { 0..=50 => 220, _ => 440 })\n"
        ).unwrap();

        // By kind rather than by index: reading a slider as a number still
        // builds the node it would have been, and leaves it unreferenced.
        let sin = g.nodes.iter().find(|n| n.kind == NodeKind::Sin).expect("a sin");
        assert_eq!(sin.inputs[0], NodeInput::Const(440.0));
        assert!(controls::baked_by_name("pick"), "a matched reading should be marked");
    }

    /// Read as a number in one place and as a signal in another, it is still
    /// one slider — and it is marked, because a drag that is heard in one
    /// place and not the other is a drag the panel has to warn about.
    #[test]
    fn a_slider_read_both_ways_is_marked_as_the_baked_one_it_partly_is() {
        let _controls = exclusive();
        lower_src("sin(220) * slider(\"level\", 0, 1, 0.5) + sin(slider(\"level\").m2h)\n")
            .unwrap();
        assert!(controls::baked_by_name("level"));
    }

    /// One name is one slider wherever it is written, and the range the first
    /// one asked for is the range both of them get — which has to be true of
    /// the graph and not only of the warning in the problems panel.
    #[test]
    fn a_name_written_twice_lowers_to_one_slot() {
        let _controls = exclusive();
        let g = lower_src(
            "lowpass(saw(110), slider(\"cutoff\", 200, 5000), 1) * slider(\"cutoff\", 0, 1)\n")
            .unwrap();

        assert_eq!(slots(&g), vec![NodeInput::Const(0.0), NodeInput::Const(0.0)]);
        assert_eq!(
            controls::shape_of("cutoff").map(|(_, _, lo, hi, _)| (lo, hi)),
            Some((200.0, 5000.0)));
    }

    /// Two names are two controls, which is the thing that makes naming them
    /// worth anything.
    #[test]
    fn two_names_are_two_slots() {
        let _controls = exclusive();
        let g = lower_src("sin(220) * slider(\"a\") + sin(330) * slider(\"b\")\n").unwrap();
        assert_eq!(slots(&g), vec![NodeInput::Const(0.0), NodeInput::Const(1.0)]);
    }

    /// Where a session has already been dialled in, the number baked into a
    /// program is where the slider *is* rather than where the program says it
    /// starts — otherwise a re-eval would quietly undo the last five minutes.
    #[test]
    fn a_baked_reading_takes_the_position_rather_than_the_starting_point() {
        let _controls = exclusive();
        let src = "fn tone(n) = sin(n.m2h)\nplay([c4], tone, slider(\"speed\", 1, 4, 1))\n"
            .to_string();
        let items = parse(src).expect("parse failed");
        controls::declare_in(&items);
        set("speed", 3.0);

        let lowered = lower(&items).expect("lowering failed");
        // Within a float of 3: a position is held as the `f32` fraction of its
        // own range, so a value put in and read back has been through one
        // normalise and one map. That is the cost of one control reading
        // through several ranges, and it is a good deal smaller than anything
        // it could change about the sound.
        assert!(matches!(lowered.bindings[0].rate, Rate::Fixed(r) if (r - 3.0).abs() < 1e-6),
                "got: {:?}", lowered.bindings[0].rate);
    }

    /// A slider is a signal wherever a signal goes, and the top of a file is
    /// one of those places. Matching `Value::Signal` here used to drop it, and
    /// what that sounded like was a line that did nothing at all.
    #[test]
    fn a_slider_written_as_the_whole_of_a_line_reaches_the_output() {
        let _controls = exclusive();
        let g = lower_src("slider(\"level\")\n").unwrap();
        assert_eq!(g.output.map(|id| g.nodes[id.0].kind), Some(NodeKind::Control));
    }

    /// The same rule one level down: any signal in a loop makes the loop
    /// audio, and a slider is one.
    #[test]
    fn a_loop_answering_with_sliders_is_audio_like_any_other_signal() {
        let _controls = exclusive();
        let g = lower_src("for i in 0..=1 { sin(220) * slider(\"a\") }\n").unwrap();
        // A node per pass, as every inlined body gets — and both reading the
        // one slot, because the name is what a slider is.
        assert_eq!(slots(&g), vec![NodeInput::Const(0.0), NodeInput::Const(0.0)]);
        assert!(g.output.is_some(), "the loop should have summed into the output");
    }

    /// An enum member is read once, where the enum is written, so a control
    /// named there would stop moving anything.
    #[test]
    fn an_enum_member_cannot_be_a_slider() {
        let _controls = exclusive();
        let message = err("enum Filters { low = slider(\"low\", 100, 400) }\nsin(220)\n");
        assert!(message.contains("is a slider"), "got: {message}");
    }

    /// A toggle is a signal like any other, which is what makes multiplying
    /// by it the ordinary use — a part in or out with no number to choose.
    #[test]
    fn a_toggle_is_a_node_the_graph_reads_like_a_slider() {
        let _controls = exclusive();
        let g = lower_src("sin(220) * toggle(\"mute\")\n").unwrap();

        assert_eq!(slots(&g), vec![NodeInput::Const(0.0)]);
        assert!(!controls::baked_by_name("mute"));
    }

    #[test]
    fn a_toggle_starts_off_unless_the_program_says_otherwise() {
        let _controls = exclusive();
        lower_src("sin(220) * toggle(\"mute\")\n").unwrap();
        assert_eq!(controls::value_at(0, 0.0, 1.0), 0.0);

        let _ = lower_src("sin(220) * toggle(\"lead\", 1)\n");
        let (slot, ..) = controls::shape_of("lead").expect("declared");
        assert_eq!(controls::value_at(slot, 0.0, 1.0), 1.0);
    }

    /// Two ends and nothing in between, so a number that is neither is a
    /// number somebody expected to mean something.
    #[test]
    fn a_toggle_that_starts_part_way_is_refused() {
        let _controls = exclusive();
        let message = err("sin(220) * toggle(\"mute\", 0.5)\n");
        assert!(message.contains("off or on"), "got: {message}");
    }

    #[test]
    fn a_toggle_has_no_range_to_give_it() {
        let _controls = exclusive();
        let message = err("sin(220) * toggle(\"mute\", 0, 1)\n");
        assert!(message.contains("no range"), "got: {message}");
    }

    /// One name is one control and a slot has one travel, so the two kinds
    /// collide exactly as two ranges do.
    #[test]
    fn one_name_cannot_be_both_a_slider_and_a_toggle() {
        let _controls = exclusive();
        let items = parse("sin(220) * toggle(\"level\") * slider(\"level\")\n".to_string())
            .expect("parse failed");
        let (found, warnings) = controls::declare_in(&items);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, controls::Kind::Toggle);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("also as a"), "got: {}", warnings[0]);
    }

    // ---- trigger ----

    fn bindings(src: &str) -> Vec<crate::pattern::patterns::Binding> {
        let items = parse(src.to_string()).expect("parse failed");
        controls::declare_in(&items);
        lower(&items).expect("lowering failed").bindings
    }

    /// The section is lowered when the program runs, not when the button is
    /// pressed — so a press compiles nothing and can fail at nothing.
    #[test]
    fn a_triggered_section_is_lowered_at_once_and_marked_for_its_button() {
        let _controls = exclusive();
        let bs = bindings(
            "fn inst(n) = sin(n)\nfn fill() = play_once([60, 63], inst)\n\
             trigger(\"fill\", fill)\n");

        assert_eq!(bs.len(), 1, "the section's notes are written now");
        let (slot, ..) = controls::shape_of("fill").expect("declared");
        assert_eq!(bs[0].button, Some(slot));
    }

    /// Offset zero, because there is no bar yet at which it begins — the bar
    /// arrives when somebody hits the button.
    #[test]
    fn a_triggered_section_starts_at_zero_rather_than_at_the_origin() {
        let _controls = exclusive();
        let bs = bindings(
            "fn inst(n) = sin(n)\nfn fill() = play_once([60], inst)\n\
             trigger(\"fill\", fill)\n");
        assert_eq!(bs[0].start, 0.0);
    }

    /// A trigger is a statement about the panel, like `midiclock` is one about
    /// a port, so what it fires is not also playing on its own.
    #[test]
    fn a_section_on_a_button_does_not_also_play_where_it_is_written() {
        let _controls = exclusive();
        let bs = bindings(
            "fn inst(n) = sin(n)\nfn fill() = play_once([60], inst)\n\
             play([48], inst)\ntrigger(\"fill\", fill)\n");

        let loose: Vec<_> = bs.iter().filter(|b| b.button.is_none()).collect();
        assert_eq!(loose.len(), 1, "only the plain play is unbuttoned");
        assert_eq!(loose[0].target, crate::pattern::patterns::Target::from("inst"));
    }

    /// A `wthen` inside a fired section keeps its own group and picks the
    /// button up as well, so a choice still rerolls inside the window a press
    /// opens — the same way a nested `wthen` keeps its group inside an arm.
    #[test]
    fn a_choice_inside_a_fired_section_keeps_its_own_group() {
        let _controls = exclusive();
        let bs = bindings(
            "fn inst(n) = sin(n)\n\
             fn a() = play_once([60], inst)\n\
             fn b() = play_once([63], inst)\n\
             fn fill() = play_once([48], inst).wthen([a, b], [1, 1])\n\
             trigger(\"fill\", fill)\n");

        let (slot, ..) = controls::shape_of("fill").expect("declared");
        assert!(bs.iter().all(|b| b.button == Some(slot)), "all of it is on the button");
        assert!(bs.iter().any(|b| b.choice.is_some()), "and the choice survived");
    }

    #[test]
    fn a_trigger_needs_a_section_to_fire() {
        let _controls = exclusive();
        let message = err("trigger(\"fill\")\n");
        assert!(message.contains("one section"), "got: {message}");
    }

    /// The same refusal `.then` makes, in the same words' shape: a play bound
    /// to a name already sounds where it was written.
    #[test]
    fn a_trigger_refuses_a_play_that_has_already_been_placed() {
        let _controls = exclusive();
        let message = err(
            "fn inst(n) = sin(n)\nlet a = play_once([60], inst)\ntrigger(\"fill\", a)\n");
        assert!(message.contains("already been placed"), "got: {message}");
    }

    #[test]
    fn a_slider_needs_a_name_written_in_quotes() {
        let _controls = exclusive();
        let message = err("let name = 1\nsin(220) * slider(name)\n");
        assert!(message.contains("written in quotes"), "got: {message}");
    }

    /// The panel is drawn before the program runs, so a range it would have to
    /// run the program to know is one it cannot draw.
    #[test]
    fn a_range_that_has_to_be_worked_out_is_refused() {
        let _controls = exclusive();
        let message = err("let top = 5000\nsin(slider(\"cutoff\", 200, top))\n");
        assert!(message.contains("written out as a number"), "got: {message}");
    }

    #[test]
    fn a_slider_cannot_be_chained_into() {
        let _controls = exclusive();
        let message = err("sin(220 >> slider(200, 5000))\n");
        assert!(message.contains("written at the call"), "got: {message}");
    }
}
