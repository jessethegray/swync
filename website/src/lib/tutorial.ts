/**
 * The tutorial, in reading order.
 *
 * Unlike the reference, none of this is generated: the tutorial is prose
 * somebody wrote, and its order is an argument about what to learn first. This
 * table is that order, and the only place it is written down — the sidebar, the
 * index, and the prev/next links at the foot of a page all read it, so adding a
 * chapter is a page under `src/pages/tutorial/` and one entry here.
 */
export interface TutorialPage {
	/** The last segment of the URL: `/tutorial/<slug>/`. */
	slug: string;
	title: string;
	/** A sentence for the index — what the chapter is for, not what it covers. */
	blurb: string;
}

export const TUTORIAL: TutorialPage[] = [
	{
		slug: "installing",
		title: "Installing",
		blurb: "Getting swync onto your machine, downloaded or built from source",
	},
	{
		slug: "getting-started",
		title: "Getting started",
		blurb: "Make a sound, quiet it down, and wrap it in a function you can play",
	},
	{
		slug: "looking-around",
		title: "A look around",
		blurb: "The transport, the panels and the meters — where everything lives",
	},
	{
		slug: "making-noise",
		title: "Making Noise",
		blurb: "Exploring simple synthesis in the language",
	},
	{
		slug: "patterns",
		title: "Patterns",
		blurb: "Let's make some music",
	},
	{
		slug: "notes-basics",
		title: "Note Basics",
		blurb: "How to write different notes different ways",
	},
	{
		slug: "rhythm-basics",
		title: "Rhythm Basics",
		blurb: "I got rhythm, I got proportional representation, who can ask for anything more?",
	},
	{
		slug: "rhythm-notation",
		title: "Rhythmic Notation",
		blurb: "I got more rhythm, and it has western note values, seriously, is there anything more?",
	},
	{
		slug: "rests-drums",
		title: "Rests and Drum Triggers",
		blurb: "Seriously, there's more?",
	},
	{
		slug: "lanes",
		title: "Lanes",
		blurb: "Making sounds that live more interesting lives",
	},
	{
		slug: "lanes-lists",
		title: "Lanes and Lists",
		blurb: "How a lane is counted, and how to hand a parameter a whole list at once",
	},
	{
		slug: "play-arrange",
		title: "Simple arrangment",
		blurb: "Arranging our work into a song",
	},
	{
		slug: "chords",
		title: "Chords and polyphony",
		blurb: "Simultaneity and polyphony within voices",
	},
	{
		slug: "samples",
		title: "Sampling",
		blurb: "Amen",
	},
	{
		slug: "enums",
		title: "Enums",
		blurb: "Giving a group of related values one name each, and comparing them",
	},
	{
		slug: "match",
		title: "Match",
		blurb: "One value, an answer for each thing it might be, and a check that you wrote them all",
	},
	{
		slug: "midi",
		title: "MIDI",
		blurb: "Playing your hardware, and letting your hardware play you",
	},
	{
		slug: "controls",
		title: "Controls",
		blurb: "Sliders and buttons, written into the piece rather than added to a panel",
	},
];

export function pageBySlug(slug: string): TutorialPage | undefined {
	return TUTORIAL.find((p) => p.slug === slug);
}

/** The chapters either side of one, for the links at the foot of a page. */
export function neighbors(slug: string): {
	prev?: TutorialPage;
	next?: TutorialPage;
} {
	const i = TUTORIAL.findIndex((p) => p.slug === slug);
	if (i === -1) return {};
	return { prev: TUTORIAL[i - 1], next: TUTORIAL[i + 1] };
}
