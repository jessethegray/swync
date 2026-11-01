//! The projects that have been opened, most recent first.
//!
//! A project is a folder somewhere on a disk, and until the splash page there
//! was no way back into one but the folder chooser: the app remembered the
//! single project that was open when it closed (`Session` in `lib.rs`) and
//! nothing about the ones before it. This is that history — a list of paths in
//! the app's config folder, pushed to every time a project is opened.
//!
//! Only paths are kept. A project's *name* lives in its own
//! `swync-project.json` and is free to change while the app is not looking, so
//! reading the list resolves each name from the project itself rather than
//! trusting a copy taken when it was last opened. For the same reason a path
//! that is no longer a folder is dropped on the way out instead of being
//! pruned on the way in: a project on an unmounted volume is still a project,
//! and it comes back on the list the moment the volume is there again.

use crate::project;

use std::path::Path;

/// What the list is called, in the app's config folder. Not in any project:
/// which pieces somebody has been working on belongs to the machine, the same
/// reason the recording folder does.
pub const FILE: &str = "recent-projects.json";

/// How many to keep. Long enough to hold the handful of pieces in progress,
/// short enough that the splash page is a list you read rather than scroll.
const LIMIT: usize = 10;

/// One entry, as the splash page draws it: what the project calls itself, and
/// where it is.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
pub struct Recent {
    pub name: String,
    /// Absolute, so a click opens it without knowing where the list came from
    /// — and shown under the name, since two pieces may share one.
    pub path: String,
}

/// Read the stored paths.
///
/// Anything wrong with the file is an empty list rather than an error: a
/// history is a convenience, and a first run, a half-written file and one
/// written by a version that has since changed shape all deserve the same
/// answer — the splash page simply has nothing to offer yet.
fn stored(file: &Path) -> Vec<String> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The list with `root` at its head, held to `LIMIT`.
///
/// Reopening a project is a move to the front and not a second entry, so the
/// list is of projects rather than of openings.
fn pushed(stored: Vec<String>, root: &str) -> Vec<String> {
    let mut list = Vec::with_capacity(stored.len() + 1);
    list.push(root.to_string());
    list.extend(stored.into_iter().filter(|saved| saved != root));
    list.truncate(LIMIT);
    list
}

/// What of the list is still worth offering, named as the projects name
/// themselves.
///
/// A folder that has moved, been deleted or gone with its volume is skipped,
/// for the reason a remembered session's missing file is: none of those are
/// errors, and a click that cannot land is worse than a short list. A folder
/// that is there but whose settings cannot be read still opens — that is a
/// project with no file yet, which is every folder until it is saved once —
/// and is listed under the folder's own name.
fn listed(stored: Vec<String>) -> Vec<Recent> {
    stored
        .into_iter()
        .filter(|path| Path::new(path).is_dir())
        .map(|path| {
            let root = Path::new(&path);
            let name = project::read(root)
                .ok()
                .flatten()
                .map(|project| project.name)
                .unwrap_or_else(|| project::folder_name(root));
            Recent { name, path }
        })
        .collect()
}

/// The projects to offer on the splash page.
pub fn read(file: &Path) -> Vec<Recent> {
    listed(stored(file))
}

/// Put a project at the head of the list.
///
/// Writing is best-effort by the same argument as reading: the caller has just
/// opened a project, and a history that could not be saved is not a reason to
/// say so over the piece.
pub fn remember(file: &Path, root: &str) -> Result<(), String> {
    let list = pushed(stored(file), root);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string(&list).map_err(|e| e.to_string())?;
    std::fs::write(file, text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "swync-recents-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("should create a temp dir");
        dir
    }

    /// The whole point of the file: open a project, close the app, find it
    /// waiting on the splash page.
    #[test]
    fn a_project_that_was_opened_is_offered_again() {
        let dir = temp_dir("roundtrip");
        let file = dir.join(FILE);
        let project = dir.join("piece");
        std::fs::create_dir_all(&project).expect("should create the project");

        remember(&file, &project.display().to_string()).expect("should remember");

        let read = read(&file);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].path, project.display().to_string());
        assert_eq!(read[0].name, "piece");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A project is named by its own file, so renaming the piece renames the
    /// entry — the list holds a path and asks the project the rest.
    #[test]
    fn an_entry_is_named_by_the_project_rather_than_the_folder() {
        let dir = temp_dir("named");
        let file = dir.join(FILE);
        let root = dir.join("folder-name");
        std::fs::create_dir_all(&root).expect("should create the project");
        project::write(
            &root,
            &Project { name: "Nocturne".into(), ..Project::new(&root, 120.0, Default::default(), 1.0) },
        )
        .expect("should write the project");

        remember(&file, &root.display().to_string()).expect("should remember");

        assert_eq!(read(&file)[0].name, "Nocturne");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Reopening a project moves it to the front rather than listing it twice.
    #[test]
    fn reopening_a_project_moves_it_to_the_front() {
        let first = "/one".to_string();
        let second = "/two".to_string();

        let list = pushed(vec![second.clone(), first.clone()], &first);

        assert_eq!(list, vec![first, second]);
    }

    /// The list is held to its limit, dropping whatever has been untouched
    /// longest.
    #[test]
    fn the_oldest_project_falls_off_the_end() {
        let stored: Vec<String> = (0..LIMIT).map(|i| format!("/p{i}")).collect();

        let list = pushed(stored, "/new");

        assert_eq!(list.len(), LIMIT);
        assert_eq!(list[0], "/new");
        assert_eq!(list[LIMIT - 1], format!("/p{}", LIMIT - 2));
    }

    /// A folder that has moved or gone is skipped: a click on it could not
    /// land, and a short list is better than a dead row.
    #[test]
    fn a_project_that_is_no_longer_a_folder_is_not_offered() {
        let dir = temp_dir("missing");
        let there = dir.join("there");
        std::fs::create_dir_all(&there).expect("should create the project");

        let list = listed(vec![
            dir.join("gone").display().to_string(),
            there.display().to_string(),
        ]);

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, there.display().to_string());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A first run has no file, which is an empty list and not a failure.
    #[test]
    fn no_file_yet_is_an_empty_list() {
        let dir = temp_dir("first-run");

        assert_eq!(read(&dir.join(FILE)), Vec::new());

        std::fs::remove_dir_all(&dir).ok();
    }
}
