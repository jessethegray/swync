import type { ReactNode } from "react";
import { ActivityBar, ActivityIcon, PanelTitle, icons } from "./ActivityBar";

/** Which of the panel's views is on top. */
export type SideTab = "problems" | "project" | "search" | "libraries";

/** What each view is called, on its tooltip and in the panel's header. */
const LABELS: Record<SideTab, string> = {
  project: "Project",
  search: "Search",
  libraries: "Libraries",
  problems: "Problems",
};

/**
 * Whether a name off disk is still one of this panel's views.
 *
 * Beside the labels because that is the list: a view that has been renamed or
 * dropped since the layout was written is not an error, and the caller falls
 * back to the view the panel starts on. The same answer `usable_session` gives
 * about a file that is no longer there.
 */
export function isSideTab(name: string): name is SideTab {
  return name in LABELS;
}

interface SidePanelProps {
  open: boolean;
  /** Pixels wide, as the drag handle has left it. */
  width: number;
  /** Grab handle for that drag, drawn along the panel's trailing edge. */
  onResizeStart: (e: React.PointerEvent) => void;
  tab: SideTab;
  /**
   * A tray icon was hit. Whether that opens the panel, changes what it is
   * showing or shuts it is the caller's to decide, since it owns `open`.
   */
  onSelect: (tab: SideTab) => void;
  /** Drawn on the Problems icon, so a failure is visible from anywhere. */
  problemCount: number;
  /** Whether those problems stopped a run, which colours that badge. */
  problemsAreErrors: boolean;
  problems: ReactNode;
  project: ReactNode;
  search: ReactNode;
  libraries: ReactNode;
}

/**
 * The left-hand panel: the project's files, what is in them, and the last
 * run's problems.
 *
 * The tray of icons is drawn whichever way the panel is, so this is a fragment
 * rather than one element — the strip belongs to the window's edge, and only
 * the panel beside it comes and goes.
 *
 * Every view stays mounted and the hidden ones are only taken off screen, so
 * switching views keeps what each was showing — the folders opened in the tree
 * and a set of search results are worth as much as the errors beside them, and
 * rebuilding any of them on every click would be a tax on looking at the rest.
 */
export function SidePanel({
  open,
  width,
  onResizeStart,
  tab,
  onSelect,
  problemCount,
  problemsAreErrors,
  problems,
  project,
  search,
  libraries,
}: SidePanelProps) {
  return (
    <>
      <ActivityBar side="left">
        {(["project", "search", "libraries", "problems"] as const).map((name) => (
          <ActivityIcon
            key={name}
            side="left"
            label={LABELS[name]}
            icon={icons[name]}
            // Nothing is selected while the panel is shut: the marker says
            // which view is up, and none of them is.
            selected={open && tab === name}
            count={name === "problems" ? problemCount : undefined}
            countIsError={problemsAreErrors}
            onClick={() => onSelect(name)}
          />
        ))}
      </ActivityBar>

      <aside
        style={{ width }}
        className={
          "relative shrink-0 flex-col border-r border-neutral-800 bg-neutral-950/40 " +
          (open ? "flex" : "hidden")
        }
      >
        <PanelTitle>{LABELS[tab]}</PanelTitle>

        <div
          className={
            "min-h-0 flex-1 flex-col overflow-y-auto " +
            (tab === "problems" ? "flex" : "hidden")
          }
        >
          {problems}
        </div>
        <div
          className={
            "min-h-0 flex-1 flex-col overflow-y-auto " +
            (tab === "project" ? "flex" : "hidden")
          }
        >
          {project}
        </div>
        <div
          className={
            "min-h-0 flex-1 flex-col overflow-y-auto " +
            (tab === "search" ? "flex" : "hidden")
          }
        >
          {search}
        </div>
        <div
          className={
            "min-h-0 flex-1 flex-col overflow-y-auto " +
            (tab === "libraries" ? "flex" : "hidden")
          }
        >
          {libraries}
        </div>

        {/* Mirrors the transport's handle, on the edge that faces the editor. */}
        <div
          onPointerDown={onResizeStart}
          title="Drag to resize"
          role="separator"
          aria-orientation="vertical"
          className="absolute inset-y-0 -right-1 z-10 w-2 cursor-col-resize hover:bg-blue-400/40 active:bg-blue-600/60"
        />
      </aside>
    </>
  );
}
