//! Imports, mostly against a filesystem made of a map.
//!
//! Every test here is a little project: one file being run, and the files it
//! reaches for. The checks are on the program that comes out — a flat list of
//! definitions with no `use` left in it — and, where it matters, on what that
//! program lowers to, since a name that resolves to nothing is only visible
//! once something tries to play it.
//!
//! The tests about the drawn patterns use a real folder rather than the map,
//! because that is their claim: a project's patterns are a file in it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::*;
use crate::lowerer::lower::lower;
use crate::parser::parser::parse;
use crate::scheduler::voice::Instruments;
use crate::swync_graph::ugen_nodes::NodeInput;

/// The folder the program being run is in.
const DIR: &str = "/p";
/// The program being run.
const ENTRY: &str = "/p/song.swync";

struct Files(HashMap<PathBuf, String>);

impl Sources for Files {
    fn read(&self, path: &Path) -> Option<String> {
        self.0.get(path).cloned()
    }
}

fn files(files: &[(&str, &str)]) -> Files {
    Files(
        files
            .iter()
            .map(|(path, text)| (PathBuf::from(path), (*text).to_string()))
            .collect(),
    )
}

/// Expand `entry` against `modules`, as if it were being run from `/p/song.swync`.
fn expand_ok(entry: &str, modules: &[(&str, &str)]) -> Vec<SwyncItem> {
    match try_expand(entry, modules) {
        Ok(items) => items,
        Err(e) => panic!("expected {entry:?} to expand, got: {e}"),
    }
}

fn try_expand(entry: &str, modules: &[(&str, &str)]) -> Result<Vec<SwyncItem>, Diagnostic> {
    try_expand_from(entry, modules, &[])
}

/// The same, with somewhere else to look once the project has nothing: the
/// library stores, nearest first.
fn try_expand_from(
    entry: &str,
    modules: &[(&str, &str)],
    libraries: &[&str],
) -> Result<Vec<SwyncItem>, Diagnostic> {
    let items = parse(entry.to_string()).expect("the entry file should parse");
    let libraries: Vec<PathBuf> = libraries.iter().map(PathBuf::from).collect();
    expand_from(
        items,
        entry,
        &files(modules),
        Path::new(DIR),
        Some(Path::new(ENTRY)),
        // The implicit import has its own tests below; these are about the
        // `use`s a file writes for itself.
        None,
        &libraries,
    )
}

fn expand_err(entry: &str, modules: &[(&str, &str)]) -> Diagnostic {
    expand_err_with(entry, modules, &[])
}

fn expand_err_with(entry: &str, modules: &[(&str, &str)], libraries: &[&str]) -> Diagnostic {
    match try_expand_from(entry, modules, libraries) {
        Err(e) => e,
        Ok(_) => panic!("expected {entry:?} not to expand"),
    }
}

/// Every name the expanded program defines.
fn defined(items: &[SwyncItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            SwyncItem::Function { name, .. } | SwyncItem::Let { name, .. } => Some(name.0.clone()),
            _ => None,
        })
        .collect()
}

/// The constant a program's single `sin` was handed — the codebase's usual way
/// of making a folded number observable.
fn constant(entry: &str, modules: &[(&str, &str)]) -> f64 {
    constant_of(&expand_ok(entry, modules))
}

fn constant_of(items: &[SwyncItem]) -> f64 {
    let graph = lower(&items.to_vec()).expect("should lower").graph;
    match graph.nodes[0].inputs[0] {
        NodeInput::Const(v) => v,
        ref other => panic!("expected a folded constant, got {other:?}"),
    }
}

const DRUMS: &str = "\
fn kick(f) = sin(f) * perc(0.001, 0.25)
fn snare(f) = noise() * perc(0.001, 0.1)
";

// ---- what a `use` brings in ----

/// The plain case: a whole module, reached through its own name.
#[test]
fn a_module_is_reached_through_its_name() {
    let items = expand_ok(
        "use drums\nplay([50], drums::kick)\n",
        &[("/p/drums.swync", DRUMS)],
    );

    assert!(defined(&items).contains(&"drums::kick".to_string()));

    let lowered = lower(&items).expect("should lower");
    assert_eq!(lowered.bindings.len(), 1);
    assert_eq!(lowered.bindings[0].target, "drums::kick");
    // And the scheduler can build the voice that binding names.
    assert!(Instruments::from_program(&items).has("drums::kick"));
}

/// `use drums::kick` — one name, unqualified.
#[test]
fn a_single_name_arrives_unqualified() {
    let items = expand_ok(
        "use drums::kick\nplay([50], kick)\n",
        &[("/p/drums.swync", DRUMS)],
    );

    let lowered = lower(&items).expect("should lower");
    assert_eq!(lowered.bindings[0].target, "drums::kick");
}

/// The rest of the module stays where it is: importing one name is not
/// importing the file.
#[test]
fn a_single_name_does_not_bring_its_neighbours() {
    let items = expand_ok(
        "use drums::kick\nplay([50], snare)\n",
        &[("/p/drums.swync", DRUMS)],
    );

    // Expansion leaves an unimported name alone rather than refusing it: it
    // could still be a builtin or a drawn pattern. What it is not is the
    // module's `snare`, and nothing in the program defines it.
    assert!(!defined(&items).contains(&"snare".to_string()));
    let err = match lower(&items) {
        Err(e) => e,
        Ok(_) => panic!("`snare` should be unbound"),
    };
    assert!(err.contains("snare"), "got: {err}");
}

#[test]
fn a_braced_list_takes_several_names() {
    let items = expand_ok(
        "use drums::{kick, snare as clap}\nplay([50], kick)\nplay([60], clap)\n",
        &[("/p/drums.swync", DRUMS)],
    );

    let lowered = lower(&items).expect("should lower");
    let played: Vec<String> = lowered
        .bindings
        .iter()
        .map(|b| b.target.label())
        .collect();
    assert_eq!(played, vec!["drums::kick", "drums::snare"]);
}

#[test]
fn a_glob_takes_everything() {
    let items = expand_ok(
        "use drums::*\nplay([50], kick)\nplay([60], snare)\n",
        &[("/p/drums.swync", DRUMS)],
    );
    assert_eq!(lower(&items).expect("should lower").bindings.len(), 2);
}

#[test]
fn a_module_can_be_renamed() {
    let items = expand_ok(
        "use drums as d\nplay([50], d::kick)\n",
        &[("/p/drums.swync", DRUMS)],
    );
    assert_eq!(
        lower(&items).expect("should lower").bindings[0].target,
        "drums::kick"
    );
}

/// A folder deep: `use lib::drums` is `lib/drums.swync`, and the module's name
/// is the path it was written as.
#[test]
fn a_path_walks_into_folders() {
    let items = expand_ok(
        "use lib::drums\nplay([50], drums::kick)\n",
        &[("/p/lib/drums.swync", DRUMS)],
    );
    assert!(defined(&items).contains(&"lib::drums::kick".to_string()));
    assert_eq!(
        lower(&items).expect("should lower").bindings[0].target,
        "lib::drums::kick"
    );
}

/// The ambiguous shape: `use a::b::c` is a module if there is such a file, and
/// an item of `a::b` if there is not.
#[test]
fn a_last_segment_is_a_file_before_it_is_a_name() {
    let module = expand_ok(
        "use lib::drums\nplay([50], drums::kick)\n",
        &[
            ("/p/lib/drums.swync", DRUMS),
            ("/p/lib.swync", "fn drums(f) = sin(f)\n"),
        ],
    );
    assert_eq!(
        lower(&module).expect("should lower").bindings[0].target,
        "lib::drums::kick"
    );

    let item = expand_ok(
        "use lib::drums\nplay([50], drums)\n",
        &[("/p/lib.swync", "fn drums(f) = sin(f)\n")],
    );
    assert_eq!(
        lower(&item).expect("should lower").bindings[0].target,
        "lib::drums"
    );
}

// ---- what a `use` does not bring in ----

/// A module is a library. Whatever it plays when it is run is its own business.
#[test]
fn a_module_does_not_play_itself() {
    let items = expand_ok(
        "use drums::kick\nplay([50], kick)\n",
        &[(
            "/p/drums.swync",
            "fn kick(f) = sin(f) * perc(0.001, 0.25)\nplay([40, 40], kick)\nsin(220)\n",
        )],
    );

    let lowered = lower(&items).expect("should lower");
    assert_eq!(lowered.bindings.len(), 1, "only the importer's own play");
    // The module's `sin(220)` is not in the output either.
    assert!(lowered.graph.output.is_none());
}

/// Two modules may both have a `helper`, and neither may see the other's.
#[test]
fn private_names_do_not_collide() {
    let items = expand_ok(
        "use a::*\nsin(one())\n",
        &[
            ("/p/a.swync", "fn helper() = 1\nfn one() = helper()\n"),
            ("/p/b.swync", "fn helper() = 2\nfn two() = helper()\n"),
        ],
    );
    assert_eq!(
        constant("use a::*\nuse b::*\nsin(one())\n", &[
            ("/p/a.swync", "fn helper() = 1\nfn one() = helper()\n"),
            ("/p/b.swync", "fn helper() = 2\nfn two() = helper()\n"),
        ]),
        1.0,
        "a's `one` must still reach a's `helper`",
    );
    assert!(defined(&items).contains(&"a::helper".to_string()));
}

/// A file's own definition wins over one it imported. A `use` adds a name; it
/// never takes one away.
#[test]
fn a_local_definition_beats_an_imported_one() {
    assert_eq!(
        constant("use m::*\nfn v() = 2\nsin(v())\n", &[("/p/m.swync", "fn v() = 1\n")]),
        2.0
    );
}

/// A parameter, a `let`, a `for` — none of them is an import, however it is
/// spelled.
#[test]
fn locals_shadow_imports() {
    let m = &[("/p/m.swync", "fn v() = 1\nlet n = 1\n")];

    // A parameter named like an imported value.
    assert_eq!(constant("use m::*\nfn f(n) = n\nsin(f(9))\n", m), 9.0);
    // A `let` in a block.
    assert_eq!(
        constant("use m::*\nfn f() {\n  let n = 7\n  n\n}\nsin(f())\n", m),
        7.0
    );
    // A `for` variable.
    assert_eq!(
        constant("use m::*\nlet xs = for n in 5..=5 { n }\nsin(xs[0])\n", m),
        5.0
    );
    // And with nothing shadowing it, the import is what `n` means.
    assert_eq!(constant("use m::*\nsin(n)\n", m), 1.0);
}

// ---- modules that import modules ----

#[test]
fn imports_are_transitive() {
    let items = expand_ok(
        "use leads::*\nplay([50], lead)\n",
        &[
            ("/p/leads.swync", "use tones::*\nfn lead(f) = tone(f) * 0.5\n"),
            ("/p/tones.swync", "fn tone(f) = sin(f)\n"),
        ],
    );

    // The module a module needed is defined before the module that needs it.
    let names = defined(&items);
    let tone = names.iter().position(|n| n == "tones::tone").expect("tone");
    let lead = names.iter().position(|n| n == "leads::lead").expect("lead");
    assert!(tone < lead, "a module must be defined before its importer: {names:?}");

    assert!(lower(&items).is_ok());
}

/// One file, however many things import it: it is read once and its
/// definitions appear once.
#[test]
fn a_shared_module_is_only_expanded_once() {
    let items = expand_ok(
        "use a::*\nuse b::*\nsin(one() + two())\n",
        &[
            ("/p/a.swync", "use base::*\nfn one() = v()\n"),
            ("/p/b.swync", "use base::*\nfn two() = v()\n"),
            ("/p/base.swync", "fn v() = 1\n"),
        ],
    );

    let defined = defined(&items);
    assert_eq!(
        defined.iter().filter(|n| *n == "base::v").count(),
        1,
        "{defined:?}"
    );
}

// ---- refusals ----

#[test]
fn a_missing_module_says_where_it_looked() {
    let err = expand_err("use drums\nsin(220)\n", &[]);
    assert_eq!(err.stage, Stage::Import);
    assert!(
        err.message.contains("/p/drums.swync"),
        "should name the file it wanted: {}",
        err.message
    );
    // And point at the `use` that asked for it.
    assert_eq!(err.line, Some(1));
    assert_eq!(err.snippet.as_deref(), Some("use drums"));
}

#[test]
fn a_missing_name_says_so() {
    let err = expand_err("use drums::kik\nsin(220)\n", &[("/p/drums.swync", DRUMS)]);
    assert!(
        err.message.contains("has no `kik`") && err.message.contains("did you mean `kick`"),
        "got: {}",
        err.message
    );
    assert_eq!(err.line, Some(1));
}

#[test]
fn a_cycle_is_reported_as_the_route_that_closed_it() {
    let err = expand_err(
        "use a::*\nsin(220)\n",
        &[
            ("/p/a.swync", "use b::*\nfn v() = w()\n"),
            ("/p/b.swync", "use a::*\nfn w() = v()\n"),
        ],
    );
    assert!(
        err.message.contains("circular import"),
        "got: {}",
        err.message
    );
    assert!(err.message.contains("a.swync"), "got: {}", err.message);
}

/// A module that imports the file being run closes a cycle too, even though
/// that file is only on disk as far as it can tell.
#[test]
fn importing_the_running_file_back_is_a_cycle() {
    let err = expand_err(
        "use m::*\nsin(220)\n",
        &[
            ("/p/m.swync", "use song::*\nfn v() = 1\n"),
            // On disk it is an ordinary file, and only the loading chain knows
            // it is the one being run.
            (ENTRY, "use m::*\nsin(220)\n"),
        ],
    );
    assert!(err.message.contains("circular import"), "got: {}", err.message);
}

/// A syntax error in a module is reported where it is, in the file it is in —
/// not as a failure of the file that imported it.
#[test]
fn a_broken_module_reports_its_own_position() {
    let err = expand_err(
        "use m::*\nsin(220)\n",
        &[("/p/m.swync", "fn v() = 1\nfn f(a b) = a\n")],
    );

    assert_eq!(err.stage, Stage::Parse);
    assert_eq!(err.line, Some(2));
    assert_eq!(err.file.as_deref(), Some("/p/m.swync"));
}

/// A qualified name whose module was never imported: the mistake is the
/// missing `use`, so that is what it says.
#[test]
fn an_unimported_qualifier_is_refused() {
    let err = expand_err("play([50], drums::kick)\nuse m::*\n", &[("/p/m.swync", "fn v() = 1\n")]);
    assert!(
        err.message.contains("no module named `drums`"),
        "got: {}",
        err.message
    );
}

/// A program with no imports never asks where it is, so a buffer that has never
/// been saved still runs.
#[test]
fn a_program_without_imports_needs_no_folder() {
    let items = parse("sin(220)\n".to_string()).expect("should parse");
    let expanded = expand(items.clone(), "sin(220)\n", &Workspace::default())
        .expect("should expand without a workspace");
    assert_eq!(expanded, items);
}

/// One with imports does, and says so rather than failing to find anything.
#[test]
fn an_unsaved_program_with_imports_says_what_is_missing() {
    let src = "use drums\nsin(220)\n";
    let err = expand(parse(src.to_string()).unwrap(), src, &Workspace::default())
        .expect_err("nowhere to look");
    assert!(err.message.contains("never been saved"), "got: {}", err.message);
}

/// The shape the editor sends, read as the shape this side expects. The two
/// are written in different languages and only ever meet over IPC, so the
/// contract between them is worth one test.
#[test]
fn the_editors_payload_deserializes() {
    let sent = serde_json::json!({ "path": "/p/song.swync", "root": "/p" });
    let ws: Workspace = serde_json::from_value(sent).expect("should deserialize");
    assert_eq!(ws.dir(), Some(PathBuf::from("/p")));
    assert_eq!(ws.patterns_path(), PathBuf::from("/p/patterns.swync"));

    // A tab that has never been saved sends a null path, and the project
    // folder is what is left to resolve against.
    let unsaved = serde_json::json!({ "path": null, "root": "/p" });
    let ws: Workspace = serde_json::from_value(unsaved).expect("should deserialize");
    assert_eq!(ws.dir(), Some(PathBuf::from("/p")));
}

// ---- the import nobody writes: the project's drawn patterns ----

/// A real folder with real files in it, thrown away when the test ends —
/// however it ends, which is why it is a guard rather than a line at the
/// bottom of each test.
///
/// These go through the filesystem rather than a map of files because that is
/// the claim being tested: a project's patterns are a file in it.
struct Project {
    dir: PathBuf,
}

impl Project {
    fn new(name: &str, files: &[(&str, &str)]) -> Project {
        let dir = temp_dir(name);
        for (relative, content) in files {
            let path = dir.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("should create a folder");
            }
            std::fs::write(&path, content).expect("should write a file");
        }
        Project { dir }
    }

    /// What the editor sends when it runs one of the project's files.
    fn running(&self, relative: &str) -> Workspace {
        Workspace::new(
            Some(self.dir.join(relative).display().to_string()),
            Some(self.dir.display().to_string()),
        )
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn expand_in(src: &str, ws: &Workspace) -> Vec<SwyncItem> {
    let items = parse(src.to_string()).expect("should parse");
    expand(items, src, ws).unwrap_or_else(|e| panic!("should expand: {e}"))
}

const PATTERNS: &str = "let hats = [\\, `, \\, `]\n";
/// Enough of a program to hear a pattern with.
const PLAYS_HATS: &str = "fn hat() = noise()\nplay(hats, hat)\n";

/// The point of the whole thing: a drawn pattern is nameable in a file that
/// says nothing about it.
#[test]
fn the_projects_patterns_need_no_use() {
    let project = Project::new("needs-no-use", &[("patterns.swync", PATTERNS)]);
    let items = expand_in(PLAYS_HATS, &project.running("song.swync"));

    let lowered = lower(&items).expect("should lower");
    assert_eq!(lowered.bindings[0].source.pattern().cloned().expect("a written pattern").values().len(), 4);
}

/// And is reachable qualified, which is what makes it possible to say which
/// `hats` you meant.
#[test]
fn a_drawn_pattern_can_be_qualified() {
    let project = Project::new("qualified", &[("patterns.swync", PATTERNS)]);
    let items = expand_in(
        "fn hat() = noise()\nplay(patterns::hats, hat)\n",
        &project.running("song.swync"),
    );
    assert!(lower(&items).is_ok());
}

/// It is an import like any other, so the file's own definition beats it.
#[test]
fn a_file_may_shadow_a_drawn_pattern() {
    let project = Project::new("shadow", &[("patterns.swync", PATTERNS)]);
    let items = expand_in(
        "fn hat() = noise()\nlet hats = [\\, \\]\nplay(hats, hat)\n",
        &project.running("song.swync"),
    );

    let lowered = lower(&items).expect("should lower");
    assert_eq!(
        lowered.bindings[0].source.pattern().cloned().expect("a written pattern").values().len(),
        2,
        "the file's own two-step pattern, not the panel's four"
    );
}

/// The patterns live in the project folder, so a file in a subfolder gets the
/// same ones rather than none.
#[test]
fn patterns_reach_the_whole_project() {
    let project = Project::new("subfolder", &[("patterns.swync", PATTERNS)]);
    let items = expand_in(PLAYS_HATS, &project.running("parts/verse.swync"));
    assert!(lower(&items).is_ok());
}

/// A module is a file somebody else's project might import, so it is given
/// nothing this panel happens to hold.
#[test]
fn a_module_does_not_get_the_patterns() {
    let project = Project::new(
        "module-patterns",
        &[
            ("patterns.swync", PATTERNS),
            ("m.swync", "fn beat() = hats\n"),
        ],
    );
    // Called, not merely defined: a function body is only resolved when it is
    // inlined, so an uncalled one would prove nothing.
    let items = expand_in(
        "use m::*\nplay(beat(), hat)\nfn hat() = noise()\n",
        &project.running("song.swync"),
    );

    // The module's `hats` is unresolved rather than the panel's, and says so
    // when something asks for it.
    let err = match lower(&items) {
        Err(e) => e,
        Ok(_) => panic!("the module should not see the panel"),
    };
    assert!(err.contains("hats"), "got: {err}");
}

/// The patterns file itself is a file you can open and run, and running it
/// must not import it into itself.
#[test]
fn running_the_patterns_file_is_not_a_cycle() {
    let src = format!("{PATTERNS}fn hat() = noise()\nplay(hats, hat)\n");
    let project = Project::new("self", &[("patterns.swync", &src)]);
    let items = parse(src.clone()).expect("should parse");
    assert!(expand(items, &src, &project.running("patterns.swync")).is_ok());
}

/// A project with nothing drawn in it is just a project.
#[test]
fn no_patterns_file_changes_nothing() {
    let project = Project::new("no-patterns", &[]);
    let src = "sin(220)\n";
    let items = parse(src.to_string()).expect("should parse");
    assert_eq!(
        expand(items.clone(), src, &project.running("song.swync")).expect("should expand"),
        items
    );
}

/// The file on disk is what plays when the panel says nothing — which is what
/// a project that was closed and reopened relies on.
#[test]
fn the_patterns_file_plays_on_its_own() {
    let project = Project::new("on-disk", &[("patterns.swync", PATTERNS)]);
    let items = expand_in(PLAYS_HATS, &project.running("song.swync"));

    assert_eq!(
        lower(&items).expect("should lower").bindings[0].source.pattern().cloned().expect("a written pattern").values().len(),
        4
    );
}

/// And the panel's rows are that file when it does, whatever is on disk —
/// which is what keeps playing possible while a write is pending, or
/// impossible.
#[test]
fn the_panels_rows_are_the_patterns_file() {
    let project = Project::new("panel-wins", &[("patterns.swync", "let hats = [\\, \\, \\]\n")]);
    let mut ws = project.running("song.swync");
    ws.set_patterns(PATTERNS.to_string());

    let items = expand_in(PLAYS_HATS, &ws);
    assert_eq!(
        lower(&items).expect("should lower").bindings[0].source.pattern().cloned().expect("a written pattern").values().len(),
        4,
        "the panel's four steps, not the three last written"
    );
}

/// An empty panel is an empty panel, not an absent one: clearing the last row
/// silences it now rather than whenever the write happens to land.
#[test]
fn an_emptied_panel_empties_the_patterns() {
    let project = Project::new("emptied", &[("patterns.swync", PATTERNS)]);
    let mut ws = project.running("song.swync");
    ws.set_patterns(crate::pattern::graphical::to_source(&[]));

    let items = expand_in(PLAYS_HATS, &ws);
    let err = match lower(&items) {
        Err(e) => e,
        Ok(_) => panic!("the row was deleted; nothing should be named `hats`"),
    };
    assert!(err.contains("hats"), "got: {err}");
}

// ---- what the editor may and may not stand in front of ----

/// The panel's rows are the one thing an eval does not take from disk, and
/// they are only ever that one file.
#[test]
fn only_the_patterns_file_comes_from_the_editor() {
    let project = Project::new(
        "disk-only",
        &[("patterns.swync", "let hats = [\\]\n"), ("m.swync", "fn v() = 1\n")],
    );
    let mut ws = project.running("song.swync");
    ws.set_patterns(PATTERNS.to_string());

    assert_eq!(ws.read(&ws.patterns_path()).as_deref(), Some(PATTERNS));
    assert_eq!(
        ws.read(&project.dir.join("m.swync")).as_deref(),
        Some("fn v() = 1\n"),
        "every other file is read from the disk",
    );
}

/// A module is whatever the file says, so an edit reaches the program when it
/// is saved and not before. Nothing the editor sends can change that: there is
/// nowhere in an eval to put an unsaved buffer.
#[test]
fn a_module_is_read_from_the_disk_every_time() {
    let project = Project::new("resaved", &[("m.swync", "fn v() = 1\n")]);
    let ws = project.running("song.swync");
    let src = "use m::*\nsin(v())\n";

    let first = expand_in(src, &ws);
    assert_eq!(constant_of(&first), 1.0);

    std::fs::write(project.dir.join("m.swync"), "fn v() = 2\n").expect("should rewrite");

    let second = expand_in(src, &ws);
    assert_eq!(constant_of(&second), 2.0, "the saved file, read again");
}

/// A folder of this test's own, for the few tests that need a real one.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "swync-imports-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("should create a temp folder");
    dir
}

/// The one thing a map of files cannot check: that a real folder of real files
/// resolves the same way, through the paths the editor actually sends.
#[test]
fn modules_are_read_from_the_disk() {
    let dir = temp_dir("modules");
    std::fs::create_dir_all(dir.join("lib")).expect("should create a temp folder");
    std::fs::write(dir.join("lib").join("drums.swync"), DRUMS).expect("should write");

    let song = dir.join("song.swync");
    let src = "use lib::drums\nplay([50], drums::kick)\n";
    std::fs::write(&song, src).expect("should write");

    let ws = Workspace::new(Some(song.display().to_string()), None);
    let items = expand(parse(src.to_string()).unwrap(), src, &ws).expect("should expand");

    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        lower(&items).expect("should lower").bindings[0].target,
        "lib::drums::kick"
    );
}

/// The shipped example, compiled where it sits. It is the documentation for
/// this whole feature, so it has to be a program that runs.
#[test]
fn the_example_project_compiles() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate is inside the project")
        .join("examples");

    let song = examples.join("imports.swync");
    let src = std::fs::read_to_string(&song).expect("examples/imports.swync should exist");

    let ws = Workspace::new(Some(song.display().to_string()), None);
    let items = expand(parse(src.clone()).expect("should parse"), &src, &ws)
        .unwrap_or_else(|e| panic!("examples/imports.swync should expand: {e}"));

    let lowered = lower(&items).unwrap_or_else(|e| panic!("should lower: {e}"));
    assert_eq!(lowered.bindings.len(), 4);

    // The kit's own `play`s stayed in the kit: every binding here is one this
    // file wrote.
    let instruments = Instruments::from_program(&items);
    for binding in &lowered.bindings {
        assert!(
            binding.target.instrument().is_some_and(|i| instruments.has(i)),
            "no instrument for {}",
            binding.target
        );
    }
}

// ---- the shape of the expanded program ----

/// Nothing downstream should ever have to know imports exist.
#[test]
fn no_use_survives_expansion() {
    let items = expand_ok(
        "use drums::*\nplay([50], kick)\n",
        &[("/p/drums.swync", DRUMS)],
    );
    assert!(!items.iter().any(|i| matches!(i, SwyncItem::Use(_))));
}

/// A module's `let`s travel with its functions, and still reach each other.
#[test]
fn a_module_may_export_values() {
    assert_eq!(
        constant(
            "use m::*\nsin(hz)\n",
            &[("/p/m.swync", "let base = 55\nlet hz = base * 2\n")]
        ),
        110.0
    );
}

/// The qualified spelling of an imported name is a name like any other, so it
/// works everywhere one does — including as a method.
#[test]
fn a_qualified_name_chains_like_any_other() {
    assert_eq!(
        constant(
            "use m\nsin(3.m::double())\n",
            &[("/p/m.swync", "fn double(x) = x * 2\n")]
        ),
        6.0
    );
}

/// A name the resolver made up is never one a writer could have written, so a
/// module and a local definition cannot be confused for each other.
#[test]
fn module_names_are_paths() {
    let items = expand_ok("use lib::drums\n", &[("/p/lib/drums.swync", DRUMS)]);
    assert_eq!(
        defined(&items),
        vec!["lib::drums::kick", "lib::drums::snare"]
    );
}


// ---- the paths a module writes ----

/// Every file the expanded program will go looking for.
fn sample_paths(entry: &str, modules: &[(&str, &str)]) -> Vec<String> {
    crate::samples::paths_in(&expand_ok(entry, modules))
}

/// A module's `load` is relative to the module, not to whatever imported it.
///
/// The definitions move; the folder they were written in does not, and the
/// paths are read long after the move — so a path left as written would name a
/// file beside the *program*, which is a file nobody put there.
#[test]
fn a_modules_sample_path_is_relative_to_the_module() {
    assert_eq!(
        sample_paths(
            "use lib::kit\nplay([50], kit::hit)\n",
            &[(
                "/p/lib/kit.swync",
                "fn hit(f) = sample(load(\"samples/kick.wav\"), ramp())\n"
            )]
        ),
        vec!["/p/lib/samples/kick.wav"]
    );
}

/// The same, for a path written outside a `fn` — a module's `let`s travel too.
#[test]
fn a_modules_sample_path_travels_from_a_let() {
    assert_eq!(
        sample_paths(
            "use lib::kit::*\nsample(buf, ramp())\n",
            &[("/p/lib/kit.swync", "let buf = load(\"break.wav\")\n")]
        ),
        vec!["/p/lib/break.wav"]
    );
}

/// An absolute path already answers the question, and `samples::resolve` takes
/// it as written — so nothing here may join a folder onto it.
#[test]
fn an_absolute_sample_path_is_left_alone() {
    assert_eq!(
        sample_paths(
            "use kit\nplay([50], kit::hit)\n",
            &[(
                "/p/kit.swync",
                "fn hit(f) = sample(load(\"/Sounds/909/kick.wav\"), ramp())\n"
            )]
        ),
        vec!["/Sounds/909/kick.wav"]
    );
}

/// The program's own paths are not rewritten: nothing moved them, and they are
/// the text its errors quote back.
#[test]
fn the_programs_own_sample_path_stays_as_written() {
    assert_eq!(
        sample_paths(
            "use kit\nsample(load(\"break.wav\"), ramp())\n",
            &[("/p/kit.swync", DRUMS)]
        ),
        vec!["break.wav"]
    );
}

/// A module that defines its own `load` is not naming a file, so its argument
/// is left exactly where the module put it.
#[test]
fn a_modules_own_load_is_not_a_path() {
    let items = expand_ok(
        "use kit::*\nsin(load(2))\n",
        &[("/p/kit.swync", "fn load(x) = x * 220\n")],
    );
    assert!(defined(&items).contains(&"kit::load".to_string()));
    assert!(sample_paths("use kit::*\nsin(load(2))\n", &[("/p/kit.swync", "fn load(x) = x * 220\n")]).is_empty());
    assert_eq!(constant_of(&items), 440.0);
}

// ---- installed libraries ----

/// The store a library is installed in, in these tests. A folder like any
/// other — which is the whole of what an installed library is.
const STORE: &str = "/store";
/// The copy a project carries, which is asked before the machine's.
const VENDORED: &str = "/p/.swync/libraries";

fn expand_with(entry: &str, modules: &[(&str, &str)], libraries: &[&str]) -> Vec<SwyncItem> {
    match try_expand_from(entry, modules, libraries) {
        Ok(items) => items,
        Err(e) => panic!("expected {entry:?} to expand, got: {e}"),
    }
}

/// A `use` that names nothing in the project reaches the store, and what it
/// finds there is a module like any other.
#[test]
fn a_library_is_found_when_the_project_has_no_such_file() {
    let items = expand_with(
        "use kit\nplay([50], kit::kick)\n",
        &[("/store/kit.swync", DRUMS)],
        &[STORE],
    );

    assert!(defined(&items).contains(&"kit::kick".to_string()));
    let lowered = lower(&items).expect("should lower");
    assert_eq!(lowered.bindings[0].target, "kit::kick");
    assert!(Instruments::from_program(&items).has("kit::kick"));
}

/// A library's own `use` resolves inside the store, so a pack of several files
/// works from wherever it was installed.
#[test]
fn a_library_may_import_its_own_files() {
    assert_eq!(
        constant_of(&expand_with(
            "use kit::*\nsin(hz)\n",
            &[
                ("/store/kit.swync", "use kit::tuning::*\nlet hz = base * 2\n"),
                ("/store/kit/tuning.swync", "let base = 110\n"),
            ],
            &[STORE],
        )),
        220.0
    );
}

/// A file in the project wins. Installing something must never change what a
/// project that already worked means.
#[test]
fn a_project_file_beats_an_installed_library() {
    assert_eq!(
        constant_of(&expand_with(
            "use kit::*\nsin(hz)\n",
            &[
                ("/p/kit.swync", "let hz = 440\n"),
                ("/store/kit.swync", "let hz = 110\n"),
            ],
            &[STORE],
        )),
        440.0
    );
}

/// Even when the project's file has nothing to offer: the project answers the
/// whole question before a store is asked, so a `use` never lands half in one
/// place and half in another.
#[test]
fn a_project_file_of_that_name_answers_for_the_whole_path() {
    let err = expand_err_with(
        "use kit::kick\n",
        &[
            ("/p/kit.swync", "fn snare(f) = noise()\n"),
            ("/store/kit/kick.swync", DRUMS),
        ],
        &[STORE],
    );
    assert!(
        err.message.contains("no `kick`"),
        "should have stopped at the project's kit, got {err}"
    );
}

/// The project's own copy beats the machine's, so a project that carries a
/// library plays the one it carries.
#[test]
fn a_vendored_library_beats_an_installed_one() {
    assert_eq!(
        constant_of(&expand_with(
            "use kit::*\nsin(hz)\n",
            &[
                ("/p/.swync/libraries/kit.swync", "let hz = 330\n"),
                ("/store/kit.swync", "let hz = 110\n"),
            ],
            &[VENDORED, STORE],
        )),
        330.0
    );
}

/// A `use` that finds nothing says everywhere it went — which now includes the
/// stores, so "it is not installed" is readable from the message.
#[test]
fn a_missing_module_names_every_folder_it_looked_in() {
    let err = expand_err_with("use kit\n", &[], &[VENDORED, STORE]);
    assert!(err.message.contains("/p/kit.swync"), "got {err}");
    assert!(err.message.contains("/p/.swync/libraries/kit.swync"), "got {err}");
    assert!(err.message.contains("/store/kit.swync"), "got {err}");
}

/// A library's samples are its own, wherever it was installed — the same rule
/// as any other module, and the reason a pack can carry audio at all.
#[test]
fn a_librarys_samples_come_from_the_store() {
    let items = expand_with(
        "use kit\nplay([50], kit::hit)\n",
        &[(
            "/store/kit.swync",
            "fn hit(f) = sample(load(\"samples/909.wav\"), ramp())\n",
        )],
        &[STORE],
    );
    assert_eq!(
        crate::samples::paths_in(&items),
        vec!["/store/samples/909.wav"]
    );
}

// ---- what the editor is told a file may write ----

/// `symbols`, for a file being run from `/p/song.swync`.
///
/// Real files rather than the map, because the command behind it takes a
/// `Workspace` and a workspace reads a disk.
fn symbols_of(entry: &str, files: &[(&str, &str)]) -> Vec<Symbol> {
    let root = std::env::temp_dir().join(format!(
        "swync-symbols-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::remove_dir_all(&root).ok();
    for (path, body) in files {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().expect("has a parent")).expect("should make");
        std::fs::write(full, body).expect("should write");
    }
    std::fs::create_dir_all(&root).expect("should make");

    let song = root.join("song.swync");
    let ws = Workspace::new(Some(song.display().to_string()), Some(root.display().to_string()));
    let found = symbols(
        parse(entry.to_string()).expect("should parse"),
        entry,
        &ws,
    );

    std::fs::remove_dir_all(&root).ok();
    found
}

fn named<'a>(found: &'a [Symbol], name: &str) -> &'a Symbol {
    found
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no `{name}` in {:?}", found.iter().map(|s| &s.name).collect::<Vec<_>>()))
}

/// The case regexes cannot reach: a glob's names live in a file the editor has
/// never read, so only expansion knows them.
#[test]
fn a_glob_reports_the_names_it_actually_brought() {
    let found = symbols_of(
        "use kit::*\n",
        &[("kit.swync", "fn kick(f, decay = 0.25) = sin(f)\nlet root = 55\n")],
    );

    let kick = named(&found, "kick");
    assert!(kick.callable);
    assert_eq!(kick.params, vec!["f", "decay"]);
    // Which of them a `play` lane may name, and which the pattern fills.
    assert_eq!(kick.optional, vec![false, true]);

    assert!(!named(&found, "root").callable);
}

/// And each of the other spellings, in the spelling the file uses — which is
/// what completion has to insert.
#[test]
fn every_spelling_is_reported_as_the_file_writes_it() {
    let module = &[("kit.swync", "fn kick(f) = sin(f)\n")];

    let plain = symbols_of("use kit\n", module);
    assert!(plain.iter().any(|s| s.name == "kit::kick"));
    assert!(!plain.iter().any(|s| s.name == "kick"), "a module is not unpacked");

    let renamed = symbols_of("use kit as k\n", module);
    assert!(renamed.iter().any(|s| s.name == "k::kick"));

    let single = symbols_of("use kit::kick as thump\n", module);
    assert!(single.iter().any(|s| s.name == "thump"));
}

/// The file's own definitions come too: they are names it may write, and the
/// same question is being asked about all of them.
#[test]
fn a_files_own_definitions_are_reported() {
    let found = symbols_of("fn pad(n, cut = 800) = saw(n)\n", &[]);
    let pad = named(&found, "pad");
    assert_eq!(pad.params, vec!["n", "cut"]);
    assert_eq!(pad.optional, vec![false, true]);
}

/// A file that does not expand has nothing to add, which is most of the time
/// this is asked — and is not an error, because the buffer is half-typed.
#[test]
fn a_file_that_does_not_expand_reports_nothing() {
    assert!(symbols_of("use nothing::at::all\n", &[]).is_empty());
}

// ---- enums across files ----

/// A module that names a scale, for the tests below.
const TONES: &str = "enum Scale { major = [0, 2, 4, 5, 7, 9, 11], pentatonic = [0, 3, 5, 7] }\n";

/// Read back the constant a program folded to, once expanded.
fn folds_to(entry: &str, modules: &[(&str, &str)]) -> f64 {
    let items = expand_ok(entry, modules);
    let g = lower(&items).expect("should lower").graph;
    match g.nodes[0].inputs[0] {
        NodeInput::Const(v) => v,
        _ => panic!("expected a folded constant"),
    }
}

/// An enum is filed under the module's name like any other definition, and is
/// reached the same three ways.
#[test]
fn an_enum_can_be_imported() {
    assert_eq!(
        folds_to("use tones\nsin(61.scale(tones::Scale.major))\n",
                 &[("/p/tones.swync", TONES)]),
        60.0, "qualified through the module");

    assert_eq!(
        folds_to("use tones::Scale\nsin(61.scale(Scale.major))\n",
                 &[("/p/tones.swync", TONES)]),
        60.0, "named directly");

    assert_eq!(
        folds_to("use tones::*\nsin(61.scale(Scale.major))\n",
                 &[("/p/tones.swync", TONES)]),
        60.0, "brought in by a glob");
}

/// `use tones::Scale as S` — an enum renames like anything else, and its
/// members are still reached through whatever it is called here.
#[test]
fn an_imported_enum_can_be_aliased() {
    assert_eq!(
        folds_to("use tones::Scale as S\nsin(61.scale(S.major))\n",
                 &[("/p/tones.swync", TONES)]),
        60.0);
}

/// The renamer trap, and the reason `Scope` knows which names are enums.
///
/// `Scale.major` is the same shape as `xs.rev`, so the walk that rewrites
/// imported names would rewrite `major` too. It only shows when the file has
/// something else by that name — here an imported `major` from a second module,
/// which the member must not be confused for.
#[test]
fn a_member_is_not_renamed_to_an_import_of_the_same_name() {
    let entry = "use tones::Scale\n\
                 use helpers::major\n\
                 sin(61.scale(Scale.major) + major(1))\n";
    let modules = &[
        ("/p/tones.swync", TONES),
        ("/p/helpers.swync", "fn major(x) = x * 1000\n"),
    ];

    let items = expand_ok(entry, modules);
    let g = lower(&items).expect("should lower").graph;
    // 60 from the snapped note, 1000 from the function: the member and the
    // function are two different things and both still work.
    assert_eq!(g.nodes[0].inputs[0], NodeInput::Const(1060.0));
}

/// A parameter called `Scale` is not the enum, so the dot after it is an
/// ordinary method call — which is what makes the rule above safe to apply.
#[test]
fn a_binding_shadowing_an_enum_is_not_reached_through() {
    let entry = "use tones::Scale\n\
                 fn f(Scale) = Scale.rev[0]\n\
                 sin(f([7, 8]))\n";
    let items = expand_ok(entry, &[("/p/tones.swync", TONES)]);
    let g = lower(&items).expect("should lower").graph;
    assert_eq!(g.nodes[0].inputs[0], NodeInput::Const(8.0));
}

/// A `match` on an imported enum reaches the same member the expression form
/// does: the type is a mention and is rewritten, and the member after it is not.
#[test]
fn a_match_pattern_reaches_an_imported_enum() {
    let sections = "enum Section { verse, chorus }\n";
    assert_eq!(
        folds_to("use parts::Section\n\
                  sin(match Section.chorus { Section.verse => 1, Section.chorus => 2 })\n",
                 &[("/p/parts.swync", sections)]),
        2.0);
}

/// The other half of the renamer trap, in the position where it would be
/// silent. A name a pattern *binds* is not a mention of anything, so a file
/// that has imported something under that name must not have the binding
/// rewritten to it — the arm would read correctly and be about somebody else's
/// definition.
#[test]
fn a_name_bound_by_a_pattern_is_not_renamed_to_an_import() {
    let entry = "use helpers::n\n\
                 sin(match 7 { n => n * 2 })\n";
    let items = expand_ok(entry, &[("/p/helpers.swync", "fn n() = 1000\n")]);
    let g = lower(&items).expect("should lower").graph;
    assert_eq!(g.nodes[0].inputs[0], NodeInput::Const(14.0),
               "the arm is about its own `n`, not the imported one");
}

/// A module's enum keeps its written name in messages: the reader wrote
/// `use tones::Scale`, and has never seen the spelling expansion filed it under.
#[test]
fn an_error_about_an_imported_enum_names_it_as_the_file_wrote_it() {
    let items = expand_ok(
        "use tones::Scale\nsin(Scale.majr)\n",
        &[("/p/tones.swync", TONES)],
    );
    let err = match lower(&items) {
        Err(e) => e,
        Ok(_) => panic!("`Scale.majr` should be refused"),
    };
    assert!(err.contains("enum `Scale`"), "got: {err}");
    assert!(!err.contains("tones::Scale"), "and not the filed name: {err}");
}
