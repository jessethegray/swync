/** One entry on the list, as `recent_projects` reports it. */
export interface RecentProject {
  name: string;
  /** Absolute, and shown under the name: two pieces may share a name, and
   *  which copy of one you are about to open is worth being able to read. */
  path: string;
}

interface SplashPageProps {
  /** Most recent first, already filtered to the folders that are still there. */
  recents: RecentProject[];
  /** Open one of them, which is the same act as the tree's own project
   *  choice: the folder is opened in place, not in another window. */
  onOpenRecent: (path: string) => void;
  onNewProject: () => void;
  onOpenProject: () => void;
}

/**
 * What the window shows when nothing is open.
 *
 * A project is a folder somewhere on a disk, and the only ways into one were a
 * menu item and a folder chooser — so an app with nothing open was an empty
 * editor over an empty tree, and getting back to last night's piece meant
 * remembering where it was and finding it again. This is the way back in: the
 * projects that have been opened, and the two acts that make a new one or
 * reach a folder the list has never seen.
 *
 * It stands in for the whole window rather than for the editor pane alone.
 * Everything around that pane — the transport, the tab strip, both panels —
 * describes a project being worked on, and with nothing open there is nothing
 * for any of them to say. The transport in particular is a fader and a tempo
 * for a piece that is not loaded.
 *
 * The acts are named with their shortcuts, the way the editor's own empty
 * state is, because the buttons are the second place they live: the File menu
 * is the first, and it is still there once a project is open and the splash
 * page has gone. The way back is in that menu too — Close Project, which
 * closes the project and the tabs that belong to it and lands here.
 */
export function SplashPage({
  recents,
  onOpenRecent,
  onNewProject,
  onOpenProject,
}: SplashPageProps) {
  return (
    <div className="flex h-screen flex-col items-center justify-center gap-10 bg-neutral-900 px-6 text-neutral-100">
      <div className="flex flex-col items-center gap-1">
        <h1 className="text-3xl font-semibold tracking-tight">Swync</h1>
        <p className="text-sm text-neutral-500">Open a project to start playing</p>
      </div>

      <div className="flex w-full max-w-md flex-col gap-8">
        <div className="flex flex-col gap-2">
          <button
            onClick={onNewProject}
            className="flex items-center justify-between rounded border border-neutral-700 bg-neutral-800 px-4 py-2.5 text-left text-sm text-neutral-100 hover:bg-neutral-700"
          >
            New Project…
            <span className="font-mono text-xs text-neutral-400">⌘⇧N</span>
          </button>
          <button
            onClick={onOpenProject}
            className="flex items-center justify-between rounded border border-neutral-800 px-4 py-2.5 text-left text-sm text-neutral-300 hover:bg-neutral-800"
          >
            Open Project…
            <span className="font-mono text-xs text-neutral-500">⌘⇧O</span>
          </button>
        </div>

        <div className="flex flex-col gap-2">
          <h2 className="text-xs font-medium uppercase tracking-wide text-neutral-500">
            Recent projects
          </h2>
          {recents.length === 0 ? (
            // A first run, and every run after one where nothing was opened.
            // Said plainly rather than left blank: an empty space under a
            // heading reads as a list that failed to load.
            <p className="text-sm text-neutral-600">No projects yet</p>
          ) : (
            <ul className="flex flex-col">
              {recents.map((project) => (
                <li key={project.path}>
                  <button
                    onClick={() => onOpenRecent(project.path)}
                    // The path is the title as well as the second line: a long
                    // one is truncated to keep the rows a readable width, and
                    // hovering is how the rest of it is read.
                    title={project.path}
                    className="flex w-full flex-col items-start gap-0.5 rounded px-3 py-2 text-left hover:bg-neutral-800"
                  >
                    <span className="text-sm text-neutral-200">{project.name}</span>
                    <span className="w-full truncate font-mono text-xs text-neutral-500">
                      {project.path}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </div>
  );
}
