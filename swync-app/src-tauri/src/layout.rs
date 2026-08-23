//! Where the window was, and what was open around the editor.
//!
//! This is the third of the three things the app remembers, and the split is
//! the same one `settings.rs` draws: a project's file travels with the piece,
//! the settings are about this machine, and what is here is about this screen
//! — a window's size means nothing on a monitor it was never opened on, and a
//! panel dragged wide on a desk is not something anybody wants arriving with
//! somebody else's music.
//!
//! Its own file rather than a field in the settings, because the two are
//! written by different hands at different rates. The settings are sent whole
//! from the panel that shows them; a window being dragged is written from here
//! several times a second, while nothing in the frontend knows it moved. One
//! file holding both would have each writer flattening the other's half — a
//! window resized and then a font zoomed would put the window back where it
//! was, and neither side could see it happen.
//!
//! Nothing in it is required, and a file that cannot be read is a first run
//! rather than a failure, for the reason the settings give: none of this is
//! worth refusing to start over, and every field has a default the app would
//! have used anyway. What those defaults *are* is deliberately not written
//! down here — see [`Panel`].

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the layout file is called, in the app's config directory.
pub const FILE: &str = "layout.json";

/// How long after the last change the file is written.
///
/// A drag is hundreds of events and one layout, so the burst is let settle
/// first — the same shape the project watcher uses, and for the same reason.
const SETTLE: Duration = Duration::from_millis(400);

/// How much of the window's top edge has to land on a screen for the window to
/// be worth putting back there: less than this and there is no title bar left
/// to drag it out by.
///
/// In the platform's own pixels, like everything else here.
const GRAB: i32 = 40;

/// The smallest window worth restoring, in those same pixels.
///
/// Not a minimum size — the app has none, and a window can be dragged smaller
/// than this all it likes. It is a guard on the *file*, which is JSON in a
/// folder anybody can open: a zero in it would otherwise open a window with
/// nothing in it and no way to tell why.
const SMALLEST: u32 = 200;

/// A rectangle on the desktop, in the platform's own pixels: a monitor, or the
/// window measured against them.
///
/// Physical rather than logical throughout, because the two are only worth
/// converting between if something is going to move: the monitors arrive this
/// way, the window reports itself this way, and a size that went out logical
/// and came back physical would be a window that grew every launch on a retina
/// screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    fn right(&self) -> i32 {
        self.x.saturating_add(self.width as i32)
    }

    fn bottom(&self) -> i32 {
        self.y.saturating_add(self.height as i32)
    }
}

/// Where the window was, and how big.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    /// Where its top-left corner was.
    ///
    /// `None` leaves the placing to the platform, which is what a first run
    /// gets — and what a window whose screen has since been unplugged gets,
    /// since a remembered corner on a monitor that is not there today would
    /// open the app somewhere nobody can reach it.
    #[serde(default)]
    pub position: Option<Position>,
    pub width: u32,
    pub height: u32,
    /// Whether it was maximized, which is a state rather than a size.
    ///
    /// The size beside it is the one it had *before* being maximized, so
    /// un-maximizing a restored window gives back the window somebody chose
    /// rather than the screen it was filling.
    #[serde(default)]
    pub maximized: bool,
}

/// The top-left corner of the window, in the desktop's coordinates.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub x: i32,
    pub y: i32,
}

/// One of the two side panels as it was left.
///
/// The widths and the view names are the frontend's, and are kept as they
/// arrive: what a panel may be dragged to, and which views it has, are things
/// the panel knows and this does not. A default written down twice is one that
/// can disagree with itself — the same reason `settings.rs` keeps the editor's
/// font size as an `Option` rather than naming the size it starts at.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Panel {
    pub open: bool,
    /// Pixels wide, as the drag handle left it — kept whether or not the panel
    /// is open, so reopening one gives back the width it had.
    pub width: f64,
    /// Which of its views was on top, by the name the frontend calls it.
    pub view: String,
}

/// What the window and the panels were, last time there was a last time.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Layout {
    #[serde(default)]
    pub window: Option<Window>,
    #[serde(default)]
    pub left: Option<Panel>,
    #[serde(default)]
    pub right: Option<Panel>,
}

/// Whether the window would land somewhere it can be reached, given the
/// screens there are today.
///
/// Overlapping a monitor is not enough on its own: a window whose only visible
/// pixels are its bottom-right corner is one that cannot be dragged, resized
/// or closed. What has to be on a screen is a piece of the top edge wide
/// enough to grab.
pub fn reachable(window: Rect, monitors: &[Rect]) -> bool {
    monitors.iter().any(|screen| {
        let across = window.right().min(screen.right()) - window.x.max(screen.x);
        // The title bar, rather than the whole window: the rest of it may hang
        // off the bottom of the screen and still be perfectly usable.
        let down = (window.y + GRAB).min(screen.bottom()) - window.y.max(screen.y);
        across >= GRAB && down > 0
    })
}

/// What a remembered window is still worth restoring of.
///
/// Every field in one is a fact about a desk that has moved on since it was
/// written — a screen unplugged, a laptop docked, a file hand-edited — and
/// none of the ways it can have moved on is an error. A size that is nonsense
/// is dropped whole; a position that is no longer on any screen is dropped on
/// its own, so the window keeps the size somebody chose and the platform
/// places it. This mirrors `usable_session` in `lib.rs`: what cannot be
/// restored simply does not come back.
pub fn usable(window: Window, monitors: &[Rect]) -> Option<Window> {
    if window.width < SMALLEST || window.height < SMALLEST {
        return None;
    }
    // With no monitors to check against — a headless machine, or a platform
    // that would not say — the position is kept: guessing it away would be a
    // worse answer than the one that was true last time.
    let lost = window.position.is_some_and(|at| {
        !monitors.is_empty()
            && !reachable(
                Rect { x: at.x, y: at.y, width: window.width, height: window.height },
                monitors,
            )
    });
    Some(Window { position: if lost { None } else { window.position }, ..window })
}

/// Read the layout, or nothing if there is none to read.
pub fn read(path: &Path) -> Layout {
    match std::fs::read_to_string(path) {
        // Half-written by a crash, or written by a version that has since
        // changed the shape: a first run rather than a failure.
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Layout::default(),
    }
}

/// Write it, making the config directory if this is the first time.
pub fn write(path: &Path, layout: &Layout) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(layout).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// The layout as it is right now, and the thread that writes it down.
///
/// Every change goes through here rather than to the file, and that is what
/// makes two writers safe: the window's own events arrive on the main thread
/// while the panels arrive from the frontend, and both are edits to one value
/// under one lock rather than two read-modify-writes racing over a file.
///
/// Cloning one is cloning the handle, not the layout — every clone is the same
/// value and the same thread.
#[derive(Clone)]
pub struct Writer {
    /// What is true now. Read by the thread when it writes, and by the command
    /// that hands the layout to a window as it opens.
    now: Arc<Mutex<Layout>>,
    /// A nudge per change. Carries nothing: the layout it refers to is the one
    /// above, which by the time the thread looks may have changed again.
    changed: Sender<()>,
    /// Where it is written. `None` on a machine with no config directory to
    /// write to, where the layout is still tracked and answered for — the
    /// panels ask for it as the window opens — and simply never written down.
    /// The alternative is a relative path, and a relative path here is a file
    /// dropped into whatever directory the app happened to be launched from:
    /// the source tree under `tauri dev`, and `/` for a bundled app.
    path: Option<PathBuf>,
}

impl Writer {
    /// Start remembering, from what was read off disk.
    pub fn start(path: Option<PathBuf>, layout: Layout) -> Writer {
        let (changed, nudges) = mpsc::channel();
        let writer = Writer { now: Arc::new(Mutex::new(layout)), changed, path };

        let background = writer.clone();
        std::thread::spawn(move || {
            while nudges.recv().is_ok() {
                // Let the burst settle. A window being dragged sends one of
                // these a frame, and all of them describe the same window
                // coming to rest somewhere.
                loop {
                    match nudges.recv_timeout(SETTLE) {
                        Ok(()) => continue,
                        Err(RecvTimeoutError::Timeout) => break,
                        // The app is closing. Whatever the last change was is
                        // already in `now`, and `flush` on the way out has it.
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                background.flush();
            }
        });

        writer
    }

    /// Change it, and start the clock on writing it out.
    pub fn change(&self, edit: impl FnOnce(&mut Layout)) {
        // A poisoned lock is a thread that panicked mid-edit, which leaves a
        // layout that is merely wrong rather than one that is dangerous —
        // and refusing to remember a window from here on would be the worse
        // half of the trade.
        let mut now = self.now.lock().unwrap_or_else(|e| e.into_inner());
        edit(&mut now);
        drop(now);
        // A closed channel is the app on its way out, which is not a failure.
        let _ = self.changed.send(());
    }

    /// What is true now, for the window that is asking as it opens.
    pub fn current(&self) -> Layout {
        self.now.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Write it out now, without waiting for the burst to settle.
    ///
    /// What the closing window calls, because the settling above is longer
    /// than the last drag before a quit: a debounce that outlives the app it
    /// is debouncing remembers nothing.
    pub fn flush(&self) {
        let Some(path) = &self.path else { return };
        let layout = self.current();
        if let Err(e) = write(path, &layout) {
            // Costs the next launch its window and nothing this one, so it is
            // said rather than shown — there is no panel left to show it in by
            // the time this matters.
            eprintln!("could not remember the layout: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "swync-layout-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("should create a temp folder");
        dir
    }

    /// One 1920x1080 screen with its top-left corner at the origin.
    fn screen() -> Rect {
        Rect { x: 0, y: 0, width: 1920, height: 1080 }
    }

    fn window(x: i32, y: i32) -> Window {
        Window {
            position: Some(Position { x, y }),
            width: 800,
            height: 600,
            maximized: false,
        }
    }

    #[test]
    fn a_layout_that_has_never_been_written_is_empty() {
        let root = temp("first-run");
        assert_eq!(read(&root.join(FILE)), Layout::default());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn what_is_written_is_what_is_read_back() {
        let root = temp("round-trip");
        let path = root.join("nested").join(FILE);
        let layout = Layout {
            window: Some(Window {
                position: Some(Position { x: 120, y: 64 }),
                width: 1280,
                height: 800,
                maximized: true,
            }),
            left: Some(Panel { open: true, width: 340.0, view: "project".to_string() }),
            right: Some(Panel { open: false, width: 288.0, view: "controls".to_string() }),
        };
        write(&path, &layout).expect("should write");
        assert_eq!(read(&path), layout);
        std::fs::remove_dir_all(&root).ok();
    }

    /// It is JSON in a folder anybody can open, so it can be nonsense by the
    /// time it is read — and a window is not worth refusing to start over.
    #[test]
    fn a_layout_file_that_cannot_be_read_is_a_first_run_rather_than_a_failure() {
        let root = temp("nonsense");
        let path = root.join(FILE);
        std::fs::write(&path, "{ this is not json").expect("should write");
        assert_eq!(read(&path), Layout::default());
        std::fs::remove_dir_all(&root).ok();
    }

    /// A file from a version that had not thought of half of this is read for
    /// the half it does have, which is what every `default` here is for.
    #[test]
    fn a_layout_missing_everything_optional_still_reads() {
        let root = temp("partial");
        let path = root.join(FILE);
        std::fs::write(&path, r#"{"window":{"width":900,"height":700}}"#).expect("should write");
        assert_eq!(
            read(&path).window,
            Some(Window { position: None, width: 900, height: 700, maximized: false })
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_window_where_it_was_left_is_restored_whole() {
        let saved = window(100, 100);
        assert_eq!(usable(saved, &[screen()]), Some(saved));
    }

    /// The common case of a laptop that was docked: everything was on the
    /// second screen, and today there is only the first.
    #[test]
    fn a_window_on_a_screen_that_is_gone_keeps_its_size_and_loses_its_place() {
        let saved = window(2400, 300);
        let restored = usable(saved, &[screen()]).expect("should keep the size");
        assert_eq!(restored.position, None);
        assert_eq!((restored.width, restored.height), (800, 600));
    }

    /// Hanging off an edge is ordinary — a window pushed past the bottom of
    /// the screen is still one somebody put there on purpose.
    #[test]
    fn a_window_hanging_off_the_bottom_of_a_screen_is_still_where_it_was() {
        let saved = window(200, 1000);
        assert_eq!(usable(saved, &[screen()]).expect("should keep it").position, saved.position);
    }

    /// Overlapping is not enough: what has to be reachable is the title bar,
    /// because a window with only its bottom corner on screen cannot be
    /// dragged back out.
    #[test]
    fn a_window_with_only_its_bottom_corner_showing_is_not_reachable() {
        let saved = window(-780, -560);
        assert_eq!(usable(saved, &[screen()]).expect("should keep the size").position, None);
    }

    #[test]
    fn a_window_over_the_second_of_two_screens_is_left_there() {
        let second = Rect { x: 1920, y: 0, width: 2560, height: 1440 };
        let saved = window(2400, 300);
        assert_eq!(usable(saved, &[screen(), second]), Some(saved));
    }

    /// A machine that will not say what screens it has is not a machine with
    /// no screens: the position that was true last time is the best answer
    /// there is, and guessing it away would move a window nobody moved.
    #[test]
    fn with_no_screens_to_check_against_a_window_is_left_alone() {
        let saved = window(2400, 300);
        assert_eq!(usable(saved, &[]), Some(saved));
    }

    /// Only the file can produce one of these — a window cannot be dragged to
    /// nothing — and a window with no window in it is worse than none.
    #[test]
    fn a_window_too_small_to_hold_anything_is_not_restored() {
        assert_eq!(usable(Window { width: 0, height: 0, ..window(0, 0) }, &[screen()]), None);
        assert_eq!(usable(Window { height: 10, ..window(0, 0) }, &[screen()]), None);
    }

    /// The whole point of the writer: two hands editing one layout, and both
    /// halves surviving. The window's events arrive on the main thread while
    /// the panels arrive from the frontend, and neither may flatten the other.
    #[test]
    fn a_window_and_a_panel_can_be_remembered_without_flattening_each_other() {
        let root = temp("two-hands");
        let path = root.join(FILE);
        let writer = Writer::start(Some(path.clone()), Layout::default());

        writer.change(|layout| {
            layout.window = Some(window(10, 20));
        });
        writer.change(|layout| {
            layout.left = Some(Panel { open: true, width: 400.0, view: "search".to_string() });
        });
        writer.flush();

        let written = read(&path);
        assert_eq!(written.window, Some(window(10, 20)));
        assert_eq!(
            written.left.as_ref().expect("the panel should be there").view,
            "search"
        );
        assert_eq!(written, writer.current());

        std::fs::remove_dir_all(&root).ok();
    }

    /// A drag is one layout, however many events it is. Nothing is written
    /// until the burst has settled, which is what keeps a resize off the disk
    /// a hundred times.
    #[test]
    fn a_burst_of_changes_is_not_written_until_it_settles() {
        let root = temp("settling");
        let path = root.join(FILE);
        let writer = Writer::start(Some(path.clone()), Layout::default());

        for width in 800..900 {
            writer.change(|layout| {
                layout.window = Some(Window { width, ..window(0, 0) });
            });
        }
        // Still mid-drag, so the file does not exist yet — and `current` is
        // the truth throughout, which is what a window opening now would read.
        assert!(!path.exists(), "a settling burst should not have been written");
        assert_eq!(writer.current().window.expect("should be there").width, 899);

        std::thread::sleep(SETTLE * 3);
        assert_eq!(read(&path).window.expect("should have been written").width, 899);

        std::fs::remove_dir_all(&root).ok();
    }
}
