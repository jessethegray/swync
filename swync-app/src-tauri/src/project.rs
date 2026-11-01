//! What a project remembers between sessions.
//!
//! A project is a folder, and until now that was the whole of it: a tree in the
//! panel and a `patterns.swync` beside the files. But some of what you set while
//! working belongs to the piece rather than to the app — the tempo it is played
//! at, how loud it sits, what it is called — and all of it was lost the moment
//! the window closed.
//!
//! This is the file that keeps it: a `swync-project.json` in the project folder,
//! read when the project opens and written as any of it changes. JSON rather
//! than swync, because none of it is a program: the project's other file,
//! `patterns.swync`, is folded into every run, and settings must never be.
//!
//! Nothing here is required. A folder with no such file is still a project —
//! that is every folder until it is opened once — and reading one says so
//! rather than failing, so the transport is left exactly where the performer
//! put it. A file that is *there* but unreadable is a different matter, and is
//! reported: the next thing the editor would do is write over it.

use crate::diagnostic::{Diagnostic, Stage};
use crate::engine::DEFAULT_MASTER_VOLUME;
use crate::scheduler::clock::{bpm_from_cps, Meter, DEFAULT_CPS, DEFAULT_METER};

use std::path::{Path, PathBuf};

/// What the settings file is called, in every project.
pub const FILE: &str = "swync-project.json";

/// The tempo a project plays at when its file does not say — the clock's own
/// default, so a project that predates this file sounds as it always did.
fn default_bpm() -> f64 {
    bpm_from_cps(DEFAULT_CPS, DEFAULT_METER)
}

/// The signature a project is in when its file does not say. Four four, which
/// is what every project written before this field existed was in.
fn default_meter() -> Meter {
    DEFAULT_METER
}

/// The same for the master, which defaults to unity: a program is heard as
/// written until somebody turns it down.
fn default_volume() -> f64 {
    DEFAULT_MASTER_VOLUME as f64
}

/// The name a folder's project has when nothing else names it: the folder's.
/// Public because the recent-projects list names an entry this way too, for a
/// folder whose settings file is not there or cannot be read.
pub fn folder_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project")
        .to_string()
}

/// What a project remembers.
///
/// Every field has a default, so a hand-written file may say only the one thing
/// it means to change and the rest is what the engine would have done anyway.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Project {
    /// What the project is called, which is the editor's to show and nothing
    /// else's. It starts as the folder's name and is free to diverge: a folder
    /// is named for where it sits on a disk, a piece for what it is.
    #[serde(default)]
    pub name: String,
    /// Beats per minute, as the transport reads it.
    #[serde(default = "default_bpm")]
    pub bpm: f64,
    /// The time signature, which is how long a bar is and therefore what every
    /// `take`, `at` and bare pattern in the project is counted in.
    ///
    /// It belongs to the piece rather than to the app for the same reason the
    /// tempo does — and more so: a program written in 3/4 means something else
    /// entirely when read in 4/4, so this travels with the files rather than
    /// with whoever last dragged a control.
    #[serde(default = "default_meter")]
    pub meter: Meter,
    /// Master output, as a linear amplitude between silence and unity.
    #[serde(default = "default_volume")]
    pub volume: f64,
}

impl Project {
    /// A project for a folder that has none saved, taking the transport as it
    /// stands. Opening a folder must not move a fader, so what is playing now
    /// *is* the answer until the file says otherwise.
    pub fn new(root: &Path, bpm: f64, meter: Meter, volume: f64) -> Project {
        Project { name: folder_name(root), bpm, meter, volume }.sanitized(root)
    }

    /// Make a file safe to hand the engine.
    ///
    /// This one is edited by hand — it is JSON in the project the tree is
    /// showing — so a zero tempo or a volume of 40 is a thing that can arrive
    /// here. None of it is worth refusing a project over: a nonsense number is
    /// replaced by the default, a volume outside the fader's travel is clamped
    /// to it, and a blank name falls back to the folder's.
    fn sanitized(mut self, root: &Path) -> Project {
        self.name = self.name.trim().to_string();
        if self.name.is_empty() {
            self.name = folder_name(root);
        }
        if !self.bpm.is_finite() || self.bpm <= 0.0 {
            self.bpm = default_bpm();
        }
        // A signature nobody could write in note values — 4/5, or a bar of a
        // thousand — is replaced rather than refused, exactly as a nonsense
        // tempo is. The alternative is a project that will not open because a
        // hand-edited number is wrong.
        if !self.meter.is_valid() {
            self.meter = default_meter();
        }
        self.volume = if self.volume.is_finite() {
            self.volume.clamp(0.0, 1.0)
        } else {
            default_volume()
        };
        self
    }
}

/// Where a project's settings live.
pub fn file_path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// The file a project is played from, in every project.
///
/// A project has an entry point the way a crate has a `main.rs`: play runs
/// this file rather than whichever tab is in front, so editing a module and
/// reaching for play still plays the piece rather than the fragment. What is
/// in front is a keystroke away — the editor plays it with the shift of the
/// same shortcut — and that is the one auditioning a module needs.
pub const MAIN: &str = "main.swync";

/// Where that file lives.
pub fn main_path(root: &Path) -> PathBuf {
    root.join(MAIN)
}

/// The project's `main.swync`, if the folder has one.
///
/// `None` for every project made before this file existed, and for a folder
/// that happens to have a *directory* by that name. The editor answers both
/// the same way — by playing the file in front, as it always did — so neither
/// is a failure worth reporting.
pub fn main_file(root: &Path) -> Option<PathBuf> {
    let path = main_path(root);
    path.is_file().then_some(path)
}

/// Start a project's `main.swync`, and leave one that is already there alone.
///
/// Empty rather than a starter program: what play runs is the piece, and a
/// piece that opens with somebody else's two lines in it is a file whose first
/// edit is a delete.
///
/// Created with `create_new`, which is the whole reason this is not a `write`.
/// New Project on a folder that has been worked in before adopts it exactly as
/// opening it would, and adopting must not write over the piece.
pub fn create_main(root: &Path) -> Result<(), Diagnostic> {
    let path = main_path(root);
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(Diagnostic::message(
            Stage::Import,
            format!("could not create {}: {e}", path.display()),
        )),
    }
}

/// Read a project's settings.
///
/// `None` is a folder that has never been saved as a project, which is not a
/// failure — it is every folder the first time it is opened, and every project
/// made before this file existed. An unparseable file is a failure, and is
/// named: the editor is about to start writing this file, and it must not do
/// that over something it could not read.
pub fn read(root: &Path) -> Result<Option<Project>, Diagnostic> {
    let path = file_path(root);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let project: Project = serde_json::from_str(&text).map_err(|e| {
                Diagnostic::message(
                    Stage::Import,
                    format!("could not read {}: {e}", path.display()),
                )
            })?;
            Ok(Some(project.sanitized(root)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Diagnostic::message(
            Stage::Import,
            format!("could not read {}: {e}", path.display()),
        )),
    }
}

/// Write a project's settings out, and say where they went.
///
/// Sanitised on the way, for the same reason as on the way in: what lands on
/// disk is a file this app can open again, whatever the editor was holding.
pub fn write(root: &Path, project: &Project) -> Result<PathBuf, Diagnostic> {
    let path = file_path(root);
    let project = project.clone().sanitized(root);
    // Pretty, and with the newline every other tool expects at the end of a
    // text file: this sits in the project tree and is meant to be opened.
    let mut text = serde_json::to_string_pretty(&project).map_err(|e| {
        Diagnostic::message(Stage::Import, format!("could not write {}: {e}", path.display()))
    })?;
    text.push('\n');

    std::fs::write(&path, text).map_err(|e| {
        Diagnostic::message(Stage::Import, format!("could not save {}: {e}", path.display()))
    })?;

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "swync-settings-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("should create a temp project");
        dir
    }

    /// The whole point of the file: set a tempo, close, come back to it.
    #[test]
    fn settings_survive_a_project_being_reopened() {
        let root = temp_project("roundtrip");
        let saved = Project { name: "Nocturne".into(), bpm: 96.0, meter: Default::default(), volume: 0.7 };

        let path = write(&root, &saved).expect("should write");
        assert_eq!(path, file_path(&root));

        let read = read(&root).expect("should read").expect("should be a project");
        assert_eq!(read, saved);

        std::fs::remove_dir_all(&root).ok();
    }

    /// A folder nobody has saved settings for is not a failure — it is every
    /// folder until it is opened once.
    #[test]
    fn a_folder_with_no_file_has_no_settings() {
        let root = temp_project("none");
        assert_eq!(read(&root).expect("a missing file is not an error"), None);
        std::fs::remove_dir_all(&root).ok();
    }

    /// One that does not parse is, because writing over it is what happens next.
    #[test]
    fn an_unreadable_file_is_reported_with_its_path() {
        let root = temp_project("broken");
        std::fs::write(file_path(&root), "{ not json at all").expect("should write");

        let err = read(&root).expect_err("should not read");
        assert!(
            err.message.contains(FILE),
            "the message should name the file, got {err}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// A file may say only what it means to change.
    #[test]
    fn missing_fields_fall_back_to_the_defaults() {
        let root = temp_project("partial");
        std::fs::write(file_path(&root), r#"{ "bpm": 140 }"#).expect("should write");

        let read = read(&root).expect("should read").expect("should be a project");
        assert_eq!(read.bpm, 140.0);
        assert_eq!(read.volume, default_volume());
        // Unnamed is named for its folder rather than blank.
        assert_eq!(read.name, folder_name(&root));

        std::fs::remove_dir_all(&root).ok();
    }

    /// Hand-edited nonsense is corrected rather than refused: none of it is
    /// worth failing to open a project over.
    #[test]
    fn unusable_values_are_replaced_or_clamped() {
        let root = temp_project("nonsense");
        std::fs::write(
            file_path(&root),
            r#"{ "name": "   ", "bpm": 0, "volume": 40 }"#,
        )
        .expect("should write");

        let read = read(&root).expect("should read").expect("should be a project");
        assert_eq!(read.name, folder_name(&root));
        assert_eq!(read.bpm, default_bpm());
        assert_eq!(read.volume, 1.0);

        std::fs::remove_dir_all(&root).ok();
    }

    /// And is corrected on the way out too, so what lands on disk always opens.
    #[test]
    fn what_is_written_can_always_be_read_back() {
        let root = temp_project("sanitised");
        let wild = Project { name: String::new(), bpm: f64::NAN, meter: Default::default(), volume: -3.0 };
        write(&root, &wild).expect("should write");

        let read = read(&root).expect("should read").expect("should be a project");
        assert_eq!(read.name, folder_name(&root));
        assert_eq!(read.bpm, default_bpm());
        assert_eq!(read.volume, 0.0);

        std::fs::remove_dir_all(&root).ok();
    }

    /// What New Project does beyond writing the settings: the file play runs
    /// exists from the first moment, so the button has something to play.
    #[test]
    fn a_new_project_gets_an_empty_file_to_play() {
        let root = temp_project("main");
        create_main(&root).expect("should create");

        assert_eq!(main_file(&root), Some(main_path(&root)));
        assert_eq!(
            std::fs::read_to_string(main_path(&root)).expect("should read"),
            "",
            "a new project's program is the performer's to write"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// New Project on a folder that has been worked in is an adoption, and an
    /// adoption that overwrote the piece would be the worst bug in the app.
    #[test]
    fn starting_a_project_where_one_is_playable_leaves_that_file_alone() {
        let root = temp_project("main-exists");
        std::fs::write(main_path(&root), "sin(440) * 0.1").expect("should write");

        create_main(&root).expect("an existing file is not an error");

        assert_eq!(
            std::fs::read_to_string(main_path(&root)).expect("should read"),
            "sin(440) * 0.1"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Every project made before this file existed, which plays the file in
    /// front instead.
    #[test]
    fn a_project_without_that_file_has_nothing_of_its_own_to_play() {
        let root = temp_project("no-main");
        assert_eq!(main_file(&root), None);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A folder is not a program, and answering with one would have the editor
    /// read it and report a failure nobody caused.
    #[test]
    fn a_folder_by_that_name_is_not_something_to_play() {
        let root = temp_project("main-is-a-folder");
        std::fs::create_dir(main_path(&root)).expect("should create");
        assert_eq!(main_file(&root), None);
        std::fs::remove_dir_all(&root).ok();
    }

    /// A folder with no settings takes the transport as it stands: picking a
    /// project must not move a fader.
    #[test]
    fn a_new_project_takes_the_transport_as_it_is() {
        let root = temp_project("fresh");
        let project = Project::new(&root, 132.0, Meter { top: 3, bottom: 4 }, 0.5);
        assert_eq!(project.name, folder_name(&root));
        assert_eq!(project.bpm, 132.0);
        assert_eq!(project.meter, Meter { top: 3, bottom: 4 });
        assert_eq!(project.volume, 0.5);
        std::fs::remove_dir_all(&root).ok();
    }

    /// The signature travels with the piece. A program in 3/4 means something
    /// else read in 4/4, so this is the field it would be worst to lose.
    #[test]
    fn the_signature_survives_a_round_trip() {
        let root = temp_project("signature");
        let saved = Project {
            name: "Waltz".into(),
            bpm: 120.0,
            meter: Meter { top: 3, bottom: 4 },
            volume: 0.8,
        };
        write(&root, &saved).expect("should write");

        let read = read(&root).expect("should read").expect("should be a project");
        assert_eq!(read.meter, Meter { top: 3, bottom: 4 });

        std::fs::remove_dir_all(&root).ok();
    }

    /// A project written before the field existed opens in 4/4, which is what
    /// it was written in — there was nothing else to be.
    #[test]
    fn a_project_without_a_signature_opens_in_four_four() {
        let root = temp_project("no-signature");
        std::fs::write(
            file_path(&root),
            r#"{ "name": "Older", "bpm": 96.0, "volume": 0.5 }"#,
        )
        .expect("should write");

        let read = read(&root).expect("should read").expect("should be a project");
        assert_eq!(read.meter, DEFAULT_METER);
        assert_eq!(read.bpm, 96.0);

        std::fs::remove_dir_all(&root).ok();
    }

    /// And one hand-edited into a signature nobody can write opens anyway,
    /// in 4/4 — the same forgiveness a nonsense tempo gets.
    #[test]
    fn an_impossible_signature_is_replaced_rather_than_refused() {
        let root = temp_project("bad-signature");
        std::fs::write(
            file_path(&root),
            r#"{ "name": "Odd", "bpm": 96.0, "meter": { "top": 4, "bottom": 5 }, "volume": 0.5 }"#,
        )
        .expect("should write");

        let read = read(&root).expect("should read").expect("should be a project");
        assert_eq!(read.meter, DEFAULT_METER);

        std::fs::remove_dir_all(&root).ok();
    }
}
