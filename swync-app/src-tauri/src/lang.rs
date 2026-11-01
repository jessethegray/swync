//! The language's callable surface, as data.
//!
//! Names, arities and parameter names used to live in three places at once: the
//! match arm in `lowerer::call`, the arity literals in `lowerer::lists`, and —
//! for parameters — nowhere except the error strings in `swync_graph::realizer`.
//! The editor needs all of it, so it lives here instead, and the lowerer reads
//! it rather than repeating it. Adding a UGen to `UGENS` is now the only step
//! needed to make it callable *and* completable.
//!
//! Parameter names for the pure audio-rate UGens follow fundsp's own `Input N:`
//! documentation; the ones baked in at construction follow the names already
//! used in the realizer's error messages.
//!
//! The `doc` strings are the one part of this file that is not decided here.
//! They are a copy of the body of the matching page under
//! `website/src/content/docs/`, flattened to a line, because the app's docs
//! panel and the site are two renderings of one reference and a reader who has
//! read one has read the other. The prose is written on the page and copied
//! back; a name added here seeds its page from whatever is written below
//! (`website/scripts/sync-reference.mjs`), and after that the page is the
//! source and this is the copy.

use serde::Serialize;

use crate::swync_graph::ugen_nodes::NodeKind;

/// What a name accepts in its first parameter — which is to say, what may be
/// written to the left of the dot that calls it, since `a.f(b)` is `f(a, b)`.
///
/// The editor filters the method-position completions on this. It is declared
/// rather than derived because "what the first parameter accepts" is not
/// readable from the parameter's name: `perc(attack, release)` and
/// `sin(frequency)` both take a number, but only one of them will also take a
/// signal, and only the realizer knows it.
///
/// `every_builtin_receives_what_it_declares` holds this to the truth by
/// compiling each name against a receiver of the kind it claims.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ValueKind {
    /// Nothing at all: the name takes no arguments, so nothing may precede it.
    /// Never a result — every callable name answers with something.
    Nothing,
    /// Not knowable from the table. Only ever a result: `sum` folds to a number
    /// or to a signal depending on what the list holds, and `choice` answers
    /// with whichever element it drew. Treated as "could be anything".
    Any,
    /// A node input — a signal, or a number folded to a constant.
    Signal,
    /// A compile-time number. A signal here is an error, not a modulation.
    Number,
    /// A list.
    List,
    /// A pattern: a list of steps, or a single step's worth of value.
    Pattern,
    /// A handle from `play`, `play_once`, `playn` or `play_all`.
    Play,
    /// A `fn` named rather than called — what the arrangement combinators take
    /// as a section, and what `seq` takes on its left. Never a result: naming a
    /// function is the only way to produce one, so nothing answers with it.
    Section,
    /// A loaded audio file, from `load`.
    Buffer,
    /// A written note value — `q`, `e`, `h`, or a tie or dot built from them.
    /// Only meaningful inside a pattern, where it is how long a step lasts.
    Duration,
    /// A double-quoted string. Only `load` takes one, and only written out —
    /// so nothing ever *answers* with text, and nothing can be chained into a
    /// name that wants it.
    Text,
    /// A speed to run a pattern at, from `accel`. Only `play`'s `rate` accepts
    /// one, and it accepts a plain number there too — so this is what a name
    /// *answers* with rather than something anything receives.
    Rate,
    /// A list marked to be passed whole, from `'` or `list`. Only a `play` lane
    /// takes one, and it takes plain numbers there too — so like `Rate`, this
    /// is what a name answers with rather than something anything receives.
    Lane,
    /// Gear to send notes to, from `midiout`. Only `play`'s instrument slot
    /// accepts one, and it accepts a `fn` there too — so like `Rate` and
    /// `Lane`, this is what a name answers with rather than something anything
    /// receives.
    Destination,
    /// A keyboard to play, from `midiin`. The mirror of `Destination`, in
    /// `play`'s other slot, and only ever a result for the same reason.
    Source,
}

/// A UGen builtin: a name that lowers to a graph node.
///
/// Arity is `params.len()` — the two cannot disagree.
pub struct Ugen {
    pub name: &'static str,
    pub kind: NodeKind,
    pub params: &'static [&'static str],
    pub receives: ValueKind,
    /// What the name answers with, for the next dot in a chain.
    pub returns: ValueKind,
    pub doc: &'static str,
}

/// A list builtin: a name evaluated during lowering that emits no node.
///
/// Unlike UGens these can accept several arg counts, so `arities` is explicit
/// rather than derived. `variadic` means "or anything above the largest listed".
pub struct ListBuiltin {
    pub name: &'static str,
    pub params: &'static [&'static str],
    pub arities: &'static [usize],
    pub variadic: bool,
    pub receives: ValueKind,
    /// What the name answers with, for the next dot in a chain.
    pub returns: ValueKind,
    pub doc: &'static str,
}

/// Words the lexer reserves. `null` is deliberately absent: it is lexed
/// (`parser::lex::Token::Null`) but no parser rule ever consumes it, so
/// offering it as a completion would suggest something unusable.
///
/// `as` is here for the editor's sake rather than the lexer's — it is only a
/// keyword inside a `use`, and reads as one wherever it is written.
pub static KEYWORDS: &[&str] = &["fn", "let", "if", "else", "for", "in", "use", "as", "enum", "match"];

pub static UGENS: &[Ugen] = &[
    Ugen {
        name: "adsr",
        kind: NodeKind::ADSR,
        params: &["gate", "attack", "decay", "sustain", "release"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Gated ADSR envelope. Rises while the gate is positive, releases when it returns to zero. Times are in seconds; sustain is a level in 0..=1.",
    },
    Ugen {
        name: "afollow",
        kind: NodeKind::Afollow,
        params: &["signal", "attack", "release"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Asymmetric parameter follower. Smooths rising segments over `attack` and falling ones over `release` (halfway response times, in seconds).",
    },
    Ugen {
        name: "allpass",
        kind: NodeKind::Allpass,
        params: &["audio", "frequency", "q"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Allpass filter. Passes all frequencies but shifts their phase around the center frequency.",
    },
    Ugen {
        name: "allpole",
        kind: NodeKind::Allpole,
        params: &["audio", "delay"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "First-order allpass filter with a configurable delay at DC, in samples (must be > 0).",
    },
    Ugen {
        name: "bandpass",
        kind: NodeKind::Bandpass,
        params: &["audio", "frequency", "q"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Bandpass filter. Keeps frequencies near the center, attenuating either side.",
    },
    Ugen {
        name: "bandrez",
        kind: NodeKind::Bandrez,
        params: &["audio", "frequency", "q"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Resonant two-pole bandpass filter.",
    },
    Ugen {
        name: "bell",
        kind: NodeKind::Bell,
        params: &["audio", "frequency", "q", "gain"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Bell equalizer. Boosts or cuts a band around the center frequency by `gain` (an amplitude multiplier, not dB).",
    },
    Ugen {
        name: "biquad",
        kind: NodeKind::Biquad,
        params: &["signal", "a1", "a2", "b0", "b1", "b2"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Arbitrary biquad filter with coefficients in normalized form. All five coefficients must be constants.",
    },
    Ugen {
        name: "brown",
        kind: NodeKind::Brown,
        params: &[],
        receives: ValueKind::Nothing,
        returns: ValueKind::Signal,
        doc: "Brown noise: -6 dB per octave. Darker than pink.",
    },
    Ugen {
        name: "butterpass",
        kind: NodeKind::Butterpass,
        params: &["audio", "cutoff"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Second-order Butterworth lowpass. Maximally flat passband, no resonance control.",
    },
    Ugen {
        name: "chorus",
        kind: NodeKind::Chorus,
        params: &["audio", "seed", "separation", "variation", "mod_frequency"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Five-voice mono chorus, mixed with the dry signal. Stack two with different seeds for stereo. All parameters except the audio input are constants.",
    },
    Ugen {
        name: "clip",
        kind: NodeKind::Clip,
        params: &["signal"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Hard-clip the signal to -1..=1.",
    },
    Ugen {
        name: "clip_to",
        kind: NodeKind::ClipTo,
        params: &["signal", "minimum", "maximum"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Hard-clip the signal to `minimum`..=`maximum`. Both bounds are constants.",
    },
    Ugen {
        name: "dcblock",
        kind: NodeKind::Dcblock,
        params: &["signal"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Remove DC offset, keeping the signal zero-centered. Cutoff is 10 Hz.",
    },
    Ugen {
        name: "declick",
        kind: NodeKind::Declick,
        params: &["signal"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Fade the signal in over 10 ms from time zero, suppressing the click at the start of a graph.",
    },
    Ugen {
        name: "delay",
        kind: NodeKind::Delay,
        params: &["signal", "time"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Fixed delay of `time` seconds, rounded to the nearest sample. The time is a constant — use `tap` for a modulatable delay.",
    },
    Ugen {
        name: "dsf_saw",
        kind: NodeKind::DsfSaw,
        params: &["frequency", "roughness"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Saw-like discrete summation formula oscillator. `roughness` in 0..=1 sets how much successive partials are attenuated.",
    },
    // Time-based envelopes. `env` needs the note length, which voices pre-bind
    // as `dur`; `perc` is self-contained.
    Ugen {
        name: "env",
        kind: NodeKind::Env,
        params: &["attack", "decay", "sustain", "release", "duration"],
        receives: ValueKind::Number,
        returns: ValueKind::Signal,
        doc: "Time-based ADSR for one-shot voices. Attack, decay and sustain fill `duration`. The note is held for `duration` and the voice lasts `duration + release`. All arguments are constants.",
    },
    Ugen {
        name: "dsf_square",
        kind: NodeKind::DsfSquare,
        params: &["frequency", "roughness"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Square-like discrete summation formula oscillator. `roughness` in 0..=1 sets how much successive partials are attenuated.",
    },
    Ugen {
        name: "fir3",
        kind: NodeKind::Fir3,
        params: &["signal", "gain"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Three-point symmetric FIR filter, specified by its `gain` (>= 0) at the Nyquist frequency. A gain below 1 gives a gentle lowpass.",
    },
    Ugen {
        name: "follow",
        kind: NodeKind::Follow,
        params: &["signal", "response_time"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Parameter follower. Smooths the signal with the given halfway response time, in seconds.",
    },
    Ugen {
        name: "hammond",
        kind: NodeKind::Hammond,
        params: &["frequency"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Hammond organ wavetable oscillator. Emphasizes the first three partials.",
    },
    Ugen {
        name: "highpass",
        kind: NodeKind::Highpass,
        params: &["audio", "cutoff", "q"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Resonant highpass filter.",
    },
    Ugen {
        name: "highpole",
        kind: NodeKind::Highpole,
        params: &["audio", "cutoff"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "First-order one-pole one-zero highpass. No resonance.",
    },
    Ugen {
        name: "highshelf",
        kind: NodeKind::Highshelf,
        params: &["audio", "frequency", "q", "gain"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "High shelf filter. Scales everything above the center frequency by `gain` (an amplitude multiplier).",
    },
    Ugen {
        name: "hold",
        kind: NodeKind::Hold,
        params: &["signal", "frequency", "variability"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Sample-and-hold. Samples the signal at `frequency` Hz; `variability` in 0..=1 jitters the sampling interval and is a constant.",
    },
    Ugen {
        name: "impulse",
        kind: NodeKind::Impulse,
        params: &[],
        receives: ValueKind::Nothing,
        returns: ValueKind::Signal,
        doc: "A single one followed by silence. Useful for exciting `pluck` or measuring an impulse response.",
    },
    Ugen {
        name: "input",
        kind: NodeKind::Input,
        params: &["channel"],
        receives: ValueKind::Number,
        returns: ValueKind::Signal,
        doc: "One channel of the live audio input, counted from 0. Silence until an input device is chosen in the settings panel, and on any channel the chosen device does not have — so a piece written against an interface still runs on the laptop it is edited on.",
    },
    Ugen {
        name: "limiter",
        kind: NodeKind::Limiter,
        params: &["signal", "attack", "release"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Look-ahead limiter holding the signal to -1..=1. Look-ahead equals the attack time. Times are constants, in seconds.",
    },
    Ugen {
        name: "line",
        kind: NodeKind::Line,
        params: &["start", "end", "duration"],
        receives: ValueKind::Number,
        returns: ValueKind::Signal,
        doc: "One straight segment: `start` to `end` over `duration` seconds, held constant at `end` value after `duration`.",
    },
    Ugen {
        name: "lorenz",
        kind: NodeKind::Lorenz,
        params: &["frequency"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Lorenz chaotic oscillator. The frequency input has only a slight effect on the output.",
    },
    Ugen {
        name: "lowpass",
        kind: NodeKind::Lowpass,
        params: &["audio", "cutoff", "q"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Resonant lowpass filter.",
    },
    Ugen {
        name: "lowpole",
        kind: NodeKind::Lowpole,
        params: &["audio", "cutoff"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "First-order one-pole lowpass. No resonance.",
    },
    Ugen {
        name: "lowrez",
        kind: NodeKind::Lowrez,
        params: &["audio", "cutoff", "q"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Resonant two-pole lowpass filter.",
    },
    Ugen {
        name: "lowshelf",
        kind: NodeKind::Lowshelf,
        params: &["audio", "frequency", "q", "gain"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Low shelf filter. Scales everything below the center frequency by `gain` (an amplitude multiplier).",
    },
    Ugen {
        name: "mls",
        kind: NodeKind::Mls,
        params: &[],
        receives: ValueKind::Nothing,
        returns: ValueKind::Signal,
        doc: "Maximum length sequence noise: a repeating pseudorandom run of -1 and 1.",
    },
    Ugen {
        name: "mls_bits",
        kind: NodeKind::MlsBits,
        params: &["bits"],
        receives: ValueKind::Number,
        returns: ValueKind::Signal,
        doc: "Maximum length sequence noise from an n-bit sequence (1..=31). More bits means a longer period before it repeats. Constant.",
    },
    Ugen {
        name: "moog",
        kind: NodeKind::Moog,
        params: &["signal", "cutoff", "q"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Moog-style resonant lowpass ladder filter.",
    },
    Ugen {
        name: "morph",
        kind: NodeKind::Morph,
        params: &["signal", "frequency", "q", "morph"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Filter that morphs continuously between modes: `morph` runs -1 (lowpass) to 0 (peak) to 1 (highpass).",
    },
    Ugen {
        name: "noise",
        kind: NodeKind::Noise,
        params: &[],
        receives: ValueKind::Nothing,
        returns: ValueKind::Signal,
        doc: "White noise.",
    },
    Ugen {
        name: "notch",
        kind: NodeKind::Notch,
        params: &["audio", "frequency", "q"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Notch filter. Removes a narrow band around the center frequency.",
    },
    Ugen {
        name: "organ",
        kind: NodeKind::Organ,
        params: &["frequency"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Organ wavetable oscillator. Emphasizes octave partials.",
    },
    Ugen {
        name: "peak",
        kind: NodeKind::Peak,
        params: &["audio", "frequency", "q"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Peaking filter.",
    },
    Ugen {
        name: "perc",
        kind: NodeKind::Perc,
        params: &["attack", "release"],
        receives: ValueKind::Number,
        returns: ValueKind::Signal,
        doc: "Self-contained percussive envelope: rise, fall, silence. Both times are constants.",
    },
    Ugen {
        name: "pink",
        kind: NodeKind::Pink,
        params: &[],
        receives: ValueKind::Nothing,
        returns: ValueKind::Signal,
        doc: "Pink noise: -3 dB per octave.",
    },
    Ugen {
        name: "pinkpass",
        kind: NodeKind::Pinkpass,
        params: &["signal"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Pinking filter: -3 dB per octave. Turns white noise into pink.",
    },
    Ugen {
        name: "pluck",
        kind: NodeKind::Pluck,
        params: &["excitation", "frequency", "gain_per_second", "damping"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Karplus-Strong plucked string. Feed it a burst — `impulse()` or a short noise envelope — as the excitation. Frequency, gain and damping (0..=1) are constants.",
    },
    Ugen {
        name: "poly_pulse",
        kind: NodeKind::PolyPulse,
        params: &["frequency", "width"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "PolyBLEP pulse wave. Fast and fairly bandlimited; `width` in 0..=1 is the duty cycle.",
    },
    Ugen {
        name: "poly_saw",
        kind: NodeKind::PolySaw,
        params: &["frequency"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "PolyBLEP saw wave. Fast and fairly bandlimited.",
    },
    Ugen {
        name: "poly_square",
        kind: NodeKind::PolySquare,
        params: &["frequency"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "PolyBLEP square wave. Fast and fairly bandlimited.",
    },
    Ugen {
        name: "pulse",
        kind: NodeKind::Pulse,
        params: &["frequency", "width"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Bandlimited pulse wave oscillator. `width` in 0..=1 is the duty cycle.",
    },
    Ugen {
        name: "ramp",
        kind: NodeKind::Ramp,
        params: &["frequency"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Rising ramp from 0 to 1 at the given repetition frequency, starting at 0. Not bandlimited — useful as a phasor, not as audio.",
    },
    Ugen {
        name: "resonator",
        kind: NodeKind::Resonator,
        params: &["audio", "frequency", "q"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Constant-gain bandpass resonator.",
    },
    // fundsp's reverbs are all stereo. swync's graph is mono end to end, so
    // each one is wrapped: the signal feeds both inputs and the two outputs are
    // averaged back down. They are wet-only — mix the dry signal yourself.
    Ugen {
        name: "reverb",
        kind: NodeKind::Reverb,
        params: &["audio", "room_size", "time", "damping"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Reverb (32-channel FDN). `room_size` is in meters (10 is an average room), `time` is the decay to -60 dB in seconds, `damping` in 0..=1 rolls off the highs. Wet only: `x + reverb(x, 10, 3, 0.5) * 0.2`. All parameters except the audio input are constants.",
    },
    Ugen {
        name: "reverb2",
        kind: NodeKind::Reverb2,
        params: &["audio", "room_size", "time", "diffusion", "modulation", "damping_cutoff"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Hybrid FDN reverb — richer and more expensive than `reverb`. `room_size` is in meters and clamps to 10..=30, `diffusion` in 0..=1 thickens the tail, `modulation` around 1 adds movement (higher goes audibly Doppler), and `damping_cutoff` is the lowpass applied to each loop pass, in hertz. Wet only. All parameters except the audio input are constants.",
    },
    Ugen {
        name: "reverb3",
        kind: NodeKind::Reverb3,
        params: &["audio", "time", "diffusion", "damping_cutoff"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Allpass-loop reverb, with no room size — just `time` to -60 dB, `diffusion` in 0..=1, and a `damping_cutoff` in hertz applied to each loop pass. Wet only. All parameters except the audio input are constants.",
    },
    Ugen {
        name: "reverb4",
        kind: NodeKind::Reverb4,
        params: &["audio", "room_size", "time"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Reverb with a slow fade-in, for swells rather than rooms. `room_size` is in meters and is treated as at least 15; below that the delay times stop sounding like a space. Wet only. Both `room_size` and `time` are constants.",
    },
    Ugen {
        name: "rossler",
        kind: NodeKind::Rossler,
        params: &["frequency"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Rossler chaotic oscillator, with peaks at multiples of the frequency input.",
    },
    Ugen {
        name: "saw",
        kind: NodeKind::Saw,
        params: &["frequency"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Bandlimited saw wavetable oscillator.",
    },
    Ugen {
        name: "sin",
        kind: NodeKind::Sin,
        params: &["frequency"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Sine oscillator.",
    },
    Ugen {
        name: "soft_saw",
        kind: NodeKind::SoftSaw,
        params: &["frequency"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Soft saw wavetable oscillator. Contains all partials but falls off like a triangle wave.",
    },
    Ugen {
        name: "square",
        kind: NodeKind::Square,
        params: &["frequency"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Bandlimited square wavetable oscillator.",
    },
    Ugen {
        name: "tap",
        kind: NodeKind::Tap,
        params: &["signal", "delay", "min_delay", "max_delay"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Tapped delay line with cubic interpolation. Unlike `delay`, the delay time is a signal, so it can be modulated. It must stay within the constant `min_delay`..=`max_delay` bounds, in seconds.",
    },
    Ugen {
        name: "tick",
        kind: NodeKind::Tick,
        params: &["signal"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Single-sample delay. The building block for feedback and comb filters.",
    },
    Ugen {
        name: "triangle",
        kind: NodeKind::Triangle,
        params: &["frequency"],
        receives: ValueKind::Signal,
        returns: ValueKind::Signal,
        doc: "Bandlimited triangle wavetable oscillator.",
    },
];

pub static LIST_BUILTINS: &[ListBuiltin] = &[
    ListBuiltin {
        name: "list",
        params: &["number"],
        arities: &[1],
        variadic: true,
        receives: ValueKind::Number,
        returns: ValueKind::Lane,
        doc: "Marks numbers as one value rather than a sequence. A lane is read by note — the nth note takes the nth value, wrapping — so `div: [2, 3]` gives the first note 2 and the second 3, where `div: list(2, 3)` gives every note both. `'[2, 3]` is the same value written short. Only a lane reads one.",
    },
    ListBuiltin {
        name: "len",
        params: &["list"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::Number,
        doc: "The number of elements in the list.",
    },
    ListBuiltin {
        name: "zip",
        params: &["list"],
        arities: &[1],
        variadic: true,
        receives: ValueKind::List,
        returns: ValueKind::List,
        doc: "Pair elements positionally: `zip([1, 2], [3, 4])` is `[[1, 3], [2, 4]]`. Every argument must be a list and all must be the same length.",
    },
    ListBuiltin {
        name: "stack",
        params: &["pattern"],
        arities: &[1],
        variadic: true,
        receives: ValueKind::Pattern,
        returns: ValueKind::Pattern,
        doc: "Creates patterns that sound at once instead of in turn: `stack([c4;2, e4;2], [g4;4])`. Each argument keeps its own division, so the layers need not agree on a rhythm. Creates chords and polyphony.",
    },
    ListBuiltin {
        name: "rev",
        params: &["list"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::List,
        doc: "Reverse a list",
    },
    ListBuiltin {
        name: "palindrome",
        params: &["list"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::List,
        doc: "The list followed by its mirror: `[a, b, c]` becomes `[a, b, c, c, b, a]`.",
    },
    ListBuiltin {
        name: "rotl",
        params: &["list", "amount"],
        arities: &[1, 2],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::List,
        doc: "Rotate left, wrapping. `amount` defaults to 1; a negative amount rotates right.",
    },
    ListBuiltin {
        name: "rotr",
        params: &["list", "amount"],
        arities: &[1, 2],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::List,
        doc: "Rotate right, wrapping. `amount` defaults to 1; a negative amount rotates left.",
    },
    ListBuiltin {
        name: "push",
        params: &["list", "value"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::List,
        doc: "A new list with `value` appended.",
    },
    ListBuiltin {
        name: "pop",
        params: &["list"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::List,
        doc: "A new list without its last element.",
    },
    ListBuiltin {
        name: "sort",
        params: &["list"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::List,
        doc: "The list sorted ascending.",
    },
    ListBuiltin {
        name: "sum",
        params: &["list"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::Any,
        doc: "Add every element together.",
    },
    ListBuiltin {
        name: "split",
        params: &["list", "size"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::List,
        doc: "Break the list into chunks of `size`.",
    },
    ListBuiltin {
        name: "choice",
        params: &["list"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::Any,
        doc: "One element picked at random. Re-rolled on every eval.",
    },
    ListBuiltin {
        name: "wchoice",
        params: &["values", "weights"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::Any,
        doc: "Weighted random choice The two lists run in parallel; weights must be finite and >= 0.",
    },
    ListBuiltin {
        name: "scramble",
        params: &["list"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::List,
        doc: "The list shuffled. Re-rolled on every eval.",
    },
    ListBuiltin {
        name: "map",
        params: &["list", "transform"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::List,
        doc: "Apply a function to every element: `riff.map(up)`.",
    },
    ListBuiltin {
        name: "dot",
        params: &["value", "dots"],
        arities: &[1, 2],
        variadic: false,
        receives: ValueKind::Duration,
        returns: ValueKind::Duration,
        doc: "Dot a written note value: `q.dot` is a dotted quarter, a quarter and an eighth. `dots` defaults to 1; `q.dot(2)` is the doubly-dotted quarter. Can be chained as well `q.dot.dot`.",
    },
    ListBuiltin {
        name: "filter",
        params: &["list", "predicate"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::List,
        doc: "Filter a list based on the predicate.",
    },
];

/// Compile-time arithmetic on numbers. Shares `ListBuiltin`'s shape because
/// the two are the same kind of thing — a name evaluated during lowering that
/// emits no node — and they differ only in what they accept.
///
/// These read naturally through the dot operator: `60.m2h`.
pub static MATH_BUILTINS: &[ListBuiltin] = &[
    // --- conversions ---
    ListBuiltin {
        name: "m2h",
        params: &["note"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "MIDI note number to frequency in hertz: `69.m2h` is 440. Equal temperament.",
    },
    ListBuiltin {
        name: "h2m",
        params: &["hz"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Frequency in hertz to MIDI note number, the inverse of `m2h`: `440.h2m` is 69. The result may be fractional.",
    },
    ListBuiltin {
        name: "db",
        params: &["decibels"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Decibels to a linear amplitude: `0.db` is 1, `-6.db` is about 0.5.",
    },
    ListBuiltin {
        name: "amp",
        params: &["amplitude"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Linear amplitude to decibels, the inverse of `db`: `1.amp` is 0. The amplitude must be above zero.",
    },
    ListBuiltin {
        name: "cents",
        params: &["hz", "cents"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Detune a frequency by cents, 1/100 of a semitone: `440.cents(-14)` flattens A4 slightly.",
    },
    ListBuiltin {
        name: "bpm",
        params: &["beats"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Beats per minute to bars per second.",
    },
    // --- pitch arithmetic ---
    //
    // These three take a list where the rest of this table takes a number, and
    // they declare it as `Pattern` — the accept-set being claimed rather than
    // the musical reading, since a note or a list of them is exactly what a
    // pattern already means here. `Any` back, because the result follows the
    // subject: a note in, a note out; a list in, a list out. The others stay
    // `Number`, which is not an oversight — `60.round` is arithmetic that
    // happens to be done on a note, while these three are the operations whose
    // subject is as naturally a riff as a single pitch.
    ListBuiltin {
        name: "oct",
        params: &["note", "octaves"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Pattern,
        returns: ValueKind::Any,
        doc: "Transpose a MIDI note by whole octaves: `60.oct(-1)` is 48. A list is transposed note by note, keeping each one's `;` length and leaving rests alone — `[c4;q, ef, g].oct(-1)`. Nested lists move with it, so a subdivision group transposes as a group.",
    },
    ListBuiltin {
        name: "semi",
        params: &["note", "semitones"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Pattern,
        returns: ValueKind::Any,
        doc: "Transpose a MIDI note by semitones: `60.semi(7)` is 67. A list is transposed note by note, keeping each one's `;` length and leaving rests alone — `[c4;q, ef, g].semi(7)`. Nested lists move with it, so a subdivision group transposes as a group.",
    },
    ListBuiltin {
        name: "scale",
        params: &["note", "scale"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Pattern,
        returns: ValueKind::Any,
        doc: "Snap a MIDI note to the nearest tone of a scale, given as semitone offsets within an octave: `61.scale([0, 2, 4, 5, 7, 9, 11])` is 60. A list is snapped note by note, keeping each one's `;` length and leaving rests alone, so a whole riff can be bent into a scale at once.",
    },
    // --- ranges and shaping ---
    ListBuiltin {
        name: "clamp",
        params: &["x", "lo", "hi"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Constrain a number to `lo..=hi`.",
    },
    ListBuiltin {
        name: "norm",
        params: &["x", "lo", "hi"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Map 0..1 onto `lo..hi`. Values outside 0..1 extrapolate; `clamp` first if you do not want that.",
    },
    ListBuiltin {
        name: "wrap",
        params: &["x", "lo", "hi"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Fold a number back into `lo..hi`, wrapping around rather than clamping. Useful for modular pitch.",
    },
    ListBuiltin {
        name: "round",
        params: &["x"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Nearest whole number, halves away from zero.",
    },
    ListBuiltin {
        name: "floor",
        params: &["x"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Largest whole number at or below `x`.",
    },
    ListBuiltin {
        name: "ceil",
        params: &["x"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Smallest whole number at or above `x`.",
    },
    ListBuiltin {
        name: "abs",
        params: &["x"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Magnitude, dropping the sign.",
    },
    ListBuiltin {
        name: "pow",
        params: &["x", "exponent"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "`x` raised to a power: `2.pow(10)` is 1024.",
    },
    ListBuiltin {
        name: "sqrt",
        params: &["x"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Square root. `x` must not be negative.",
    },
    ListBuiltin {
        name: "log2",
        params: &["x"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Base-2 logarithm. `x` must be above zero.",
    },
];

/// Random number generators, drawn while the program is lowered.
///
/// Same shape as the math builtins, and the same story: they fold to a value
/// during lowering and emit no node. Which is what makes them useful — a draw
/// becomes a constant in the graph, so a pattern built from one is a *fixed*
/// riff for that evaluation rather than something that squirms while it plays.
/// Re-evaluating rolls again.
///
/// The one exception falls out of how voices work: an instrument body is
/// lowered afresh for every note, so a draw written *inside* a `fn` used as an
/// instrument is a new number on each note. That is the difference between
/// `rands(8, 60, 72) >> play(lead)` — eight notes, settled — and a `lead` whose
/// own body says `rand(0, 1)`, which wanders note to note.
///
/// Audio-rate randomness is a different thing entirely and already has names:
/// `noise`, `pink`, `brown`, `mls`, and `hold` to step them.
pub static RANDOM_BUILTINS: &[ListBuiltin] = &[
    // --- uniform ---
    ListBuiltin {
        name: "rand",
        params: &["lo", "hi"],
        arities: &[0, 2],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "A uniform number. `rand()` draws from 0..1; `rand(lo, hi)` — or `60.rand(72)` — draws from `lo..hi`, with `hi` excluded.",
    },
    ListBuiltin {
        name: "randi",
        params: &["lo", "hi"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "A uniform whole number in `lo..hi`, with `hi` excluded. Both bounds must be whole.",
    },
    ListBuiltin {
        name: "coin",
        params: &["probability"],
        arities: &[0, 1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "1 or 0, `coin()` is even 50/50 probability. `coin(0.25)` is 1 one time in four.",
    },
    ListBuiltin {
        name: "seed",
        params: &["seed"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Use a fixed seed for all random values after it is set. A voice is lowered per note and does not see a top-level seed, so a `seed` inside an instrument pins that instrument identically on every note.",
    },
    // --- shaped ---
    ListBuiltin {
        name: "gauss",
        params: &["mean", "deviation"],
        arities: &[0, 1, 2],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "A normally distributed number: clustered around `mean`, two thirds of it within one `deviation`. Both default to the standard 0 and 1. Unbounded.",
    },
    ListBuiltin {
        name: "humanize",
        params: &["x", "amount"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Nudge a number off its grid: `x` plus random `amount`.",
    },
    ListBuiltin {
        name: "expo",
        params: &["mean"],
        arities: &[0, 1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "An exponentially distributed number above zero with the given `mean` (default 1).",
    },
    ListBuiltin {
        name: "tri",
        params: &["lo", "hi", "mode"],
        arities: &[2, 3],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "A triangular draw over `lo..hi`, peaking at `mode`.",
    },
    ListBuiltin {
        name: "cauchy",
        params: &["median", "spread"],
        arities: &[0, 1, 2],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "A heavy-tailed draw around `median` (default 0) with the given `spread` (default 1).",
    },
    ListBuiltin {
        name: "pareto",
        params: &["scale", "shape"],
        arities: &[0, 1, 2],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "A power-law draw at or above `scale` (default 1). A smaller `shape` (default 1) makes big values likelier.",
    },
    ListBuiltin {
        name: "poisson",
        params: &["mean"],
        arities: &[0, 1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "A whole count averaging `mean` (default 1).",
    },
    // --- lists ---
    ListBuiltin {
        name: "rands",
        params: &["count", "lo", "hi"],
        arities: &[1, 3],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::List,
        doc: "A list of `count` uniform numbers, from 0..1 or from `lo..hi`.",
    },
    ListBuiltin {
        name: "randis",
        params: &["count", "lo", "hi"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::List,
        doc: "A list of `count` whole numbers in `lo..hi`, `hi` excluded.",
    },
    ListBuiltin {
        name: "walk",
        params: &["count", "start", "step"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::List,
        doc: "A random walk. `Count` numbers beginning at `start`, each drifting from the one before by up to `step` either way.",
    },
    ListBuiltin {
        name: "walki",
        params: &["count", "start", "step"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::List,
        doc: "`walk` in whole steps: each move is a whole number from `-step` to `step`, so the result stays on the semitone grid. `walki(16, 60, 2)` wanders a melody around middle C. `step` must be whole.",
    },
    ListBuiltin {
        name: "choices",
        params: &["list", "count"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::List,
        returns: ValueKind::List,
        doc: "`count` elements chosen. Does not repeat. Asking for the whole list is equivalent to `scramble`. > len is an error.",
    },
    ListBuiltin {
        name: "randscale",
        params: &["count", "scale", "lo", "hi"],
        arities: &[2, 4],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::List,
        doc: "`count` notes drawn evenly from the tones of a scale given as semitone offsets within an octave between MIDI `lo` and `hi` (60..72 by default). Unlike snapping a uniform draw with `scale`, every degree is equally likely and nothing lands outside the range: `randscale(8, [0, 3, 5, 7, 10], 48, 72)` is a pentatonic riff over two octaves.",
    },
];

/// Names that exist but belong to neither table: `play` is intercepted before
/// evaluation because it needs its instrument argument syntactically, `dur`
/// is not a function at all — it is the note length, bound only inside a voice
/// — and `accel` answers with a kind of value nothing else in the language
/// produces or takes.
pub static SPECIALS: &[ListBuiltin] = &[
    ListBuiltin {
        name: "then",
        params: &["play", "section"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "Sequence one section after another: `playn(verse, lead, 4).then(chorus)`.",
    },
    ListBuiltin {
        name: "then_after",
        params: &["play", "bars", "section"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "`then`, with `bars` of silence in between the previous play: `playn(verse, lead, 4).then_after(1, chorus)` leaves a bar's rest before the chorus. The gap cannot be negative.",
    },
    ListBuiltin {
        name: "then_afters",
        params: &["play", "seconds", "section"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "`then_after`, in seconds instead of bars: `playn(verse, lead, 4).then_afters(1.5, chorus)`. The seconds are converted to bars at the tempo the program was played at, so changing the tempo afterwards stretches the gap along with the music. The gap cannot be negative.",
    },
    ListBuiltin {
        name: "then_afterd",
        params: &["play", "value", "section"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "`then_after`, in a written note value instead of bars: `playn(verse, lead, 4).then_afterd(h, chorus)` leaves half a bar's rest in 4/4. Takes the values a pattern is written in — `w`, `h`, `q`, `e`, `s`, dotted with `q.dot`, added with `h + e` — and how much of a bar one is follows the time signature. A plain number is refused: write `then_after` for bars or `then_afters` for seconds.",
    },
    ListBuiltin {
        name: "overlap",
        params: &["play", "bars", "section"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "`then`, but the section starts `bars` *before* the first one ends. Two functions will sound together over the join: `playn(verse, lead, 8).overlap(2, chorus)`. The chain carries on from whichever of the two ends later.",
    },
    ListBuiltin {
        name: "with",
        params: &["play", "section"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "Run a section *alongside* the previous play in the chain: `playn(verse, lead, 4).with(drums).then(chorus)`. The pair finishes when the later of them does.",
    },
    ListBuiltin {
        name: "at",
        params: &["bar", "section"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Play,
        doc: "Place a section at an absolute bar, counted from the origin: `at(8, chorus)`. Bars are counted from 0, so `at(8, …)` is the ninth bar",
    },
    ListBuiltin {
        name: "seq",
        params: &["section"],
        arities: &[1],
        variadic: true,
        receives: ValueKind::Section,
        returns: ValueKind::Play,
        doc: "Plays plays one after another without the nesting: `seq(intro, verse, chorus, verse)`. Only plays the next when the previous pattern completes.",
    },
    ListBuiltin {
        name: "then_n",
        params: &["play", "section", "times"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "Run a section n `times`. `play_once(intro, lead).then_n(verse, 4)`.",
    },
    ListBuiltin {
        name: "loop",
        params: &["play", "times"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "Loops the entire chain prior to its call n `times`. `playn(groove, kit, 3).then_fill(roll).loop(4)` is four bars of groove-and-fill.",
    },
    ListBuiltin {
        name: "then_each",
        params: &["play", "list", "body"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "One pass of the section per element of the list, in sequence, with the element from the list passed in: `play_once(intro, lead).then_each([1, 2, 4], faster)` calls `faster(1)`, then `faster(2)`, then `faster(4)`.",
    },
    ListBuiltin {
        name: "then_fill",
        params: &["play", "pattern", "rate"],
        arities: &[2, 3],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "A fill is played by the play it chains from: `playn(groove, drums, 4).then_fill([1, 1, 1, 1])`. The instrument and every lane are inherited and only the pattern is new.",
    },
    ListBuiltin {
        name: "quantize",
        params: &["play", "grid"],
        arities: &[1, 2],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "Round where a chain of plays has reached up to a multiple of `grid` bars, without touching what is already playing: `playn(pat, inst, 3, 2).quantize().then(chorus)`. `grid` defaults to 1. Useful when a section's length is not a whole number of bars.",
    },
    ListBuiltin {
        name: "wait",
        params: &["play", "bars"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "Hold a chain open for `bars` of silence before anything follows: `playn(verse, lead, 4).wait(2).then(chorus)`. What is already playing is untouched — only the next section moves, the same way `quantize` rounds the join without shortening the notes. `.then_after(2, chorus)` says the same thing about one join; `wait` is a link in the chain, so `.loop` repeats the gap with the notes. Cannot be negative.",
    },
    ListBuiltin {
        name: "waits",
        params: &["play", "seconds"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "`wait`, in seconds instead of bars: `playn(verse, lead, 4).waits(1.5)`. The seconds are converted to bars at the tempo the program was played at, so changing the tempo afterwards stretches the gap along with the music. Cannot be negative.",
    },
    ListBuiltin {
        name: "waitd",
        params: &["play", "value"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "`wait`, in a written note value instead of bars: `playn(verse, lead, 4).waitd(h)` waits half a bar in 4/4. Takes the values a pattern is written in — `w`, `h`, `q`, `e`, `s`, dotted with `q.dot`, added with `h + e` — and how much of a bar one is follows the time signature. A plain number is refused: write `wait` for bars or `waits` for seconds.",
    },
    ListBuiltin {
        name: "take",
        params: &["play", "bars"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "Interrupts a play after n number of bars, `play(riff, lead).take(8).then(chorus)`.",
    },
    ListBuiltin {
        name: "stop",
        params: &["play"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "Stops a play.",
    },
    ListBuiltin {
        name: "wthen",
        params: &["play", "sections", "weights"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "Weighted choice between sections each iteration: `playn(intro, lead, 2).wthen([verse, chorus], [0.7, 0.3])`. Weights are relative and need not sum to 1.",
    },
    ListBuiltin {
        name: "rthen",
        params: &["play", "sections"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "Random choice without weighting: `playn(intro, lead, 2).rthen([verse, chorus, bridge])`. Rerolls each iteration.",
    },
    ListBuiltin {
        name: "maybe",
        params: &["play", "chance", "section"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "Play a section with probability `chance` (0 to 1). Maybe decides each iteration: `playn(groove, drums, 4).maybe(0.25, fill)`.",
    },
    ListBuiltin {
        name: "shuffle_then",
        params: &["play", "sections"],
        arities: &[2],
        variadic: false,
        receives: ValueKind::Play,
        returns: ValueKind::Play,
        doc: "Plays every section in a randomized order: `play_once(intro, lead).shuffle_then([verse, chorus, bridge])`. It is evaluated like `scramble`.",
    },
    ListBuiltin {
        name: "play",
        params: &["pattern", "instrument", "rate"],
        arities: &[2, 3],
        variadic: false,
        receives: ValueKind::Pattern,
        returns: ValueKind::Play,
        doc: "Schedule a pattern on an instrument: `pat >> play(kick)`. The instrument must name a user `fn`. `rate` defaults to 1. Any further function parameter is added as a lane — `play(bass, cut: [400, 2000])` lane's values are sampled at each note's onset. Two names are reserved and reach the note rather than the instrument: `legato:` scales its length, and `pan:` places it across the stereo field from -1 (left) through 0 (centre) to 1 (right). All children of `play` follow the same conventions.",
    },
    ListBuiltin {
        name: "play_once",
        params: &["pattern", "instrument", "rate"],
        arities: &[2, 3],
        variadic: false,
        receives: ValueKind::Pattern,
        returns: ValueKind::Play,
        doc: "`play`, stopping after one pass of the pattern: `[60, 64, 67] >> play_once(stab)`.",
    },
    ListBuiltin {
        name: "playn",
        params: &["pattern", "instrument", "times", "rate"],
        arities: &[3, 4],
        variadic: false,
        receives: ValueKind::Pattern,
        returns: ValueKind::Play,
        doc: "`play`, stopping after `times` passes of the pattern: `playn([220, 330], bass, 4)`. Lanes work as they do on `play`.",
    },
    ListBuiltin {
        name: "play_all",
        params: &["section"],
        arities: &[1],
        variadic: true,
        receives: ValueKind::Section,
        returns: ValueKind::Play,
        doc: "Executes all plays simultaneously. `play_all(verse, bassline).then(chorus)`.",
    },
    ListBuiltin {
        name: "accel",
        params: &["from", "to", "bars"],
        arities: &[3],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Rate,
        doc: "Accelerates a play's rate `from` to `to` over `bars` bars. Holds at `to` upon completion. Use in the rate parameter of `play` — `play(riff, lead, accel(1, 2, 4))`. A `to` less than from `from` slows down — `play(riff, lead, accel(2, 1, 4))` Both rates must be above zero: at zero the pattern would stop rather than slow.",
    },
    ListBuiltin {
        name: "dur",
        params: &[],
        arities: &[],
        variadic: false,
        receives: ValueKind::Nothing,
        returns: ValueKind::Number,
        doc: "The current note's length in seconds. Passed to a function used in a `play` scoped to the function.",
    },
    ListBuiltin {
        name: "qvs",
        params: &[],
        arities: &[],
        variadic: false,
        receives: ValueKind::Nothing,
        returns: ValueKind::Number,
        doc: "The quarter note in seconds. Use with lengths: `perc(0, qvs / 2)` or `delay(qvs * 0.75)`",
    },
    ListBuiltin {
        name: "qvh",
        params: &[],
        arities: &[],
        variadic: false,
        receives: ValueKind::Nothing,
        returns: ValueKind::Number,
        doc: "The quarter note in hertz — `1 / qvs`. Use in oscillators: `sin(qvh * 2)`",
    },
    ListBuiltin {
        name: "load",
        params: &["path"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Text,
        returns: ValueKind::Buffer,
        doc: "Read an audio file into a buffer: `let amen = load(\"breaks/amen.wav\")`. The path is relative to the file it is written in. Any format symphonia reads: wav, mp3, flac, ogg.",
    },
    ListBuiltin {
        name: "midiin",
        params: &["device", "channel"],
        arities: &[1, 2],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Source,
        doc: "A keyboard to play, in place of a pattern: `play(midiin(\"keystation\"), lead)`, or `midiin(\"keystation\").play(lead)`. The device is a port's name — matched on any part of it, ignoring case — or its number in the settings panel's list. `channel` is 1-16 and defaults to every channel, which is what a keyboard on its own means. A note arrives as its MIDI note number, the same thing a pattern step carries. An instrument that declares a `vel` parameter is given the velocity, 0 to 1. A keyboard has no length, so it takes neither a rate nor `play_once`/`playn`.",
    },
    ListBuiltin {
        name: "midiclock",
        params: &["device"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Number,
        doc: "Send the transport's clock to a port, so a synth's arps and delays line up with the part written for it: `midiclock(\"deluge\")`. Twenty-four ticks to the quarter note, plus start and stop with the transport. The device is a port's name — matched on any part of it, ignoring case — or its number. Following somebody else's clock is the other way round and is not this: it is in the settings panel under MIDI, because whether you are the slave tonight is about the rig rather than the piece.",
    },
    ListBuiltin {
        name: "cc",
        params: &["device", "number", "channel", "lo", "hi"],
        arities: &[2, 3, 5],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Signal,
        doc: "One controller — a knob, a wheel, a pedal — as a signal: `cc(\"push\", 74)` is 0 to 1. `channel` is 1-16 and defaults to 1. Give a range to map it straight onto something: `cc(\"push\", 74, 1, 200, 5000)` is a filter cutoff. Smoothed over a few milliseconds, because seven bits wired to a cutoff zippers. Zero until the controller first moves, and silent when the port is not connected.",
    },
    ListBuiltin {
        name: "bend",
        params: &["device", "channel", "lo", "hi"],
        arities: &[1, 2, 4],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Signal,
        doc: "The pitch wheel as a signal, -1 to 1 with the wheel at rest reading 0 — so `note + bend(\"keys\", 1, -2, 2)` bends two semitones either way. Give a range to change that. Smoothed like `cc`.",
    },
    ListBuiltin {
        name: "aftertouch",
        params: &["device", "channel", "lo", "hi"],
        arities: &[1, 2, 4],
        variadic: false,
        receives: ValueKind::Number,
        returns: ValueKind::Signal,
        doc: "Channel pressure — how hard the keys are being leant on — as a signal, 0 to 1. Give a range to map it. Smoothed like `cc`. Zero on a keyboard that does not send it.",
    },
    ListBuiltin {
        name: "slider",
        params: &["name", "lo", "hi", "start"],
        arities: &[1, 3, 4],
        variadic: false,
        // The name is written in quotes and read off the syntax, exactly as a
        // `load` path is — so like `load`, nothing can stand to the left of
        // this dot.
        receives: ValueKind::Text,
        // Both, and which one depends on where it is written: the node in the
        // graph wherever a signal goes, and the position it is standing at
        // wherever the language can only take a number. `sum` and `choice` are
        // `Any` for the same reason.
        returns: ValueKind::Any,
        doc: "A control in the panel, named where it is used: `lowpass(saw(110), slider(\"cutoff\", 200, 5000), 1)` draws a slider called cutoff in the controls panel and reads it at audio rate, so dragging it is heard immediately with nothing recompiled. The range defaults to 0 to 1, and a fourth number says where it starts — otherwise it starts at the bottom. The name is what labels the control and what makes it the same control after an edit, so writing one name in two places is one slider moving both. Those two places need not agree on a range: what a slider holds is a position along its travel, and each place maps it into the range written there — at halfway `slider(\"cutoff\", 200, 5000)` reads 2600 while `slider(\"cutoff\", 0, 1)` reads 0.5, one hand moving both across their own ends. The panel draws the range declared first. A slider keeps its position across an evaluation and forgets it when the app quits, which is why the number written here is a starting point rather than a value. Smoothed over a few milliseconds, because a drag wired straight to a cutoff zippers. Written where the language wants a compile-time number — a `;` length, a pattern step, a delay time — it is the position it stood at when the program compiled; the panel marks those, and letting go of one runs the program again.",
    },
    ListBuiltin {
        name: "toggle",
        params: &["name", "start"],
        arities: &[1, 2],
        variadic: false,
        // The name is written in quotes and read off the syntax, as `slider`'s
        // is — so like `slider`, nothing can stand to the left of this dot.
        receives: ValueKind::Text,
        // Both, for the reason a `slider` is both: the node wherever a signal
        // goes, and the 0 or 1 it stands at wherever only a number will do.
        returns: ValueKind::Any,
        doc: "A switch in the panel, off or on, named where it is used: `sin(220) * toggle(\"mute\")` draws a toggle called mute and reads it at audio rate, so flipping it is heard immediately. It is 0 or 1, which makes multiplying by it the ordinary use — a part that is in or out without a number to choose. A second argument says which end it starts at: `toggle(\"lead\", 1)` starts on. Everything else is a `slider`: one name is one control wherever it is written, it keeps its state across an evaluation and forgets it when the app quits, and its ends are smoothed over a few milliseconds so switching something in does not click.",
    },
    ListBuiltin {
        name: "trigger",
        params: &["name", "section"],
        arities: &[2],
        variadic: false,
        // The name is written in quotes and read off the syntax, as the other
        // controls' are — so nothing can stand to the left of this dot.
        receives: ValueKind::Text,
        // A statement about the panel, like `midiclock` is one about a port:
        // writing it is the whole of what it says. It answers with a number
        // for the reason `midiclock` does — every callable name answers with
        // something, and zero is the least it can mean.
        returns: ValueKind::Number,
        doc: "A button in the panel that starts a section when pressed: `trigger(\"fill\", fill)` draws a button called fill and plays `fill` each time it is hit. The section is a `fn` named rather than called, the same thing `.then` takes, and how long it lasts is what it says — a `play_once` or `playn` inside it is a one-shot, a plain `play` keeps going. It is lowered when the program runs, not when the button is pressed, so a press compiles nothing and can fail at nothing. A press starts the section on the next beat, and pressing again restarts it from the top. Nothing else stops: a fired section plays over what is already going, and its own notes are the only ones it decides.",
    },
    ListBuiltin {
        name: "midiout",
        params: &["device", "channel"],
        arities: &[1, 2],
        variadic: false,
        // A port may be named with text, but nothing in the language ever
        // *answers* with text, so a number is the whole truth about what can
        // stand to the left of this dot.
        receives: ValueKind::Number,
        returns: ValueKind::Destination,
        doc: "Gear to send a pattern to, in place of an instrument: `play(bass, midiout(\"deluge\"), vel: [1, .6])`. The device is a port's name — matched on any part of it, ignoring case — or its number in the settings panel's list. `channel` is 1-16 and defaults to 1. A pattern's values are MIDI note numbers, which is what `c4`, `semi` and `scale` already count in. Lanes: `vel` is 0 to 1, `chan` overrides the channel per note, and `legato` sets how long the note is held. A port that is not connected is a warning rather than an error, so a piece written for a rack still runs on the laptop it is edited on.",
    },
    ListBuiltin {
        name: "sample",
        params: &["buffer", "position", "channel"],
        arities: &[2, 3],
        variadic: false,
        receives: ValueKind::Buffer,
        returns: ValueKind::Signal,
        doc: "Read a buffer at a position: 0 is the start, 1 is the end. Anything outside that is silence. `position` is a signal: `sample(b, ramp(1 / b.secs))` -- plays forwards, `1 - ramp(...)` backwards. `channel` defaults to 0 and wraps if the buffer has fewer.",
    },
    ListBuiltin {
        name: "slice",
        params: &["buffer", "start", "end", "rate", "channel"],
        arities: &[3, 4, 5],
        variadic: false,
        receives: ValueKind::Buffer,
        returns: ValueKind::Signal,
        doc: "Read a portion of a buffer, once: `slice(amen, 0, 0.25)`. `start` and `end` are between 0 and 1. `start` past `end` plays that portion backwards. `rate` defaults to 1.",
    },
    ListBuiltin {
        name: "secs",
        params: &["buffer"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Buffer,
        returns: ValueKind::Number,
        doc: "Attribute on a buffer. How long a buffer is, in seconds.",
    },
    ListBuiltin {
        name: "channels",
        params: &["buffer"],
        arities: &[1],
        variadic: false,
        receives: ValueKind::Buffer,
        returns: ValueKind::Number,
        doc: "How many channels a buffer has — 1 for mono, 2 for a stereo file.",
    },
];

/// Look up a UGen by name. `lowerer::call` reads arity from `params.len()`, so
/// this table is the single definition of both.
pub fn ugen(name: &str) -> Option<&'static Ugen> {
    UGENS.iter().find(|u| u.name == name)
}

/// Look up a list builtin by name. `lowerer::lists` reads its allowed arities
/// from here.
pub fn list_builtin(name: &str) -> Option<&'static ListBuiltin> {
    LIST_BUILTINS.iter().find(|b| b.name == name)
}

/// Look up a math builtin by name. `lowerer::math` reads its arities from here.
pub fn math_builtin(name: &str) -> Option<&'static ListBuiltin> {
    MATH_BUILTINS.iter().find(|b| b.name == name)
}

/// Look up a random builtin by name. `lowerer::random` reads its arities from
/// here.
pub fn random_builtin(name: &str) -> Option<&'static ListBuiltin> {
    RANDOM_BUILTINS.iter().find(|b| b.name == name)
}

// ---------------------------------------------------------------------------
// Note names.
// ---------------------------------------------------------------------------

/// Semitone offset of each natural note from C.
const NOTE_OFFSETS: [(u8, i32); 7] = [
    (b'c', 0),
    (b'd', 2),
    (b'e', 4),
    (b'f', 5),
    (b'g', 7),
    (b'a', 9),
    (b'b', 11),
];

/// `c0` is MIDI 12 and `g9` is 127, the top of the MIDI range.
pub const MIN_NOTE_OCTAVE: i32 = 0;
pub const MAX_NOTE_OCTAVE: i32 = 9;

pub enum NoteName {
    /// A MIDI note number, and the octave it was spelled with.
    ///
    /// The octave is kept as well as folded in because a sequence carries it
    /// forward to the notes after it, and by the time the pattern lowerer sees
    /// a step there is nothing but a number left to read it off. Recovering it
    /// by dividing would answer for a bare frequency too, and `[220, a]` has no
    /// octave in it to carry.
    Note { midi: f64, octave: i32 },
    /// A note spelled without an octave: `a`, `ef`, `gs`. Only meaningful in a
    /// sequence, where an earlier note has said which octave is in force.
    PitchClass(i32),
    /// Shaped like a note, but the octave is outside the supported range.
    OctaveOutOfRange(i32),
    NotANote,
}

/// Read a bare identifier as a note name: letter, optional `s`/`f`, optional
/// octave.
///
/// Deliberately **not** seeded into the environment. Resolution happens only
/// when a variable lookup misses, so user bindings shadow note names for free
/// and `lower_voice` — which runs per note — pays nothing to set them up. That
/// shadowing is the whole of what keeps `f`, `a` and `e` usable as ordinary
/// parameter names now that the octave is optional; outside a sequence an
/// octave-less note is an error rather than a value, so nothing silently
/// becomes a pitch where a name was meant.
///
/// Flats are `f` rather than `b`, both because `b` is itself a note and because
/// `db3` would read against the `db` builtin. Enharmonics are allowed: `bs3` is
/// `c4`, `cf4` is `b3`.
pub fn note(name: &str) -> NoteName {
    let Some(&letter) = name.as_bytes().first() else {
        return NoteName::NotANote;
    };
    let Some((_, offset)) = NOTE_OFFSETS.iter().find(|(c, _)| *c == letter) else {
        return NoteName::NotANote;
    };

    let rest = &name[1..];
    let (accidental, digits) = if let Some(d) = rest.strip_prefix('s') {
        (1, d)
    } else if let Some(d) = rest.strip_prefix('f') {
        (-1, d)
    } else {
        (0, rest)
    };

    // Letter and accidental with nothing after them: a pitch class, waiting for
    // the sequence around it to say which octave.
    if digits.is_empty() {
        return NoteName::PitchClass(offset + accidental);
    }
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return NoteName::NotANote;
    }
    let Ok(octave) = digits.parse::<i32>() else {
        return NoteName::NotANote;
    };
    if !(MIN_NOTE_OCTAVE..=MAX_NOTE_OCTAVE).contains(&octave) {
        return NoteName::OctaveOutOfRange(octave);
    }

    NoteName::Note { midi: in_octave(offset + accidental, octave), octave }
}

/// A pitch class placed in an octave. Enharmonics run off either end by a
/// semitone — `bs3` is `c4` and `cf4` is `b3` — which is the point of folding
/// the accidental into the offset before it gets here.
pub fn in_octave(offset: i32, octave: i32) -> f64 {
    ((octave + 1) * 12 + offset) as f64
}

// ---------------------------------------------------------------------------
// Written note values.
// ---------------------------------------------------------------------------

/// A written note value, in beats, held exactly.
///
/// A rational rather than an `f64` because a tuplet's count is decided by a
/// gcd, and a gcd is only meaningful on exact numbers. Dotted values put a 3 in
/// the numerator, so the moment `q.dot` exists the denominators stop being
/// powers of two and float rounding starts choosing the wrong tuplet. The
/// numbers involved are tiny — powers of two times factors of 3/2 — so an
/// `i64` pair is never near overflowing.
///
/// Kept here beside `note` rather than in the lowerer because it is language
/// surface: `q` is spelled and resolved exactly as `c4` is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Beats {
    num: i64,
    den: i64,
}

fn gcd_i64(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    // Only reachable from `Beats::new(0, _)`, which nothing writes: durations
    // are positive. One keeps the reduction a no-op rather than dividing by nil.
    if a == 0 { 1 } else { a }
}

impl Beats {
    /// Always reduced, with the sign in the numerator, so `PartialEq` means
    /// equality of value and not merely of spelling — `2/4` has to equal `1/2`
    /// or the gcd walk compares unequal things.
    pub fn new(num: i64, den: i64) -> Beats {
        debug_assert!(den != 0, "a duration with no denominator");
        let sign = if den < 0 { -1 } else { 1 };
        let (num, den) = (num * sign, den * sign);
        let g = gcd_i64(num, den);
        Beats { num: num / g, den: den / g }
    }

    pub fn whole(n: i64) -> Beats {
        Beats::new(n, 1)
    }

    pub fn to_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    pub fn add(self, other: Beats) -> Beats {
        Beats::new(self.num * other.den + other.num * self.den, self.den * other.den)
    }

    /// The largest value dividing both a whole number of times — the unit a
    /// tuplet is counted in. `gcd(a/b, c/d)` is `gcd(ad, cb) / bd`, which `new`
    /// then reduces.
    pub fn gcd(self, other: Beats) -> Beats {
        Beats::new(
            gcd_i64(self.num * other.den, other.num * self.den),
            self.den * other.den,
        )
    }

    /// Dotted `dots` times: `value * (2 - 2^-dots)`.
    ///
    /// Each dot adds half of *the note*, not half of what the previous dot left
    /// — so a doubly-dotted quarter is a quarter and an eighth and a sixteenth,
    /// which is 7/4 and not the 9/4 that dotting a dotted value would give.
    /// That is why the count is a parameter rather than something reached by
    /// writing `.dot` twice.
    pub fn dotted(self, dots: u32) -> Beats {
        // (2^(n+1) - 1) / 2^n, which is 3/2, 7/4, 15/8 as n climbs.
        let scale = 1i64 << dots;
        Beats::new(self.num * (2 * scale - 1), self.den * scale)
    }

    pub fn times(self, n: i64) -> Beats {
        Beats::new(self.num * n, self.den)
    }

    /// How many `unit`s fit in this, when it is a whole number of them. The gcd
    /// of a group's contents always divides their sum exactly, so the `None`
    /// arm is unreachable from the tuplet path — it exists so the arithmetic
    /// cannot silently truncate if some later caller asks a different question.
    pub fn divide_by(self, unit: Beats) -> Option<i64> {
        let num = self.num * unit.den;
        let den = self.den * unit.num;
        if den == 0 || num % den != 0 {
            return None;
        }
        Some(num / den)
    }
}

impl std::fmt::Display for Beats {
    /// Named where there is a name for it, since that is what the writer typed;
    /// a bare count of beats otherwise, which is what a tie or a tuplet's
    /// leftovers come to.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(d) = DURATIONS.iter().find(|d| d.beats() == *self) {
            return write!(f, "{}", d.name);
        }
        if self.den == 1 {
            write!(f, "{} beats", self.num)
        } else {
            write!(f, "{}/{} beats", self.num, self.den)
        }
    }
}

/// One written value: how it is spelled, how long it is, and what to say about
/// it. The doc travels with the entry so the editor's `;` menu is generated
/// from this table rather than from a copy of it — the same rule the callable
/// names follow.
pub struct WrittenValue {
    pub name: &'static str,
    num: i64,
    den: i64,
    pub doc: &'static str,
}

/// The written values, in beats, taking a quarter as the beat — which is what
/// `BEATS_PER_CYCLE` already assumes in reading `bpm`.
///
/// Deliberately **not** seeded into the environment, for the same reason note
/// names are not: resolution happens only when a variable lookup misses, so a
/// user `let e = 3` or a parameter named `e` shadows the eighth for free.
///
/// Thirty-seconds have no spelling here. `t` is spent on the tuplet marker, and
/// inventing a second letter for a value this rare is worse than writing the
/// tuplet that reaches it.
pub static DURATIONS: &[WrittenValue] = &[
    WrittenValue { name: "w", num: 4, den: 1, doc: "Whole note — four beats, which is one bar in 4/4 and more than one in any shorter meter." },
    WrittenValue { name: "h", num: 2, den: 1, doc: "Half note — two beats." },
    WrittenValue { name: "q", num: 1, den: 1, doc: "Quarter note — one beat." },
    WrittenValue { name: "e", num: 1, den: 2, doc: "Eighth note — half a beat." },
    WrittenValue { name: "s", num: 1, den: 4, doc: "Sixteenth note — a quarter of a beat." },
];

impl WrittenValue {
    pub fn beats(&self) -> Beats {
        Beats::new(self.num, self.den)
    }
}

/// Marks a group as a tuplet: `[[c4;q, d4, e4];t, f4]`. It carries no number
/// because there is none to carry — the count, the unit and the resulting span
/// all follow from what the group holds.
pub const TUPLET: &str = "t";

/// What `t` means, kept beside the values so the editor can offer it in the
/// same menu.
pub const TUPLET_DOC: &str =
    "Tuplet — play this group in the next lower power of two. Three quarters in \
     the time of two, five eighths in the time of four. Its contents have to fill \
     the division, and cannot already come to a plain duration.";

/// Read a bare identifier as a written note value.
pub fn duration(name: &str) -> Option<Beats> {
    DURATIONS.iter().find(|d| d.name == name).map(|d| d.beats())
}

/// How many units a tuplet of `n` is played in the time of: the next lower
/// power of two, which is what "three in the time of two" and "five in the time
/// of four" are both instances of.
///
/// Answers `n` itself when `n` is already a power of two — and that equality is
/// the test for a group that is not a tuplet at all, since it would be played
/// in exactly the time it is written.
pub fn tuplet_span(n: i64) -> i64 {
    debug_assert!(n > 0, "a tuplet of {n}");
    1i64 << (63 - n.leading_zeros() as i64)
}

/// The arithmetic tuplets are decided by. Exactness is the whole point of the
/// type, so these are about it holding under the operations `tuplet` performs.
#[cfg(test)]
mod beats_tests {
    use super::*;

    fn q() -> Beats { duration("q").expect("q") }
    fn e() -> Beats { duration("e").expect("e") }
    fn h() -> Beats { duration("h").expect("h") }
    /// A dotted quarter — the value that makes a float gcd start drifting.
    fn dotted_q() -> Beats { Beats::new(3, 2) }

    #[test]
    fn equal_values_written_differently_compare_equal() {
        assert_eq!(Beats::new(2, 4), e());
        assert_eq!(Beats::new(4, 2), h());
    }

    #[test]
    fn the_gcd_is_the_unit_a_group_is_counted_in() {
        assert_eq!(q().gcd(e()), e());
        assert_eq!(h().gcd(q()), q());
        // The case the rational exists for: a dotted quarter against eighths.
        assert_eq!(dotted_q().gcd(e()), e());
    }

    /// `[q, q, q]` is three quarters; `[h, q]` is the same three, tied.
    #[test]
    fn a_count_is_the_sum_over_the_unit() {
        let three_q = q().add(q()).add(q());
        assert_eq!(three_q.divide_by(q()), Some(3));
        let tied = h().add(q());
        assert_eq!(tied.divide_by(h().gcd(q())), Some(3));
    }

    /// The rule the tuplet refusal is: a count that is already a power of two
    /// would be played in exactly the time it is written.
    #[test]
    fn a_tuplet_span_is_the_next_lower_power_of_two() {
        assert_eq!(tuplet_span(3), 2);
        assert_eq!(tuplet_span(5), 4);
        assert_eq!(tuplet_span(6), 4);
        assert_eq!(tuplet_span(7), 4);
        assert_eq!(tuplet_span(9), 8);
        for n in [1, 2, 4, 8, 16] {
            assert_eq!(tuplet_span(n), n, "{n} compresses nothing");
        }
    }

    /// Why no reading of a group has to be chosen between: halving the unit and
    /// doubling the count leaves the span it is played in alone. This is what
    /// lets `;t` carry no number.
    #[test]
    fn halving_the_unit_and_doubling_the_count_holds_the_span() {
        for n in 1..=32i64 {
            assert_eq!(tuplet_span(2 * n), 2 * tuplet_span(n), "at {n}");
        }
        // The instance of it that matters: six eighths and three quarters.
        assert_eq!(e().times(tuplet_span(6)), q().times(tuplet_span(3)));
    }

    /// Named where there is a name, since that is what was typed.
    #[test]
    fn a_value_prints_as_what_it_was_written_as() {
        assert_eq!(q().to_string(), "q");
        assert_eq!(h().to_string(), "h");
        assert_eq!(dotted_q().to_string(), "3/2 beats");
    }
}

// ---------------------------------------------------------------------------
// The editor-facing view.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinInfo {
    pub name: &'static str,
    pub params: &'static [&'static str],
    /// Every accepted argument count. A UGen has exactly one.
    pub arities: Vec<usize>,
    /// True when any count above the largest listed arity is also accepted.
    pub variadic: bool,
    /// `"ugen"`, `"list"`, `"math"`, `"random"` or `"special"` — the editor
    /// colours and ranks by this.
    pub category: &'static str,
    /// What may be written to the left of the dot that calls this name. The
    /// editor offers a name in method position only when the receiver it can
    /// see suits this.
    pub receives: ValueKind,
    /// What the name answers with, for the next dot in a chain.
    pub returns: ValueKind,
    pub doc: &'static str,
}

/// A written note value, or the tuplet marker, as the editor offers it after a
/// `;`. Served rather than mirrored: these are language surface like any other
/// name, and a second copy in TypeScript would be a second thing to forget.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DurationInfo {
    /// How it is written after the `;`.
    pub name: &'static str,
    /// Its length in beats — `None` for `t`, which is a mark rather than a
    /// length and takes its span from what the group holds.
    pub beats: Option<f64>,
    /// True for `t`, which follows a group's `]` and nothing else. The editor
    /// uses this to offer it in the one position where it means something.
    pub marks_group: bool,
    pub doc: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageMetadata {
    pub builtins: Vec<BuiltinInfo>,
    pub keywords: &'static [&'static str],
    /// What may be written after a `;`.
    pub durations: Vec<DurationInfo>,
}

/// Everything the editor needs to highlight, complete and describe swync.
pub fn metadata() -> LanguageMetadata {
    let ugens = UGENS.iter().map(|u| BuiltinInfo {
        name: u.name,
        params: u.params,
        arities: vec![u.params.len()],
        variadic: false,
        category: "ugen",
        receives: u.receives,
        returns: u.returns,
        doc: u.doc,
    });

    let lists = LIST_BUILTINS.iter().map(|b| BuiltinInfo {
        name: b.name,
        params: b.params,
        arities: b.arities.to_vec(),
        variadic: b.variadic,
        category: "list",
        receives: b.receives,
        returns: b.returns,
        doc: b.doc,
    });

    let maths = MATH_BUILTINS.iter().map(|b| BuiltinInfo {
        name: b.name,
        params: b.params,
        arities: b.arities.to_vec(),
        variadic: b.variadic,
        category: "math",
        receives: b.receives,
        returns: b.returns,
        doc: b.doc,
    });

    let randoms = RANDOM_BUILTINS.iter().map(|b| BuiltinInfo {
        name: b.name,
        params: b.params,
        arities: b.arities.to_vec(),
        variadic: b.variadic,
        category: "random",
        receives: b.receives,
        returns: b.returns,
        doc: b.doc,
    });

    let specials = SPECIALS.iter().map(|b| BuiltinInfo {
        name: b.name,
        params: b.params,
        arities: b.arities.to_vec(),
        variadic: b.variadic,
        category: "special",
        receives: b.receives,
        returns: b.returns,
        doc: b.doc,
    });

    let durations = DURATIONS
        .iter()
        .map(|d| DurationInfo {
            name: d.name,
            beats: Some(d.beats().to_f64()),
            marks_group: false,
            doc: d.doc,
        })
        // Longest first above, and the marker last: the menu reads as a scale
        // from a whole note down, with the one thing that is not a length after
        // it rather than sorted into the middle of them.
        .chain(std::iter::once(DurationInfo {
            name: TUPLET,
            beats: None,
            marks_group: true,
            doc: TUPLET_DOC,
        }))
        .collect();

    LanguageMetadata {
        builtins: ugens.chain(lists).chain(maths).chain(randoms).chain(specials).collect(),
        keywords: KEYWORDS,
        durations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Every name is unique across all three tables — a duplicate would make
    /// one entry unreachable, since `call_with` tries the tables in order.
    #[test]
    fn names_are_unique_and_non_empty() {
        let mut seen = HashSet::new();
        for name in UGENS
            .iter()
            .map(|u| u.name)
            .chain(LIST_BUILTINS.iter().map(|b| b.name))
            .chain(MATH_BUILTINS.iter().map(|b| b.name))
            .chain(RANDOM_BUILTINS.iter().map(|b| b.name))
            .chain(SPECIALS.iter().map(|b| b.name))
        {
            assert!(!name.is_empty(), "a builtin has an empty name");
            assert!(seen.insert(name), "{name} is defined twice");
        }
    }

    /// Guards the arity refactor: `lowerer::call` derives each UGen's arity
    /// from `params.len()`, so a short parameter list would silently loosen an
    /// arity check. These are the counts the old match arm enforced.
    #[test]
    fn ugen_arities_match_the_original_table() {
        let expected: &[(&str, usize)] = &[
            ("adsr", 5), ("afollow", 3), ("allpass", 3), ("allpole", 2),
            ("bandpass", 3), ("bandrez", 3), ("bell", 4), ("biquad", 6),
            ("brown", 0), ("butterpass", 2), ("chorus", 5), ("clip", 1),
            ("clip_to", 3), ("dcblock", 1), ("declick", 1), ("delay", 2),
            ("dsf_saw", 2), ("env", 5), ("dsf_square", 2), ("fir3", 2),
            ("follow", 2), ("hammond", 1), ("highpass", 3), ("highpole", 2),
            ("highshelf", 4), ("hold", 3), ("impulse", 0), ("input", 1), ("limiter", 3),
            ("line", 3), ("lorenz", 1), ("lowpass", 3), ("lowpole", 2), ("lowrez", 3),
            ("lowshelf", 4), ("mls", 0), ("mls_bits", 1), ("moog", 3),
            ("morph", 4), ("noise", 0), ("notch", 3), ("organ", 1),
            ("peak", 3), ("perc", 2), ("pink", 0), ("pinkpass", 1),
            ("pluck", 4), ("poly_pulse", 2), ("poly_saw", 1), ("poly_square", 1),
            ("pulse", 2), ("ramp", 1), ("resonator", 3),
            ("reverb", 4), ("reverb2", 6), ("reverb3", 4), ("reverb4", 3),
            ("rossler", 1),
            ("saw", 1), ("sin", 1), ("soft_saw", 1), ("square", 1),
            ("tap", 4), ("tick", 1), ("triangle", 1),
        ];

        assert_eq!(UGENS.len(), expected.len(), "a UGen was added or removed");
        for (name, arity) in expected {
            let u = ugen(name).unwrap_or_else(|| panic!("{name} is missing from UGENS"));
            assert_eq!(
                u.params.len(),
                *arity,
                "{name} takes {arity} arguments but has {} parameter names",
                u.params.len()
            );
        }
    }

    /// The bound values aside, every special is a name `lowerer::call` picks
    /// off its own path rather than finding in a table — and it has to
    /// intercept exactly those. A name in the table the lowerer does not know
    /// is completable but not callable; one it knows that is missing here is
    /// callable but invisible to the editor.
    #[test]
    fn the_table_and_the_lowerer_agree_on_the_specials() {
        use crate::lowerer::lower::Lowerer;
        // Numbers the lowerer defines in the environment before the program
        // runs, rather than calls: the note's length and the beat, in both the
        // units a signal asks for it in. The editor needs them in the table to
        // complete and document them, and nothing calls them.
        const BOUND: [&str; 3] = ["dur", "qvs", "qvh"];
        for b in SPECIALS {
            // Everything else is intercepted somewhere: the `play` family,
            // `then`, `play_all`, `load`, the four MIDI names, `slider`,
            // `accel`, and the three that begin from a buffer.
            let intercepted = Lowerer::is_play(b.name)
                || Lowerer::is_section(b.name)
                || Lowerer::is_play_all(b.name)
                || Lowerer::is_load(b.name)
                || Lowerer::is_midiout(b.name)
                || Lowerer::is_midiin(b.name)
                || Lowerer::is_midi_control(b.name)
                || Lowerer::is_midiclock(b.name)
                || Lowerer::is_control(b.name)
                || Lowerer::is_trigger(b.name)
                || Lowerer::is_buffer_builtin(b.name)
                || Lowerer::is_rate(b.name);
            assert_eq!(
                intercepted,
                !BOUND.contains(&b.name),
                "{} is in the table but nothing intercepts it (or the reverse)",
                b.name,
            );
        }
    }

    /// The realizer reads inputs positionally, so a UGen's parameter names must
    /// line up one-to-one with its inputs — no duplicates, none blank.
    #[test]
    fn ugen_parameter_names_are_distinct() {
        for u in UGENS {
            let unique: HashSet<_> = u.params.iter().collect();
            assert_eq!(unique.len(), u.params.len(), "{} repeats a parameter name", u.name);
            assert!(u.params.iter().all(|p| !p.is_empty()), "{} has a blank parameter", u.name);
        }
    }

    /// A list builtin's parameter names must cover its largest arity, or
    /// signature help would run out of names mid-call.
    #[test]
    fn list_builtin_params_cover_their_arities() {
        for b in LIST_BUILTINS.iter().chain(MATH_BUILTINS).chain(RANDOM_BUILTINS).chain(SPECIALS) {
            assert!(!b.arities.is_empty() || b.params.is_empty(), "{}", b.name);
            if let Some(max) = b.arities.iter().max() {
                assert!(
                    b.params.len() >= *max,
                    "{} accepts {max} arguments but names only {} parameters",
                    b.name,
                    b.params.len()
                );
            }
        }
    }

    /// The wire shape the `Builtin` interface in `src/swync/metadata.ts` is
    /// written against. A rename here is a silent breakage over there.
    #[test]
    fn metadata_serializes_to_the_shape_the_editor_expects() {
        let json = serde_json::to_value(metadata()).unwrap();

        let keywords = json["keywords"].as_array().unwrap();
        assert!(keywords.iter().any(|k| k == "fn"));

        // What the editor offers after a `;`. Served rather than mirrored, so
        // this is the contract `Duration` in `metadata.ts` is written against —
        // `marksGroup` in particular, which is what splits the one menu from
        // the other.
        let durations = json["durations"].as_array().unwrap();
        assert_eq!(durations.len(), DURATIONS.len() + 1, "the values, and `t`");
        let names: Vec<&str> =
            durations.iter().map(|d| d["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["w", "h", "q", "e", "s", "t"],
                   "longest first, with the marker last — the menu's order");

        let quarter = durations.iter().find(|d| d["name"] == "q").expect("q");
        assert_eq!(quarter["beats"], 1.0);
        assert_eq!(quarter["marksGroup"], false);
        assert!(quarter["doc"].as_str().unwrap().contains("Quarter"));

        let tuplet = durations.iter().find(|d| d["name"] == "t").expect("t");
        assert!(tuplet["beats"].is_null(), "a mark has no length of its own");
        assert_eq!(tuplet["marksGroup"], true);

        let builtins = json["builtins"].as_array().unwrap();
        assert_eq!(
            builtins.len(),
            UGENS.len()
                + LIST_BUILTINS.len()
                + MATH_BUILTINS.len()
                + RANDOM_BUILTINS.len()
                + SPECIALS.len()
        );

        let rand = builtins
            .iter()
            .find(|b| b["name"] == "rand")
            .expect("rand is missing");
        assert_eq!(rand["category"], "random");
        // The one shape no other table has: callable with no arguments at all,
        // *and* with a receiver before the dot.
        assert_eq!(rand["arities"], serde_json::json!([0, 2]));

        let m2h = builtins
            .iter()
            .find(|b| b["name"] == "m2h")
            .expect("m2h is missing");
        assert_eq!(m2h["category"], "math");

        let lowpass = builtins
            .iter()
            .find(|b| b["name"] == "lowpass")
            .expect("lowpass is missing");
        assert_eq!(lowpass["params"], serde_json::json!(["audio", "cutoff", "q"]));
        assert_eq!(lowpass["arities"], serde_json::json!([3]));
        assert_eq!(lowpass["variadic"], serde_json::json!(false));
        assert_eq!(lowpass["category"], "ugen");
        assert!(lowpass["doc"].as_str().unwrap().len() > 10);

        // The two shapes that are not a plain fixed arity.
        let rotl = builtins.iter().find(|b| b["name"] == "rotl").unwrap();
        assert_eq!(rotl["arities"], serde_json::json!([1, 2]));
        let zip = builtins.iter().find(|b| b["name"] == "zip").unwrap();
        assert_eq!(zip["variadic"], serde_json::json!(true));
    }

    /// Everything is documented. Empty docs render as a blank completion panel.
    #[test]
    fn everything_has_a_doc() {
        for (name, doc) in UGENS
            .iter()
            .map(|u| (u.name, u.doc))
            .chain(LIST_BUILTINS.iter().map(|b| (b.name, b.doc)))
            .chain(MATH_BUILTINS.iter().map(|b| (b.name, b.doc)))
            .chain(RANDOM_BUILTINS.iter().map(|b| (b.name, b.doc)))
            .chain(SPECIALS.iter().map(|b| (b.name, b.doc)))
        {
            assert!(!doc.trim().is_empty(), "{name} has no documentation");
        }
    }
}

#[cfg(test)]
mod receives_tests {
    use super::*;

    /// Instruments and predicates for the arguments the receiver does not fill.
    const PREAMBLE: &str = "fn inst(n) = sin(n)\nfn pred(x) = 1\nfn section() = play_once([60], inst)\nfn body(x) = play_once([x], inst)\n";

    /// One value of each kind that can stand to the left of a dot.
    ///
    /// There is no `Text` receiver: a string is only ever written out as
    /// `load`'s argument, so no expression answers with one and nothing can be
    /// chained from it.
    const RECEIVERS: [(ValueKind, &str); 6] = [
        (ValueKind::Number, "1"),
        (ValueKind::Signal, "sin(220)"),
        (ValueKind::List, "[1, 2, 3]"),
        (ValueKind::Play, "playn([60, 63], inst, 2)"),
        (ValueKind::Buffer, "load(\"test.wav\")"),
        (ValueKind::Section, "section"),
    ];

    /// The one buffer these tests know about, under the path they all write.
    ///
    /// Synthesized rather than read from disk: what is in it never matters
    /// here, only that `load("test.wav")` answers with a buffer so the kinds
    /// can be probed.
    fn test_samples() -> crate::samples::Samples {
        use std::sync::Arc;
        let mut wave = fundsp::wave::Wave::new(1, 44100.0);
        for i in 0..64 {
            wave.push(i as f32 / 64.0);
        }
        crate::samples::Samples::from_pairs([("test.wav".to_string(), Arc::new(wave))])
    }

    /// Whether a receiver of kind `receiver` may be written to the left of a
    /// name declaring `receives`. This is the rule the editor filters on; the
    /// test below is what makes it true rather than merely intended.
    pub fn accepts(receives: ValueKind, receiver: ValueKind) -> bool {
        // A result the table cannot pin down rules nothing out, so everything
        // stays on offer after it.
        if receiver == ValueKind::Any {
            return true;
        }
        match receives {
            // Only ever a result, never a parameter.
            ValueKind::Any => false,
            // A constant is a node input like any other, so a number reads here.
            ValueKind::Signal => matches!(receiver, ValueKind::Signal | ValueKind::Number),
            ValueKind::Number => receiver == ValueKind::Number,
            ValueKind::List => receiver == ValueKind::List,
            // A bare value is a one-step pattern: `60.play(inst)` sounds once.
            ValueKind::Pattern => matches!(receiver, ValueKind::List | ValueKind::Number),
            ValueKind::Play => receiver == ValueKind::Play,
            ValueKind::Section => receiver == ValueKind::Section,
            ValueKind::Buffer => receiver == ValueKind::Buffer,
            ValueKind::Duration => receiver == ValueKind::Duration,
            // Written out at the call, never produced — so nothing stands to
            // the left of a name that wants one.
            ValueKind::Text => false,
            // Only ever a result: `accel` answers with one and `play` takes one
            // in a position that is not the receiver, so nothing is ever
            // *called on* a rate.
            ValueKind::Rate => false,
            // The same, for the same reason: a lane value is written into a
            // `play` and read nowhere else, so nothing chains off one.
            ValueKind::Lane => false,
            // And the same again: a destination is written into a `play`'s
            // instrument slot, and a source into its pattern slot. Neither is
            // the receiver position.
            ValueKind::Destination | ValueKind::Source => false,
            ValueKind::Nothing => false,
        }
    }

    /// A plausible argument for a parameter the receiver does not fill. Keyed by
    /// name, so a domain error cannot be mistaken for a type error.
    fn filler(param: &str) -> &'static str {
        match param {
            "predicate" | "transform" => "pred",
            "instrument" => "inst",
            "section" => "section",
            // As many as `sections` has, or a weighted choice is short of one.
            "sections" => "[section, section, section]",
            // Only `then_each` takes a list it does not also receive, so this
            // filler had no reason to exist until now.
            "list" => "[1, 2, 3]",
            "body" => "body",
            "weights" => "[1, 2, 3]",
            "scale" => "[0, 2, 4, 5, 7, 9, 11]",
            "bits" => "10",
            "room_size" => "15",
            "hi" | "maximum" => "2",
            "times" => "2",
            "buffer" => "load(\"test.wav\")",
            "path" => "\"test.wav\"",
            // A control's name, which is read off the syntax and so has to be
            // written out — the same reason `path` is.
            "name" => "\"probe\"",
            "channel" => "0",
            // `dot` is the only name taking a written note value, so this
            // filler had no reason to exist until it did.
            "value" => "q",
            // A probability has to stay inside 0..=1, and 1 is in range.
            _ => "1",
        }
    }

    /// Does `receiver.name(...)` survive lowering *and* realization?
    ///
    /// Both halves matter: `perc` and `env` bake their first argument in at
    /// construction, so a signal there lowers cleanly and only fails when the
    /// graph is built. Lowering alone would call them signal-taking.
    fn compiles(receiver: &str, name: &str, params: &[&str], arity: usize) -> bool {
        let args: Vec<&str> = params[1..arity.max(1)].iter().map(|p| filler(p)).collect();
        let call = if args.is_empty() {
            format!("{receiver}.{name}")
        } else {
            format!("{receiver}.{name}({})", args.join(", "))
        };
        let Ok(items) = crate::parser::parser::parse(format!("{PREAMBLE}{call}\n")) else {
            return false;
        };
        match crate::lowerer::lower::lower_with_samples(&items, test_samples()) {
            Err(_) => false,
            Ok(l) => crate::swync_graph::realizer::realize(&l.graph).is_ok(),
        }
    }

    /// One name from the tables, flattened to what these tests need.
    struct Entry {
        name: &'static str,
        params: &'static [&'static str],
        /// The fewest arguments it can be called with.
        arity: usize,
        /// The fewest it can be called with *and still have a receiver*, which
        /// is not the same number: `rand` takes none or two, so `rand()` is its
        /// smallest call but `x.rand(y)` is its smallest one after a dot. Zero
        /// only for `dur`, which has no call form at all.
        method_arity: usize,
        /// False for `dur`, which is a binding rather than a function and so
        /// has no call form to probe.
        callable: bool,
        receives: ValueKind,
        returns: ValueKind,
    }

    fn callables() -> Vec<Entry> {
        UGENS
            .iter()
            .map(|u| Entry {
                name: u.name,
                params: u.params,
                arity: u.params.len(),
                method_arity: u.params.len(),
                callable: true,
                receives: u.receives,
                returns: u.returns,
            })
            .chain(
                LIST_BUILTINS
                    .iter()
                    .chain(MATH_BUILTINS)
                    .chain(RANDOM_BUILTINS)
                    .chain(SPECIALS)
                    .map(|b| Entry {
                        name: b.name,
                        params: b.params,
                        arity: b.arities.iter().min().copied().unwrap_or(0),
                        method_arity: b
                            .arities
                            .iter()
                            .copied()
                            .filter(|a| *a >= 1)
                            .min()
                            .unwrap_or(0),
                        callable: !b.arities.is_empty(),
                        receives: b.receives,
                        returns: b.returns,
                    }),
            )
            .collect()
    }

    /// The table says what each name takes on its left; this compiles every
    /// name against every kind of receiver and insists the two agree.
    ///
    /// A disagreement in one direction means the editor hides a name that
    /// works; in the other, that it offers one that cannot compile. Both are
    /// the bug this field exists to prevent.
    #[test]
    fn every_builtin_receives_what_it_declares() {
        // Every name in the table gets called, and four of them — `midiin`,
        // `cc`, `bend`, `aftertouch` — intern a MIDI port when they lower. That
        // is a slot taken out of a table the whole process shares, so this
        // holds the same guard the MIDI tests do: without it these two leak a
        // slot into whichever keyboard test happens to be running beside them,
        // and it is that test which fails.
        let _bus = crate::midi::input::exclusive();
        // And the controls' guard, for the same reason one line up: `slider`,
        // `toggle` and `trigger` intern a slot out of a table the process
        // shares, so a probe that lowers them without this leaks into whatever
        // control test is running beside it.
        let _controls = crate::controls::exclusive();
        for Entry { name, params, method_arity, receives, .. } in callables() {
            if receives == ValueKind::Nothing {
                continue;
            }
            for (receiver, src) in RECEIVERS {
                let declared = accepts(receives, receiver);
                let actual = compiles(src, name, params, method_arity);
                assert_eq!(
                    declared, actual,
                    "{name} declares {receives:?}, so `{src}.{name}(..)` should {}, \
                     but it does not",
                    if declared { "compile" } else { "be rejected" }
                );
            }
        }
    }

    /// A minimal call of `name`, usable as the receiver of the next dot.
    fn minimal_call(name: &str, params: &[&str], arity: usize) -> String {
        let args: Vec<&str> = params[..arity].iter().map(|p| filler(p)).collect();
        // The first parameter is filled here like any other, since nothing
        // precedes this call. A name callable with no arguments — `rand()` —
        // has none to fill, even though it does have parameters.
        let args = if args.is_empty() {
            Vec::new()
        } else {
            let mut a = args;
            a[0] = match params[0] {
                "list" | "values" => "[1, 2, 3]",
                "pattern" => "[60, 63]",
                "play" => "playn([60, 63], inst, 2)",
                "section" => "section",
                "buffer" => "load(\"test.wav\")",
                "path" => "\"test.wav\"",
                "name" => "\"probe\"",
                // The only name receiving a written note value is `dot`.
                "value" => "q",
                _ => "1",
            };
            a
        };
        format!("{name}({})", args.join(", "))
    }

    /// One name per kind that accepts that kind and no other, so which of them
    /// compiles identifies what the receiver was.
    const PROBES: [(ValueKind, &str); 7] = [
        (ValueKind::List, "len"),
        // `take` rather than `play_all`: gathering takes sections, which are
        // named `fn`s, so a play is exactly what may *not* be piped into it.
        // Cutting one is the move that needs a play on its left and refuses
        // everything else.
        (ValueKind::Play, "take(1)"),
        // Nothing but a buffer has a length in seconds.
        (ValueKind::Buffer, "secs"),
        // `m2h` takes a number and refuses a signal; `clip` takes either. So a
        // receiver both accept is a number, and one only `clip` accepts is a
        // signal. Order matters: the narrower probe is asked first.
        (ValueKind::Number, "m2h"),
        (ValueKind::Signal, "clip"),
        // Before the pattern probe, and it has to be: a written note value is
        // a one-step pattern, so `play_once` would take it too. Nothing but a
        // written value can be dotted.
        (ValueKind::Duration, "dot"),
        // Last, and it has to be: a list and a number are both usable as
        // patterns, so this probe accepts everything the three above do. Asked
        // here, it identifies only what nothing else would take — a layered
        // pattern, which is a pattern and nothing else.
        (ValueKind::Pattern, "play_once(inst)"),
    ];

    /// The declared result must be the one the probes actually identify.
    ///
    /// This is what makes chaining work: in `riff.rev.push(72)` the receiver of
    /// the second dot is whatever `rev` answers with, and the editor has only
    /// this field to tell it.
    #[test]
    fn every_builtin_returns_what_it_declares() {
        // Every name in the table gets called, and four of them — `midiin`,
        // `cc`, `bend`, `aftertouch` — intern a MIDI port when they lower. That
        // is a slot taken out of a table the whole process shares, so this
        // holds the same guard the MIDI tests do: without it these two leak a
        // slot into whichever keyboard test happens to be running beside them,
        // and it is that test which fails.
        let _bus = crate::midi::input::exclusive();
        // And the controls' guard, for the same reason one line up: `slider`,
        // `toggle` and `trigger` intern a slot out of a table the process
        // shares, so a probe that lowers them without this leaks into whatever
        // control test is running beside it.
        let _controls = crate::controls::exclusive();
        for Entry { name, params, arity, callable, returns, .. } in callables() {
            // `Any` is the honest answer where the result follows the input;
            // there is nothing single-valued to check it against. A name that
            // is not a function has no call to probe at all.
            if returns == ValueKind::Any || !callable {
                continue;
            }
            let call = minimal_call(name, params, arity);
            let found = PROBES.iter().find(|(_, probe)| {
                let src = format!("{PREAMBLE}{call}.{probe}\n");
                let Ok(items) = crate::parser::parser::parse(src) else { return false };
                match crate::lowerer::lower::lower_with_samples(&items, test_samples()) {
                    Err(_) => false,
                    Ok(l) => crate::swync_graph::realizer::realize(&l.graph).is_ok(),
                }
            });
            // A rate is where a chain stops, and so is a lane value and a MIDI
            // destination. `play` takes all three as arguments rather than as
            // receivers, so no probe could name any of them — and that is the
            // property worth pinning: nothing at all may follow one, so the
            // editor offering anything after the dot would be wrong.
            if matches!(
                returns,
                ValueKind::Rate | ValueKind::Lane | ValueKind::Destination | ValueKind::Source
            ) {
                assert!(
                    found.is_none(),
                    "{name} answers with a {returns:?}, so nothing should chain off \
                     `{call}` — but {:?} did",
                    found.map(|(k, _)| *k)
                );
                continue;
            }
            assert_eq!(
                found.map(|(k, _)| *k),
                Some(returns),
                "{name} declares it returns {returns:?}, but `{call}` does not behave like one"
            );
        }
    }

    /// A name taking no arguments takes no receiver either — `x.noise` is an
    /// arity error, which is why the editor drops these in method position.
    #[test]
    fn only_zero_argument_names_receive_nothing() {
        for Entry { name, params, receives, .. } in callables() {
            assert_eq!(
                params.is_empty(),
                receives == ValueKind::Nothing,
                "{name}: params {params:?} disagrees with {receives:?}"
            );
        }
    }
}
