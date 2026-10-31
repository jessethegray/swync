//! The scheduler thread.
//!
//! It free-runs from app start and is never "triggered": an eval simply
//! replaces the patterns and instruments it reads on the next pass. That
//! inversion is what keeps the clock running across re-evals.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use fundsp::prelude64::AudioUnit;
use fundsp::sequencer::{EventId, Fade, Sequencer};

use crate::audition::Audition;
use crate::diagnostic::{Diagnostic, Stage};
use crate::midi::input::HELD_SECS;
use crate::midi::out as midi;
use crate::pattern::pattern::Span;
use crate::pattern::patterns::{
    Binding, BoundEvent, LaneArg, LaneValues, Patterns, Target, VELOCITY,
};
use crate::scheduler::clock::{Clock, Meter};
use crate::swync_graph::realizer as realizer;
use crate::swync_graph::realizer::Gate;
use crate::scheduler::voice::{Instruments, build_held_voice, build_voice};

/// How far ahead of the audio clock we schedule.
pub(crate) const LOOKAHEAD_SECS: f64 = 0.2;
/// How often the thread wakes. Must be well under LOOKAHEAD_SECS, and well
/// under the clock's `START_LEAD_SECS` — a pass a tick behind an eval still has
/// to find bar 0 ahead of it.
pub(crate) const TICK: Duration = Duration::from_millis(25);

/// How often it wakes while a keyboard is being played.
///
/// A written pattern does not care when this thread runs: it is handed to the
/// sequencer with a start time a fifth of a second out, and placed to the
/// sample. A *key press* is the opposite — it has already happened, and every
/// millisecond between the press and the push is latency somebody can feel
/// under their fingers. At 25 ms this thread would be adding up to a full tick
/// of slop to an instrument that is supposed to feel connected to the hand
/// playing it, which is the difference between a keyboard and a lag.
///
/// Two milliseconds is under the point anybody detects and above the point
/// where the wake itself costs anything. It is used only while a live binding
/// exists, so the ordinary session — every program that plays no keyboard —
/// wakes as rarely as it always did.
const LIVE_TICK: Duration = Duration::from_millis(2);

/// How far ahead of the audio clock a live note is pushed.
///
/// Not the lookahead a pattern gets, which would be a fifth of a second of
/// latency. Just enough to clear the block the callback is rendering now, so
/// that a start time is never one the sequencer has already gone past.
const LIVE_LEAD_SECS: f64 = 0.005;
/// Per-voice fades, clamped against short notes before pushing.
const FADE_IN_SECS: f64 = 0.005;
const FADE_OUT_SECS: f64 = 0.02;

/// Where a failure the scheduler cannot recover from is sent. The app sets one
/// that emits to the editor; tests set one that records.
type Reporter = Box<dyn Fn(Diagnostic) + Send + Sync>;

/// What the project panel's play button has asked for, waiting for the one
/// thread allowed to touch the sequencer.
///
/// One slot rather than a queue, and the two asks share it rather than having a
/// flag each: they are the same button, so what matters is which was pressed
/// last. Clicking down a folder of kicks means the one under the pointer now,
/// not all of them at once — and a stop that arrived a moment before a play
/// must not silence the play.
enum Asked {
    Play(Audition),
    Silence,
}

/// Shared handles the scheduler reads each pass. An eval swaps their contents.
#[derive(Clone)]
pub struct SchedulerState {
    pub patterns: Arc<Mutex<Patterns>>,
    pub instruments: Arc<Mutex<Instruments>>,
    /// Raised by `stop`, consumed by the scheduler thread. Clearing the
    /// patterns stops *new* voices; only the thread that owns the sequencer
    /// can cut the ones already pushed into the lookahead window.
    stop: Arc<AtomicBool>,
    /// Installed once at startup. A `OnceLock` rather than a field set at
    /// construction because the thing it reports to — the Tauri handle — does
    /// not exist until after this state has been made.
    report: Arc<OnceLock<Reporter>>,
    /// The rate the sequencer should stamp voices with, as `f64::to_bits`, and
    /// zero while nobody has asked for one.
    ///
    /// It is here rather than passed in because the sequencer belongs to the
    /// scheduler thread outright and no other thread may touch it — which is
    /// what keeps it out of a mutex — while the thing that moves the rate is a
    /// device switch on the command thread. See `set_sample_rate`.
    sample_rate: Arc<AtomicU64>,
    /// A sample the project panel wants heard, or a request to stop hearing
    /// one. Here for the same reason `sample_rate` is: the sequencer belongs to
    /// the scheduler thread outright, and the button is pressed on another.
    asked: Arc<Mutex<Option<Asked>>>,
    /// Where the notes of a binding that plays gear rather than an instrument
    /// go. See [`crate::midi::out`] for why they leave on their own thread
    /// instead of being pushed into the sequencer like everything else.
    ///
    /// A field here rather than an argument to `start` because `schedule_pass`
    /// is called directly by a great many tests, and threading a handle
    /// through every one of them would be asking tests about rhythm to have an
    /// opinion about MIDI. [`crate::midi::out::Out::detached`] is what they
    /// get, and it swallows what it is sent.
    pub midi: midi::Out,
}

impl SchedulerState {
    pub fn new() -> Self {
        SchedulerState {
            patterns: Arc::new(Mutex::new(Patterns::default())),
            instruments: Arc::new(Mutex::new(Instruments::default())),
            stop: Arc::new(AtomicBool::new(false)),
            report: Arc::new(OnceLock::new()),
            sample_rate: Arc::new(AtomicU64::new(0)),
            asked: Arc::new(Mutex::new(None)),
            midi: midi::Out::detached(),
        }
    }

    /// The same state, sending its MIDI somewhere real. Called once, at
    /// startup, by the only caller that has a MIDI thread to point at.
    pub fn with_midi(mut self, midi: midi::Out) -> SchedulerState {
        self.midi = midi;
        self
    }

    /// Play a sample file, as soon as the next pass can push it.
    ///
    /// The buffer was decoded and the voice built by whoever called this — the
    /// command thread, which is the only one allowed to read a disk. Nothing
    /// here can fail in a way worth telling anybody about: a lock so poisoned
    /// that a button press is lost is a scheduler that has already stopped
    /// playing, and it will have said so through its own reporter.
    pub fn audition(&self, voice: Audition) {
        if let Ok(mut asked) = self.asked.lock() {
            *asked = Some(Asked::Play(voice));
        }
    }

    /// Stop whatever is being auditioned. Not a stop of the performance: the
    /// graph and the patterns are untouched.
    pub fn silence_audition(&self) {
        if let Ok(mut asked) = self.asked.lock() {
            *asked = Some(Asked::Silence);
        }
    }

    fn take_asked(&self) -> Option<Asked> {
        self.asked.lock().ok()?.take()
    }

    /// The output device has moved to another rate, so voices pushed from now
    /// on must be stamped with it.
    ///
    /// This is the half of a rate change that `AudioEngine::set_sample_rate`
    /// cannot reach. Without it every pattern voice keeps the old rate while
    /// the graph renders at the new one, and the two come out in different
    /// keys — the same failure `a_pushed_voice_runs_at_the_device_rate` pins
    /// for the startup case.
    ///
    /// Taken up on the scheduler's next pass rather than here: 25 ms later,
    /// and by the one thread allowed to touch the sequencer.
    pub fn set_sample_rate(&self, sample_rate: f64) {
        self.sample_rate.store(sample_rate.to_bits(), Ordering::Release);
    }

    fn wanted_sample_rate(&self) -> f64 {
        f64::from_bits(self.sample_rate.load(Ordering::Acquire))
    }

    /// Say where a playback failure should go. Call once, at startup; a second
    /// call is ignored rather than replacing a live handler mid-performance.
    pub fn on_error(&self, report: impl Fn(Diagnostic) + Send + Sync + 'static) {
        let _ = self.report.set(Box::new(report));
    }

    /// Ask the scheduler thread to silence everything it has pushed.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Whether anything currently bound is a keyboard, which is what decides
    /// how often the thread wakes. A poisoned lock answers no: the pass that
    /// reads it properly will fail loudly a moment later, and waking fast on
    /// the way there helps nobody.
    fn has_live_source(&self) -> bool {
        self.patterns
            .lock()
            .map(|p| p.bindings.iter().any(|b| b.source.live().is_some()))
            .unwrap_or(false)
    }

    fn take_stop(&self) -> bool {
        self.stop.swap(false, Ordering::Acquire)
    }

    /// Halt playback and say why.
    ///
    /// A pattern that cannot be turned into a voice fails on every event of
    /// every bar, for as long as it is bound: logging and carrying on means
    /// an error nobody sees, scrolling past in a console, while the music
    /// silently drops the notes it names. So the bindings go — which is what
    /// `stop_audio` does to end a performance — the pushed voices are cut by
    /// the flag, and the editor is told what happened.
    fn fail(&self, message: impl Into<String>) {
        let diagnostic = Diagnostic::message(Stage::Scheduler, message);
        eprintln!("scheduler: {diagnostic}");

        // A poisoned lock is one of the things reported here, so a failure to
        // clear must not stop the rest: the stop flag alone still silences.
        if let Ok(mut patterns) = self.patterns.lock() {
            *patterns = Patterns::default();
        }
        self.request_stop();

        if let Some(report) = self.report.get() {
            report(diagnostic);
        }
    }
}

impl Default for SchedulerState {
    fn default() -> Self {
        SchedulerState::new()
    }
}

/// Spawn the scheduler. Call once, at startup.
pub fn start(seq: Sequencer, clock: Clock, state: SchedulerState) {
    std::thread::spawn(move || run(seq, clock, state));
}

/// A voice we have pushed that may still be sounding. Stop cuts these short;
/// otherwise they retire on their own and we just forget about them.
struct Live {
    id: EventId,
    end_secs: f64,
}

/// A voice a key is holding open.
///
/// Kept apart from [`Live`] because it ends by a different means. A pattern's
/// voice knows how long it is when it is pushed; this one does not — the key
/// is still down — so it goes in with [`HELD_SECS`] on it and is cut short
/// when the release arrives. What identifies it is the three numbers the
/// release will carry, since that is all the wire says about which key came
/// up.
struct HeldKey {
    slot: usize,
    channel: u8,
    note: u8,
    id: EventId,
    /// The instrument's own envelope, waiting to be let go.
    gate: Gate,
    /// Audio time this voice started, so the release can be written in the
    /// voice's own time — which is what an envelope counts in.
    start_secs: f64,
    /// How long the instrument goes on after the key comes up. The sequencer
    /// event has to stay open at least that long or the release is cut off by
    /// the very thing that was supposed to let it run.
    tail_secs: f64,
}

/// How far the scheduler has pushed, in bars, tagged with the clock epoch it
/// was measured against.
///
/// The tag is what makes it safe to trust: a reset moves bar time backwards,
/// and a mark from before it would sit beyond every horizon this loop can
/// reach, stalling the music forever. Comparing sizes instead of epochs cannot
/// tell that apart from a tempo drop, where the horizon legitimately falls
/// behind a mark that is still good — and there, starting over would push the
/// lookahead window's notes a second time.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Mark {
    epoch: u64,
    bars: f64,
}

fn run(mut seq: Sequencer, clock: Clock, state: SchedulerState) {
    // `None` until the first pass, so a long idle period before the first eval
    // does not look like a huge backlog to catch up on.
    let mut scheduled_through: Option<Mark> = None;
    let mut live: Vec<Live> = Vec::new();
    // The sample being auditioned, which is at most one — see [`Asked`]. Kept
    // apart from `live` because it is not part of the performance: it is not
    // scheduled against bar time, and nothing about it survives an eval.
    let mut auditioning: Option<Live> = None;
    // What `wire` stamped the sequencer with is right until a device switch
    // says otherwise, and zero here means nothing has.
    let mut rate = 0.0;
    // Keys currently down, and how many have been pressed. Both outlive a
    // pass: a key held across one is still down, and a lane read by note has
    // to go on counting.
    let mut held: Vec<HeldKey> = Vec::new();
    let mut played = 0usize;
    // How long to sleep, which is not fixed — see `LIVE_TICK`. Starts at the
    // idle rate, since nothing is bound before the first eval.
    let mut tick = TICK;

    loop {
        std::thread::sleep(tick);

        // Before anything is pushed, so no voice is ever built against a rate
        // the graph has already stopped rendering at.
        let wanted = state.wanted_sample_rate();
        if wanted > 0.0 && wanted != rate {
            seq.set_sample_rate(wanted);
            rate = wanted;
        }

        if state.take_stop() {
            silence(&mut seq, &mut live);
            // A key held when the transport stopped is a voice like any other
            // in the room, and the release for it may never come — the player
            // has taken their hand off a keyboard that is no longer playing
            // anything.
            release_all(&mut seq, &mut held);
            // The room going quiet has to include the gear in it. Nothing else
            // would release those notes: a MIDI note is held until something
            // says otherwise, so a stop that only cut the sequencer would
            // leave whatever was sounding droning after the silence.
            state.midi.stop();
            // Stop means the room goes quiet, and a sample being auditioned is
            // a sound in the room like any other.
            cut(&mut seq, &mut auditioning);
            // The horizon restarts from the present on the next eval.
            scheduled_through = None;
            continue;
        }

        // After the stop, so a play button pressed while a stop was pending is
        // heard rather than cut by it, and before the pass, since it is what
        // somebody is waiting on.
        take_audition(&mut seq, &clock, &state, &mut auditioning);

        // Before the bar pass, because it is the one with somebody waiting on
        // it: a key pressed a millisecond ago should not queue behind a
        // lookahead window's worth of pattern voices.
        play_live_notes(&mut seq, &clock, &state, &mut held, &mut played);

        scheduled_through = schedule_pass(&mut seq, &clock, &state, scheduled_through, &mut live);
        // Wake fast only while something is actually being played by hand.
        // Read after the pass rather than before, so an eval that has just
        // bound a keyboard is answered at the fast rate from the next tick
        // rather than the one after.
        tick = if state.has_live_source() { LIVE_TICK } else { TICK };
        let now = clock.now_secs();
        retire(&mut live, now);
        if auditioning.as_ref().is_some_and(|voice| voice.end_secs <= now) {
            auditioning = None;
        }
    }
}

/// Turn the keys that have been pressed since the last pass into voices, and
/// end the ones that have been let go.
///
/// The counterpart of [`schedule_pass`], and the shape of it is the whole
/// difference between the two kinds of source. That one asks "what happens
/// between here and the horizon?" and can, because a written pattern is
/// already decided. This one asks "what has happened?" — a key press is in the
/// past by the time anything here hears about it, so there is no horizon to
/// work against and nothing to place: the note is pushed as close to now as
/// the sequencer will take it.
///
/// `played` counts note-ons since the app started, and is what a lane is read
/// by. A lane is "the nth note takes the nth value" everywhere else, and a
/// keyboard has an nth note like anything else does — so `vel: [1, .6]` on a
/// keyboard alternates, which is the same rule and a usable one.
fn play_live_notes(
    seq: &mut Sequencer,
    clock: &Clock,
    state: &SchedulerState,
    held: &mut Vec<HeldKey>,
    played: &mut usize,
) -> Option<()> {
    let notes = crate::midi::input::bus().take_notes();
    if notes.is_empty() {
        return Some(());
    }

    // Read once for the whole batch rather than per note. Both locks are taken
    // by `schedule_pass` too, and neither is held across building a voice.
    let bindings: Vec<Binding> = match state.patterns.lock() {
        Ok(p) => p.bindings.iter().filter(|b| b.source.live().is_some()).cloned().collect(),
        Err(e) => {
            state.fail(format!("patterns lock poisoned: {e}"));
            return None;
        }
    };
    if bindings.is_empty() {
        // Notes arriving with nothing bound to them are dropped, which is the
        // right answer for a keyboard left plugged in: a program that does not
        // name it is a program that does not want it.
        return Some(());
    }
    let instruments = match state.instruments.lock() {
        Ok(i) => i.clone(),
        Err(e) => {
            state.fail(format!("instruments lock poisoned: {e}"));
            return None;
        }
    };

    for note in notes {
        if !note.on {
            release(seq, held, &note, clock.now_secs());
            continue;
        }
        for binding in &bindings {
            let source = binding.source.live().expect("filtered to live bindings");
            if source.slot != note.slot {
                continue;
            }
            // `None` is every channel, which is what a keyboard on its own
            // means — see `Source`.
            if source.channel.is_some_and(|only| only != note.channel) {
                continue;
            }

            // A key already down retriggers rather than layering. The wire
            // sends one release per key, so a second voice on the same key
            // would have nothing to end it — the same argument `midi::out`
            // makes from the other side.
            release(seq, held, &note, clock.now_secs());

            let mut args = live_args(binding, *played);
            // Velocity reaches an instrument that asked for it, under the name
            // it would have asked for a lane by. An instrument with no `vel`
            // parameter simply never sees it, which is how an instrument
            // written before any of this still plays from a keyboard.
            if instruments.declares(binding.target.instrument().unwrap_or(""), VELOCITY)
                && !args.iter().any(|(name, _)| name == VELOCITY)
            {
                args.push((VELOCITY.to_string(), LaneArg::Num(note.velocity as f64)));
            }

            let Some(instrument) = binding.target.instrument() else {
                // A keyboard playing `midiout` is notes in and notes out, which
                // is a thru rather than an instrument. Nothing here does that
                // yet, and dropping it silently is better than the alternative
                // — see `a_keyboard_cannot_yet_be_sent_straight_back_out`.
                continue;
            };

            let start_secs = clock.now_secs() + LIVE_LEAD_SECS;
            match build_held_voice(
                &instruments, instrument, note.note as f64, &args,
                HELD_SECS, clock.beat_secs(), clock.meter(),
            ) {
                Err(e) => {
                    state.fail(format!("{instrument}: {e}"));
                    return None;
                }
                Ok(voice) => {
                    let Some(gate) = voice.gate.clone() else {
                        state.fail("a held voice was built without a gate".to_string());
                        return None;
                    };
                    // Room for the release on the end, exactly as a pattern
                    // note gets room for its tail — otherwise a key held for
                    // the full `HELD_SECS` would have its release cut off by
                    // the safety net rather than by anything musical.
                    if let Some(id) =
                        push_voice(seq, start_secs, HELD_SECS + voice.tail_secs, voice.net)
                    {
                        held.push(HeldKey {
                            slot: note.slot,
                            channel: note.channel,
                            note: note.note,
                            id,
                            gate,
                            start_secs,
                            tail_secs: voice.tail_secs,
                        });
                    }
                }
            }
        }
        *played = played.wrapping_add(1);
    }

    Some(())
}

/// The lane values one live note takes, by the same rule a written note does.
fn live_args(binding: &Binding, played: usize) -> Vec<(String, LaneArg)> {
    binding
        .lanes
        .iter()
        .filter_map(|lane| {
            let value = match &lane.values {
                LaneValues::Steps(p) => {
                    let values = p.values();
                    if values.is_empty() { return None }
                    values[played % values.len()].map(LaneArg::Num)?
                }
                LaneValues::Lists(vs) => {
                    if vs.is_empty() { return None }
                    vs[played % vs.len()].clone().map(LaneArg::List)?
                }
            };
            Some((lane.name.clone(), value))
        })
        .collect()
}

/// Let go of every voice this key is holding open.
///
/// Every one rather than the last, because two bindings may both be reading
/// the same keyboard, and one release has to end both — the wire sends one
/// message per key, not one per thing listening.
///
/// **This is a release, not a cut**, and the difference is the whole reason
/// [`Gate`] exists. Ending the sequencer event here would fade the voice out
/// over `FADE_OUT_SECS`, which is twenty milliseconds — so an instrument that
/// says `env(.., 2, dur)` would be clipped to a click by the machinery meant
/// to be playing it. Instead the envelope is told the key has come up, in its
/// own time, and it releases on its own shape; the event is then left open
/// long enough for that to finish and closed with the ordinary short fade,
/// which by then is landing on silence.
fn release(seq: &mut Sequencer, held: &mut Vec<HeldKey>, note: &crate::midi::input::LiveNote,
           now_secs: f64) {
    held.retain(|key| {
        let same = key.slot == note.slot && key.channel == note.channel && key.note == note.note;
        if same {
            // In the voice's own time, which starts when the sequencer starts
            // it. A key let go before the note has begun releases at zero
            // rather than at a negative time.
            realizer::release_at(&key.gate, now_secs - key.start_secs);
            seq.edit_relative(key.id, key.tail_secs, FADE_OUT_SECS.min(key.tail_secs));
        }
        !same
    });
}

/// Cut every held key and forget them, for a stop.
///
/// A cut rather than a release, unlike [`release`], and deliberately: stop
/// means the room goes quiet now. Letting every held note run its own release
/// would leave a pad ringing for two seconds after somebody pressed stop,
/// which is the one thing a stop is for.
fn release_all(seq: &mut Sequencer, held: &mut Vec<HeldKey>) {
    for key in held.drain(..) {
        seq.edit_relative(key.id, FADE_OUT_SECS, FADE_OUT_SECS);
    }
    // What has arrived and not been dealt with goes too. A release queued
    // behind a stop would otherwise arrive after the next press and cut a
    // voice that has only just started.
    crate::midi::input::bus().forget_notes();
}

/// Play, or stop playing, whatever the project panel's button last asked for.
///
/// A new audition cuts the one before it. Two samples at once is not what
/// clicking down a folder means, and the alternative — letting them pile up —
/// turns a list of kicks into a mush that says nothing about any of them.
fn take_audition(
    seq: &mut Sequencer,
    clock: &Clock,
    state: &SchedulerState,
    auditioning: &mut Option<Live>,
) {
    let voice = match state.take_asked() {
        None => return,
        Some(Asked::Silence) => {
            cut(seq, auditioning);
            return;
        }
        Some(Asked::Play(voice)) => voice,
    };

    cut(seq, auditioning);

    // The same lookahead every note is pushed with, and for the same reason: a
    // start time the audio thread has already rendered past is a note that
    // never sounds. What it costs is that a press is heard a fifth of a second
    // later, which for listening to a file is not a deadline anybody feels.
    let start_secs = clock.now_secs() + LOOKAHEAD_SECS;
    // Held open past the end of the buffer so that the fade-out lands on
    // silence rather than on the sample's last twenty milliseconds. `voice`
    // reads past the end as silence, which is what makes that free — see
    // `audition::voice`.
    let dur_secs = voice.secs + FADE_OUT_SECS;

    if let Some(id) = push_voice(seq, start_secs, dur_secs, voice.net) {
        *auditioning = Some(Live { id, end_secs: start_secs + dur_secs });
    }
}

/// Fade out one voice we are holding, if there is one, and forget it.
fn cut(seq: &mut Sequencer, voice: &mut Option<Live>) {
    if let Some(voice) = voice.take() {
        seq.edit_relative(voice.id, FADE_OUT_SECS, FADE_OUT_SECS);
    }
}

/// Cut every voice we have pushed and forget them.
///
/// `edit_relative` with equal end and fade times starts the fade immediately.
/// A voice that has not started yet ends before it begins, so the sequencer
/// retires it without ever rendering a sample.
fn silence(seq: &mut Sequencer, live: &mut Vec<Live>) {
    for voice in live.drain(..) {
        seq.edit_relative(voice.id, FADE_OUT_SECS, FADE_OUT_SECS);
    }
}

/// Drop voices the sequencer has already finished with, so the list tracks the
/// lookahead window rather than growing for the life of the app.
fn retire(live: &mut Vec<Live>, now_secs: f64) {
    live.retain(|voice| voice.end_secs > now_secs);
}

/// One pass of the loop: query the horizon, push whatever falls in it, and
/// return the new watermark. Split out from `run` so it can be tested without
/// threads or sleeping.
///
/// `None` means the pass was abandoned — by a stop, or by a failure that raised
/// one. Either way the loop's next tick sees the flag and cuts what is still
/// sounding, so a pass never has to unwind what it pushed.
fn schedule_pass(
    seq: &mut Sequencer,
    clock: &Clock,
    state: &SchedulerState,
    scheduled_through: Option<Mark>,
    live: &mut Vec<Live>,
) -> Option<Mark> {
    let epoch = clock.epoch();
    let now_bars = clock.now_bars();
    let horizon = clock.bars_at(clock.now_secs() + LOOKAHEAD_SECS);

    let from = match scheduled_through {
        // Never schedule into the past: if this thread stalled, skip the
        // missed events rather than firing a burst of late ones.
        Some(mark) if mark.epoch == epoch => mark.bars.max(now_bars),
        // A mark from before a reset. Bar time has moved backwards under it,
        // so it says nothing about what has been pushed: start from the
        // present, which is where the reset put bar 0.
        Some(_) | None => now_bars,
    };
    let next = Some(Mark { epoch, bars: from.max(horizon) });

    if horizon <= from {
        return next;
    }

    let events = match state.patterns.lock() {
        Ok(p) => {
            // Nothing playing yet, so claim nothing: an eval racing this pass
            // resets the clock and publishes its bindings a moment later, and
            // a watermark out at the horizon would swallow their first steps.
            if p.is_empty() {
                return Some(Mark { epoch, bars: from });
            }
            p.query(Span::new(from, horizon))
        }
        Err(e) => {
            state.fail(format!("patterns lock poisoned: {e}"));
            return None;
        }
    };
    if events.is_empty() {
        return next;
    }

    // Clone the definitions so voice lowering happens outside the lock.
    let instruments = match state.instruments.lock() {
        Ok(i) => i.clone(),
        Err(e) => {
            state.fail(format!("instruments lock poisoned: {e}"));
            return None;
        }
    };

    // A stop that landed while we were reading state wins: these events were
    // queried before it, and pushing them now would sound after the silence.
    if state.stop_requested() {
        return None;
    }

    // Gathered across the whole pass and sent in one go, rather than a message
    // per note: the MIDI thread asks the platform for its port list once per
    // batch, and a pass carries every note of the next fifth of a second.
    let mut midi: Vec<midi::Note> = Vec::new();

    for bound in events {
        let begin_secs = clock.secs_at(bound.event.begin);
        let dur_secs = clock.secs_at(bound.event.end) - begin_secs;

        // Sending MIDI takes none of what follows. There is no voice to build,
        // so no instrument to fail to build it and no tail to hold the note
        // open past its end — how long the note lasts is the whole of what the
        // gear at the other end is told, and `legato` has already scaled that
        // in `query`. The one thing `Note::from_event` does to it is take the
        // gap a wire needs off the end; see `midi::out::RELEASE_SECS`, which a
        // voice has no use for and gear cannot do without.
        let destination = match &bound.target {
            Target::Midi(destination) => destination,
            Target::Instrument(_) => {
                // The beat of the clock *this note* is played on, which is the
                // transport's divided by the speed its binding runs at: a
                // pattern at rate 2 fits two of its own beats into one of the
                // transport's, and an instrument syncing to it should hear the
                // faster one. Read per note rather than per pass, because under
                // an `accel` it is a different number for every note.
                let beat_secs = clock.beat_secs() / bound.rate;
                let instrument = bound.target.instrument().expect("matched as an instrument");
                schedule_voice(seq, state, live, &instruments, &bound, instrument,
                               begin_secs, dur_secs, beat_secs, clock.meter())?;
                continue;
            }
        };
        midi.push(midi::Note::from_event(
            destination, bound.event.value, &bound.args, begin_secs, begin_secs + dur_secs,
        ));
    }

    state.midi.play(midi);

    next
}

/// Build one voice and push it, or halt the scheduler saying why.
///
/// `None` is the same "this pass was abandoned" its caller returns — split out
/// only so that the two things a bound event can be do not have to be one
/// forty-line arm each inside the loop.
#[allow(clippy::too_many_arguments)]
fn schedule_voice(
    seq: &mut Sequencer,
    state: &SchedulerState,
    live: &mut Vec<Live>,
    instruments: &Instruments,
    bound: &BoundEvent,
    instrument: &str,
    begin_secs: f64,
    dur_secs: f64,
    beat_secs: f64,
    meter: Meter,
) -> Option<()> {
    {
        match build_voice(
            instruments, instrument, bound.event.value, &bound.args, dur_secs,
            beat_secs, meter,
        ) {
            // An instrument that will not build is a broken program, and it
            // will not build for the next event either — the same failure once
            // per step, forever. Halting on the first one puts it in front of
            // the person who can fix it instead.
            Err(e) => {
                state.fail(format!("{instrument}: {e}"));
                return None;
            }
            // An instrument's envelopes may outlast the note the pattern gave
            // it — an `env` releases from the note's end, and a `perc` shape
            // can simply be longer than the step it sits on. The event has to
            // cover that or the sequencer cuts it off mid-shape, which is the
            // rest after a note being silent where it should still be ringing.
            Ok(voice) => {
                let end_secs = begin_secs + dur_secs + voice.tail_secs;
                if let Some(id) = push_voice(seq, begin_secs, dur_secs + voice.tail_secs, voice.net)
                {
                    live.push(Live { id, end_secs });
                }
            }
        }
    }
    Some(())
}

/// Push one voice, defending against every case `Sequencer::push` asserts on.
/// Returns the event's id so stop can cut it short, or `None` if it was
/// rejected and nothing was pushed.
fn push_voice(
    seq: &mut Sequencer,
    start_secs: f64,
    dur_secs: f64,
    net: fundsp::net::Net,
) -> Option<EventId> {
    if !start_secs.is_finite() || !dur_secs.is_finite() || dur_secs <= 0.0 {
        eprintln!("scheduler: skipping voice with bad timing ({start_secs}, {dur_secs})");
        return None;
    }
    if net.inputs() != 0 || net.outputs() != 2 {
        eprintln!(
            "scheduler: voice must be 0-in/2-out, got {}-in/{}-out",
            net.inputs(),
            net.outputs()
        );
        return None;
    }

    // push asserts each fade is no longer than the event; short notes clamp.
    let half = dur_secs * 0.5;
    let fade_in = FADE_IN_SECS.min(half);
    let fade_out = FADE_OUT_SECS.min(half);

    Some(seq.push_duration(start_secs, dur_secs, Fade::Smooth, fade_in, fade_out, Box::new(net)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swync_graph::realizer::realize;
    use crate::lowerer::lower::lower;
    use crate::parser::parser::parse;
    use crate::pattern::pattern::Pattern;
    use crate::pattern::patterns::Binding;
    use crate::pattern::rate::Rate;
    use fundsp::sequencer::ReplayMode;

    fn voice_net() -> fundsp::net::Net {
        let items = parse("sin(220)\n".to_string()).unwrap();
        realize(&lower(&items).unwrap().graph).unwrap()
    }

    #[test]
    fn pushed_voice_renders_audio() {
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);
        assert!(push_voice(&mut seq, 0.0, 1.0, voice_net()).is_some());

        let mut peak = 0.0f32;
        for _ in 0..22050 {
            let (l, r) = seq.get_stereo();
            peak = peak.max(l.abs()).max(r.abs());
        }
        assert!(peak > 0.5, "voice should be audible, peak was {peak}");
    }

    /// A note shorter than the nominal fades must still push, not panic.
    #[test]
    fn very_short_notes_clamp_their_fades() {
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);
        assert!(push_voice(&mut seq, 0.0, 0.001, voice_net()).is_some());

        for _ in 0..100 {
            let _ = seq.get_stereo();
        }
    }

    #[test]
    fn bad_timing_is_skipped_not_panicked() {
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        assert!(push_voice(&mut seq, 0.0, 0.0, voice_net()).is_none());
        assert!(push_voice(&mut seq, f64::NAN, 1.0, voice_net()).is_none());
        assert!(push_voice(&mut seq, 0.0, -1.0, voice_net()).is_none());
    }

    // ---- auditioning a sample ----

    const AUDITION_RATE: f64 = 44100.0;
    /// A tenth of a second, which is about what a drum hit is.
    const AUDITION_LENGTH: usize = 4410;

    /// A buffer holding a constant, so a rendered frame either is it or is not.
    fn a_sample() -> std::sync::Arc<fundsp::wave::Wave> {
        let mut wave = fundsp::wave::Wave::new(1, AUDITION_RATE);
        for _ in 0..AUDITION_LENGTH {
            wave.push(0.5);
        }
        std::sync::Arc::new(wave)
    }

    fn an_audition() -> Audition {
        crate::audition::voice(&a_sample()).expect("should build")
    }

    /// Render `frames` and answer with the loudest thing in them.
    fn peak(seq: &mut Sequencer, frames: usize) -> f32 {
        (0..frames).fold(0.0f32, |peak, _| {
            let (l, r) = seq.get_stereo();
            peak.max(l.abs()).max(r.abs())
        })
    }

    /// The whole of what the play button does, from the ask to the sound.
    #[test]
    fn an_auditioned_sample_is_pushed_and_heard() {
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(AUDITION_RATE);
        let clock = Clock::new(AUDITION_RATE);
        let state = SchedulerState::new();
        let mut auditioning = None;

        state.audition(an_audition());
        take_audition(&mut seq, &clock, &state, &mut auditioning);
        assert!(auditioning.is_some(), "the sample should be playing");

        // The lookahead first, which is silence, and then the sample.
        let lead = (LOOKAHEAD_SECS * AUDITION_RATE) as usize;
        assert!(peak(&mut seq, lead) < 1e-6, "nothing should sound before the lead is up");
        assert!(peak(&mut seq, AUDITION_LENGTH) > 0.4, "the sample should be audible");
    }

    /// Asking twice does not play twice over: the second press replaces the
    /// first, which is what clicking down a folder of kicks means.
    #[test]
    fn a_second_audition_replaces_the_first() {
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(AUDITION_RATE);
        let clock = Clock::new(AUDITION_RATE);
        let state = SchedulerState::new();
        let mut auditioning = None;

        state.audition(an_audition());
        take_audition(&mut seq, &clock, &state, &mut auditioning);
        let first = auditioning.as_ref().map(|voice| voice.id);

        state.audition(an_audition());
        take_audition(&mut seq, &clock, &state, &mut auditioning);
        let second = auditioning.as_ref().map(|voice| voice.id);

        assert!(first.is_some() && second.is_some());
        assert_ne!(first, second, "the second press should be a new voice");
    }

    /// The button's other press. Nothing else about the scheduler moves — this
    /// is not the transport's stop.
    #[test]
    fn silencing_an_audition_ends_it() {
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(AUDITION_RATE);
        let clock = Clock::new(AUDITION_RATE);
        let state = SchedulerState::new();
        let mut auditioning = None;

        state.audition(an_audition());
        take_audition(&mut seq, &clock, &state, &mut auditioning);

        state.silence_audition();
        take_audition(&mut seq, &clock, &state, &mut auditioning);
        assert!(auditioning.is_none(), "nothing should be auditioning");

        // Well past the lead and the fade, so what is left is what the cut left.
        let lead = (LOOKAHEAD_SECS * AUDITION_RATE) as usize;
        assert!(peak(&mut seq, lead + AUDITION_LENGTH) < 1e-6, "it should be silent");
    }

    /// A pass with nobody pressing anything leaves what is playing alone. The
    /// slot is empty on all but a handful of passes in a session, and a pass
    /// that mistook that for a stop would cut every audition after 25 ms.
    #[test]
    fn a_pass_with_nothing_asked_for_changes_nothing() {
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(AUDITION_RATE);
        let clock = Clock::new(AUDITION_RATE);
        let state = SchedulerState::new();
        let mut auditioning = None;

        state.audition(an_audition());
        take_audition(&mut seq, &clock, &state, &mut auditioning);
        let playing = auditioning.as_ref().map(|voice| voice.id);

        take_audition(&mut seq, &clock, &state, &mut auditioning);
        assert_eq!(auditioning.as_ref().map(|voice| voice.id), playing);
    }

    /// The event has to outlast the buffer, or the fade-out would be laid over
    /// the sample's own last twenty milliseconds instead of over the silence
    /// after it.
    #[test]
    fn an_audition_is_held_open_past_the_end_of_the_file() {
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(AUDITION_RATE);
        let clock = Clock::new(AUDITION_RATE);
        let state = SchedulerState::new();
        let mut auditioning = None;

        state.audition(an_audition());
        take_audition(&mut seq, &clock, &state, &mut auditioning);

        let voice = auditioning.expect("should be playing");
        let played = voice.end_secs - LOOKAHEAD_SECS;
        assert!(
            played > AUDITION_LENGTH as f64 / AUDITION_RATE,
            "the note should be longer than the file, was {played}"
        );
    }

    /// A mono unit must be rejected rather than tripping push's arity assert.
    #[test]
    fn wrong_arity_voice_is_rejected() {
        use fundsp::prelude64::*;
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        let mut mono = Net::new(0, 1);
        let n = mono.push(Box::new(dc(0.5)));
        mono.connect_output(n, 0, 0);
        assert!(push_voice(&mut seq, 0.0, 1.0, mono).is_none());
    }

    /// The scheduler's own timing math: an event at bar 1 with cps 0.5
    /// starts at 2 seconds and lasts one second at 2 steps per bar.
    #[test]
    fn event_times_convert_to_seconds() {
        let clock = Clock::with_cps(44100.0, 0.5);
        let pats = Patterns {
            bindings: vec![Binding {
                target: "kick".into(),
                source: Pattern::steps([Some(1.0), Some(2.0)]).into(),
                lanes: Vec::new(),
                start: 0.0,
                bars: None, repeat: None, choice: None, button: None, rate: Rate::Fixed(1.0) }],
            ..Default::default()
        };

        let events = pats.query(Span::new(1.0, 2.0));
        assert_eq!(events.len(), 2);

        let first = &events[0].event;
        let begin_secs = clock.secs_at(first.begin);
        let dur_secs = clock.secs_at(first.end) - begin_secs;
        assert!((begin_secs - 2.0).abs() < 1e-9, "got {begin_secs}");
        assert!((dur_secs - 1.0).abs() < 1e-9, "got {dur_secs}");
    }
}

#[cfg(test)]
mod pass_tests {
    use super::*;
    use crate::pattern::pattern::Pattern;
    use crate::pattern::patterns::{Binding, Lane, LaneValues, LEGATO};
    use crate::pattern::rate::Rate;
    use crate::parser::parser::parse;
    use fundsp::sequencer::ReplayMode;

    fn state_with_kick(steps: Vec<Option<f64>>) -> SchedulerState {
        let s = SchedulerState::new();
        let ast = parse("fn kick(f) = sin(f)\n".to_string()).unwrap();
        *s.instruments.lock().unwrap() = Instruments::from_program(&ast);
        *s.patterns.lock().unwrap() = Patterns {
            bindings: vec![Binding {
                target: "kick".into(),
                source: Pattern::steps(steps).into(),
                lanes: Vec::new(),
                start: 0.0,
                bars: None, repeat: None, choice: None, button: None, rate: Rate::Fixed(1.0) }],
            ..Default::default()
        };
        s
    }

    /// The same, playing gear instead of an instrument — and holding the
    /// receiving end, so a test can see what the pass decided to send.
    fn state_playing_midi(steps: Vec<Option<f64>>, lanes: Vec<Lane>)
        -> (SchedulerState, std::sync::mpsc::Receiver<midi::Command>)
    {
        let (out, rx) = midi::Out::collecting();
        let s = SchedulerState::new().with_midi(out);
        *s.patterns.lock().unwrap() = Patterns {
            bindings: vec![Binding {
                target: Target::Midi(midi::Destination {
                    selector: crate::midi::ports::Selector::Name("deluge".into()),
                    channel: 1,
                }),
                source: Pattern::steps(steps).into(),
                lanes,
                start: 0.0,
                bars: None, repeat: None, choice: None, button: None, rate: Rate::Fixed(1.0) }],
            ..Default::default()
        };
        (s, rx)
    }

    /// Everything the pass sent, flattened out of however many batches it took.
    fn sent(rx: &std::sync::mpsc::Receiver<midi::Command>) -> Vec<midi::Note> {
        rx.try_iter()
            .flat_map(|c| match c {
                midi::Command::Play(notes) => notes,
                // Nothing else this thread sends carries notes, and these
                // tests are about the notes.
                _ => Vec::new(),
            })
            .collect()
    }

    /// The whole point of the second kind of `Target`: a binding that names
    /// gear leaves by a different road, and nothing about the pattern, the
    /// clock or the lookahead changes because of it.
    #[test]
    fn a_binding_that_names_gear_is_sent_rather_than_pushed() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let (state, rx) = state_playing_midi(vec![Some(60.0), Some(64.0)], Vec::new());
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);
        let mut live = Vec::new();

        // Two passes with the bar moving under them, because one pass only
        // ever reaches the lookahead — a fifth of a second, which at a bar a
        // second is the first of these two steps and not the second.
        let mut mark = schedule_pass(&mut seq, &clock, &state, None, &mut live);
        clock.advance(44100 / 2);
        mark = schedule_pass(&mut seq, &clock, &state, mark, &mut live);
        assert!(mark.is_some(), "neither pass should have been abandoned");

        let notes = sent(&rx);
        assert_eq!(notes.iter().map(|n| n.note).collect::<Vec<_>>(), vec![60, 64]);
        assert!(live.is_empty(), "nothing should have been pushed into the sequencer");
    }

    /// A note's two ends are decided together, here, and travel together — a
    /// note whose off was still to be worked out is one that hangs if anything
    /// goes wrong in between.
    #[test]
    fn a_sent_note_carries_both_of_its_ends() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let (state, rx) = state_playing_midi(vec![Some(60.0)], Vec::new());
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);

        schedule_pass(&mut seq, &clock, &state, None, &mut Vec::new());

        let note = sent(&rx).remove(0);
        assert!(note.off_secs > note.on_secs, "got {note:?}");
    }

    /// `legato` scales the event in `query`, above both kinds of target, so
    /// there is nothing MIDI-specific about it — which is exactly what this
    /// pins.
    #[test]
    fn legato_shortens_a_sent_note_as_it_shortens_a_voice() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let staccato = Lane {
            name: LEGATO.to_string(),
            values: LaneValues::Steps(Pattern::steps(vec![Some(0.25)])),
        };
        let (long, long_rx) = state_playing_midi(vec![Some(60.0)], Vec::new());
        let (short, short_rx) = state_playing_midi(vec![Some(60.0)], vec![staccato]);
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);

        schedule_pass(&mut seq, &clock, &long, None, &mut Vec::new());
        schedule_pass(&mut seq, &clock, &short, None, &mut Vec::new());

        let held = |notes: Vec<midi::Note>| notes[0].off_secs - notes[0].on_secs;
        assert!(held(sent(&short_rx)) < held(sent(&long_rx)) / 2.0);
    }

    /// The room going quiet has to include the gear in it. Nothing else would
    /// release those notes — a MIDI note is held until something says
    /// otherwise, so a stop that only cut the sequencer would leave whatever
    /// was sounding droning on after the silence.
    #[test]
    fn a_stop_reaches_the_gear_as_well_as_the_sequencer() {
        let (out, rx) = midi::Out::collecting();
        out.stop();
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![midi::Command::Stop]);
    }

    /// The bug from the field, end to end: a pad whose `env` says it releases
    /// over half a second was being cut off in twenty milliseconds, because
    /// letting a key go ended the *sequencer event* instead of the
    /// *envelope*. What that sounds like is a click where a release was
    /// written.
    ///
    /// This is the scheduler's half of it — `voice::gate_tests` covers the
    /// envelope itself. Together they are the two places the fix had to land.
    #[test]
    fn letting_a_key_go_does_not_cut_the_instrument_short() {
        use crate::midi::input;
        let _bus = input::exclusive();

        let rate = 44100.0;
        let clock = Clock::new(rate);
        let (state, _rx) = midi_state("fn pad(n) = saw(n.m2h) * env(0.01, 0.05, 0.7, 0.5, dur)\n");
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(rate);
        let mut held = Vec::new();
        let mut played = 0;

        let slot = input::slot_for(&crate::midi::ports::Selector::Name("keys".into())).unwrap();
        assert_eq!(slot, 0, "the binding below was built against slot 0");

        // A key goes down, and the pass that follows turns it into a voice.
        input::inject(slot, &[0x90, 60, 100]);
        play_live_notes(&mut seq, &clock, &state, &mut held, &mut played);
        assert_eq!(held.len(), 1, "the key should be holding a voice open");

        // Past the lead and well into the sustain.
        let sounding = peak_over(&mut seq, (0.2 * rate) as usize);
        assert!(sounding > 0.05, "the pad should be sounding while the key is down");

        clock.advance((0.2 * rate) as u64);
        input::inject(slot, &[0x80, 60, 0]);
        play_live_notes(&mut seq, &clock, &state, &mut held, &mut played);
        assert!(held.is_empty(), "the key should no longer be holding anything");

        // The whole of the bug is here, and *where* it is measured is the
        // point. A peak taken from the moment of release would catch the
        // sound still there in the first twenty milliseconds and pass either
        // way — the cut is a cut precisely because everything before it is
        // fine. So skip past where a cut would have landed, and listen to
        // what is left.
        peak_over(&mut seq, (0.05 * rate) as usize);
        let still_ringing = peak_over(&mut seq, (0.05 * rate) as usize);
        assert!(
            still_ringing > 0.02,
            "a tenth of a second into a half-second release the pad should still be \
             sounding, got {still_ringing} — this is the click the gate exists to prevent"
        );

        // And it does end, on the instrument's own schedule rather than never.
        peak_over(&mut seq, (0.5 * rate) as usize);
        let after = peak_over(&mut seq, (0.1 * rate) as usize);
        assert!(after < 0.01, "the release should have finished, got {after}");
    }

    /// A stop is the other way round, and deliberately: the room goes quiet
    /// now. Letting every held note run its own release would leave a pad
    /// ringing for two seconds after somebody pressed stop.
    #[test]
    fn a_stop_cuts_a_held_key_rather_than_releasing_it() {
        use crate::midi::input;
        let _bus = input::exclusive();

        let rate = 44100.0;
        let clock = Clock::new(rate);
        let (state, _rx) = midi_state("fn pad(n) = saw(n.m2h) * env(0.01, 0.05, 0.7, 0.5, dur)\n");
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(rate);
        let mut held = Vec::new();

        let slot = input::slot_for(&crate::midi::ports::Selector::Name("keys".into())).unwrap();
        input::inject(slot, &[0x90, 60, 100]);
        play_live_notes(&mut seq, &clock, &state, &mut held, &mut 0);
        peak_over(&mut seq, (0.2 * rate) as usize);

        release_all(&mut seq, &mut held);
        // Past the short fade a cut uses, and nothing like far enough for the
        // half-second release the instrument asked for.
        peak_over(&mut seq, (0.05 * rate) as usize);
        let after = peak_over(&mut seq, (0.05 * rate) as usize);
        assert!(after < 0.01, "stop should have silenced it, got {after}");
    }

    /// A keyboard bound to an instrument, and the receiving end of what it
    /// would have sent if it were bound to a port instead.
    fn midi_state(src: &str) -> (SchedulerState, std::sync::mpsc::Receiver<midi::Command>) {
        let (out, rx) = midi::Out::collecting();
        let s = SchedulerState::new().with_midi(out);
        let ast = parse(src.to_string()).unwrap();
        *s.instruments.lock().unwrap() = Instruments::from_program(&ast);
        *s.patterns.lock().unwrap() = Patterns {
            bindings: vec![Binding {
                target: "pad".into(),
                source: crate::pattern::patterns::SourceOf::Live(
                    crate::swync_graph::environment::Source { slot: 0, channel: None },
                ),
                lanes: Vec::new(),
                start: 0.0,
                bars: None, repeat: None, choice: None, button: None, rate: Rate::Fixed(1.0) }],
            ..Default::default()
        };
        (s, rx)
    }

    fn peak_over(seq: &mut Sequencer, frames: usize) -> f32 {
        let mut peak = 0.0f32;
        for _ in 0..frames {
            let (l, r) = seq.get_stereo();
            peak = peak.max(l.abs()).max(r.abs());
        }
        peak
    }

    /// A pass for the tests that do not care which voices came out of it.
    fn pass(
        seq: &mut Sequencer,
        clock: &Clock,
        state: &SchedulerState,
        from: Option<Mark>,
    ) -> Option<Mark> {
        schedule_pass(seq, clock, state, from, &mut Vec::new())
    }

    /// End to end: a pattern plus an instrument becomes audible voices in the
    /// sequencer, with no thread and no audio device involved.
    #[test]
    fn a_pass_schedules_audible_voices() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let state = state_with_kick(vec![Some(220.0), Some(330.0), Some(440.0), Some(550.0)]);
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);

        let mark = pass(&mut seq, &clock, &state, None);
        // cps 1.0, 0.2s lookahead -> horizon is bar 0.2.
        assert!((mark.unwrap().bars - 0.2).abs() < 1e-9, "watermark: {mark:?}");

        // Only the step at bar 0.0 falls inside [0, 0.2).
        assert!(peak_over(&mut seq, 4410) > 0.5, "the first step should sound");
    }

    /// The whole lane path, end to end: an eval's named argument becomes a
    /// sampled value that reaches the instrument's parameter and is audible.
    #[test]
    fn a_pass_carries_lane_values_into_its_voices() {
        use crate::lowerer::lower::lower;
        use crate::pattern::patterns::Patterns;

        let src = "fn tone(n, mul = 1) = sin(n * mul)\nplay([110], tone, mul: [4])\n";
        let ast = parse(src.to_string()).unwrap();
        let lowered = lower(&ast).expect("lower failed");

        let state = SchedulerState::new();
        *state.instruments.lock().unwrap() = Instruments::from_program(&ast);
        *state.patterns.lock().unwrap() =
            Patterns { bindings: lowered.bindings, ..Default::default() };

        let clock = Clock::with_cps(44100.0, 1.0);
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);
        pass(&mut seq, &clock, &state, None);

        // 110 Hz times a `mul` of 4 is 440 Hz, over the 0.1 s rendered here.
        let s: Vec<f32> = (0..4410).map(|_| seq.get_stereo().0).collect();
        let crossings = s.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        assert!((crossings as i64 - 44).abs() <= 2, "expected ~44 crossings, got {crossings}");
    }

    /// The whole beat path, from written source to a rendered voice: `qvh` is
    /// the transport's quarter divided by the speed the pattern runs at, so the
    /// same instrument sweeps twice as fast under a `play` at rate 2.
    ///
    /// At cps 1 a bar is a second and the quarter a quarter of one, which is
    /// 4 Hz; times the 32 written here that is 128 Hz at rate 1 and 256 at
    /// rate 2. Counted in zero crossings over a tenth of a second, the same way
    /// the lane path is checked above.
    #[test]
    fn the_rate_a_pattern_runs_at_reaches_its_instrument_as_the_beat() {
        use crate::lowerer::lower::lower;
        use crate::pattern::patterns::Patterns;

        fn crossings_at_rate(rate: &str) -> i64 {
            let src = format!("fn tone() = sin(qvh * 32)\nplay([\\], tone, {rate})\n");
            let ast = parse(src).unwrap();
            let lowered = lower(&ast).expect("lower failed");

            let state = SchedulerState::new();
            *state.instruments.lock().unwrap() = Instruments::from_program(&ast);
            *state.patterns.lock().unwrap() =
                Patterns { bindings: lowered.bindings, ..Default::default() };

            let clock = Clock::with_cps(44100.0, 1.0);
            let mut seq = Sequencer::new(0, 2, ReplayMode::None);
            seq.set_sample_rate(44100.0);
            pass(&mut seq, &clock, &state, None);

            let s: Vec<f32> = (0..4410).map(|_| seq.get_stereo().0).collect();
            s.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as i64
        }

        let plain = crossings_at_rate("1");
        let fast = crossings_at_rate("2");
        assert!((plain - 13).abs() <= 2, "expected ~13 crossings at rate 1, got {plain}");
        assert!((fast - 26).abs() <= 2, "expected ~26 crossings at rate 2, got {fast}");
    }

    /// The whole rate-curve path, from written source to scheduled voices: an
    /// `accel` reaches the scheduler as something it can evaluate itself, and
    /// the notes it pushes really do close up.
    ///
    /// Nothing about the scheduler changed for this. It queries a span and
    /// pushes what comes back, which is the reason a rate had to become a shape
    /// the pattern layer could answer with rather than anything read from the
    /// audio graph — this thread runs a lookahead ahead of the audio clock, and
    /// asks about time no graph has rendered yet.
    #[test]
    fn an_accelerating_pattern_schedules_closing_intervals() {
        use crate::lowerer::lower::lower;
        use crate::pattern::patterns::Patterns;

        let src = "fn kick(f) = sin(f)\nplayn([220], kick, 8, accel(1, 3, 4))\n";
        let ast = parse(src.to_string()).unwrap();
        let lowered = lower(&ast).expect("lower failed");

        let state = SchedulerState::new();
        *state.instruments.lock().unwrap() = Instruments::from_program(&ast);
        *state.patterns.lock().unwrap() =
            Patterns { bindings: lowered.bindings, ..Default::default() };

        // One pass wide enough to hold the whole section, so the onsets can be
        // read off in one go rather than across ticks.
        let clock = Clock::with_cps(44100.0, 1.0);
        let patterns = state.patterns.lock().unwrap().clone();
        let onsets: Vec<f64> = patterns
            .query(Span::new(0.0, 8.0))
            .iter()
            .map(|b| b.event.begin)
            .collect();

        assert_eq!(onsets.len(), 8, "eight passes: {onsets:?}");
        let gaps: Vec<f64> = onsets.windows(2).map(|w| w[1] - w[0]).collect();
        for pair in gaps.windows(2) {
            assert!(pair[1] < pair[0], "intervals should close up: {gaps:?}");
        }
        assert!(*onsets.last().unwrap() < 4.0, "all of it inside its own four bars");

        // And they are real voices, not merely times: the first one sounds.
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);
        pass(&mut seq, &clock, &state, None);
        assert!(peak_over(&mut seq, 4410) > 0.5, "the first step should sound");
    }

    /// Legato shortens the event, and the scheduler derives the voice's own
    /// lifetime from that same span — so a staccato note really does stop.
    #[test]
    fn legato_cuts_the_voice_short() {
        use crate::lowerer::lower::lower;
        use crate::pattern::patterns::Patterns;

        let render = |src: &str| {
            let ast = parse(src.to_string()).unwrap();
            let lowered = lower(&ast).expect("lower failed");
            let state = SchedulerState::new();
            *state.instruments.lock().unwrap() = Instruments::from_program(&ast);
            *state.patterns.lock().unwrap() =
                Patterns { bindings: lowered.bindings, ..Default::default() };

            // Slow enough that one step spans far beyond the rendered window:
            // at cps 0.25 a natural note lasts 4 seconds.
            let clock = Clock::with_cps(44100.0, 0.25);
            let mut seq = Sequencer::new(0, 2, ReplayMode::None);
            seq.set_sample_rate(44100.0);
            pass(&mut seq, &clock, &state, None);

            let s: Vec<f32> = (0..8820).map(|_| seq.get_stereo().0).collect();
            // 0.15 - 0.2 s: past a note shortened to 0.1 s and its fade out,
            // but a long way inside the natural one.
            s[6615..].iter().fold(0.0f32, |m, v| m.max(v.abs()))
        };

        let held = render("fn tone(n) = sin(n)\nplay([220], tone)\n");
        let short = render("fn tone(n) = sin(n)\nplay([220], tone, legato: 0.025)\n");

        assert!(held > 0.5, "the held note should still be sounding, got {held}");
        assert!(short < 0.01, "legato should have cut it short, got {short}");
    }

    /// The reported bug: an instrument with a long `env` release went silent at
    /// the step boundary — a rest after it was silence where the note should
    /// still have been ringing out into it. The release sits after the note
    /// now, and the sequencer event is given that much more room to hold it.
    #[test]
    fn an_env_release_rings_on_past_the_note() {
        use crate::lowerer::lower::lower;
        use crate::pattern::patterns::Patterns;

        let render = |src: &str| {
            let ast = parse(src.to_string()).unwrap();
            let lowered = lower(&ast).expect("lower failed");
            let state = SchedulerState::new();
            *state.instruments.lock().unwrap() = Instruments::from_program(&ast);
            *state.patterns.lock().unwrap() =
                Patterns { bindings: lowered.bindings, ..Default::default() };

            // cps 1.0 over two steps: the note is [0, 0.5) and a rest follows.
            let clock = Clock::with_cps(44100.0, 1.0);
            let mut seq = Sequencer::new(0, 2, ReplayMode::None);
            seq.set_sample_rate(44100.0);
            pass(&mut seq, &clock, &state, None);

            (0..44100).map(|_| seq.get_stereo().0).collect::<Vec<f32>>()
        };

        // Sustains flat for the note, then releases over 0.3 s — done at 0.8.
        let s = render("fn tone(n) = sin(n) * env(0.005, 0.005, 1, 0.3, dur)\nplay([220, `], tone)\n");
        let peak = |from: f64, to: f64| {
            s[(from * 44100.0) as usize..(to * 44100.0) as usize]
                .iter()
                .fold(0.0f32, |m, v| m.max(v.abs()))
        };

        assert!(peak(0.1, 0.4) > 0.5, "the note itself should sound, got {}", peak(0.1, 0.4));
        assert!(peak(0.55, 0.6) > 0.3, "the release should ring into the rest, got {}",
                peak(0.55, 0.6));
        assert!(peak(0.85, 1.0) < 0.02, "the release should be over, got {}", peak(0.85, 1.0));

        // Without an envelope nothing is added: the note ends with its step,
        // which is what says the tail above came from the `env`.
        let bare = render("fn tone(n) = sin(n)\nplay([220, `], tone)\n");
        let after = bare[(0.55 * 44100.0) as usize..(0.6 * 44100.0) as usize]
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(after < 0.02, "a voice with no release should stop at its step, got {after}");
    }

    /// The same fault reached the other way: a `perc` drum shapes itself from
    /// the onset, so on a step shorter than the shape it was cut off partway
    /// down rather than ringing into the next step.
    #[test]
    fn a_drum_longer_than_its_step_rings_into_the_next() {
        use crate::lowerer::lower::lower;
        use crate::pattern::patterns::Patterns;

        // A 0.3 s drum on 16ths at cps 1.0, which is a 0.0625 s step.
        let src = "fn kick(f) = sin(f) * perc(0.001, 0.3)\n\
                   play([55, `, `, `, `, `, `, `, `, `, `, `, `, `, `, `], kick)\n";
        let ast = parse(src.to_string()).unwrap();
        let lowered = lower(&ast).expect("lower failed");
        let state = SchedulerState::new();
        *state.instruments.lock().unwrap() = Instruments::from_program(&ast);
        *state.patterns.lock().unwrap() =
            Patterns { bindings: lowered.bindings, ..Default::default() };

        let clock = Clock::with_cps(44100.0, 1.0);
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);
        let mut live = Vec::new();
        schedule_pass(&mut seq, &clock, &state, None, &mut live);

        assert_eq!(live.len(), 1, "one drum should have been pushed");
        assert!(
            (live[0].end_secs - 0.301).abs() < 1e-6,
            "the voice should last the whole shape, got {}",
            live[0].end_secs,
        );

        // Well past the 0.0625 s step, halfway down a shape that used to have
        // been cut off there.
        let s: Vec<f32> = (0..22050).map(|_| seq.get_stereo().0).collect();
        let peak = |from: f64, to: f64| {
            s[(from * 44100.0) as usize..(to * 44100.0) as usize]
                .iter()
                .fold(0.0f32, |m, v| m.max(v.abs()))
        };
        assert!(peak(0.1, 0.15) > 0.3, "the drum should still be falling, got {}", peak(0.1, 0.15));
        assert!(peak(0.35, 0.5) < 0.02, "and be finished by 0.35 s, got {}", peak(0.35, 0.5));
    }

    /// The same fault a third way, and the one that showed it was not really
    /// about envelopes: an instrument whose echoes come back after its envelope
    /// has finished was cut off at the envelope, so only the first repeat or
    /// two were ever heard. The voice now lasts the whole chain — the release
    /// rings out and the delay hands that back later still.
    #[test]
    fn an_echo_that_arrives_after_the_envelope_is_still_heard() {
        use crate::lowerer::lower::lower;
        use crate::pattern::patterns::Patterns;

        // A 0.1 s blip echoed at 0.4 and 0.8 s, on a note of 0.5 s with a
        // 0.1 s release — the second echo lands well past both.
        let src = "fn ping(n) = { \
                     let dry = sin(n) * env(0.001, 0.05, 0.2, 0.1, dur)\n\
                     dry + delay(dry, 0.4) * 0.7 + delay(dry, 0.8) * 0.5 \
                   }\n\
                   play([220, `], ping)\n";
        let ast = parse(src.to_string()).unwrap();
        let lowered = lower(&ast).expect("lower failed");
        let state = SchedulerState::new();
        *state.instruments.lock().unwrap() = Instruments::from_program(&ast);
        *state.patterns.lock().unwrap() =
            Patterns { bindings: lowered.bindings, ..Default::default() };

        let clock = Clock::with_cps(44100.0, 1.0);
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);
        let mut live = Vec::new();
        schedule_pass(&mut seq, &clock, &state, None, &mut live);

        assert_eq!(live.len(), 1, "one note should have been pushed");
        assert!(
            (live[0].end_secs - 1.4).abs() < 1e-6,
            "0.5 s note, 0.1 s release, 0.8 s echo, got {}",
            live[0].end_secs,
        );

        let s: Vec<f32> = (0..(44100.0 * 1.5) as usize).map(|_| seq.get_stereo().0).collect();
        let peak = |from: f64, to: f64| {
            s[(from * 44100.0) as usize..(to * 44100.0) as usize]
                .iter()
                .fold(0.0f32, |m, v| m.max(v.abs()))
        };

        assert!(peak(0.0, 0.1) > 0.3, "the dry blip, got {}", peak(0.0, 0.1));
        assert!(peak(0.4, 0.5) > 0.2, "the first echo, got {}", peak(0.4, 0.5));
        assert!(peak(0.8, 0.9) > 0.1, "the second echo, past the release and the note's own \
                                       step, got {}", peak(0.8, 0.9));
    }

    /// The tail is added to the note the pattern gave, so `legato` shortens
    /// what is held and the release still gets its own time afterwards.
    #[test]
    fn legato_shortens_the_note_and_keeps_the_release() {
        use crate::lowerer::lower::lower;
        use crate::pattern::patterns::Patterns;

        let ast = parse(
            "fn tone(n) = sin(n) * env(0.005, 0.005, 1, 0.3, dur)\n\
             play([220], tone, legato: 0.2)\n"
                .to_string(),
        )
        .unwrap();
        let lowered = lower(&ast).expect("lower failed");
        let state = SchedulerState::new();
        *state.instruments.lock().unwrap() = Instruments::from_program(&ast);
        *state.patterns.lock().unwrap() =
            Patterns { bindings: lowered.bindings, ..Default::default() };

        // One step a bar at cps 1.0: a legato of 0.2 holds it for 0.2 s.
        let clock = Clock::with_cps(44100.0, 1.0);
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);
        let mut live = Vec::new();
        schedule_pass(&mut seq, &clock, &state, None, &mut live);

        assert_eq!(live.len(), 1, "one note should have been pushed");
        assert!(
            (live[0].end_secs - 0.5).abs() < 1e-6,
            "0.2 s held plus a 0.3 s release, got {}",
            live[0].end_secs,
        );
    }

    /// A pan lane rides through the binding as an ordinary lane value and is
    /// spent on the voice at the far end of the pipeline, so the only proof
    /// that all of it is wired together is source in and two channels out.
    #[test]
    fn a_pan_lane_reaches_the_stereo_output() {
        use crate::lowerer::lower::lower;
        use crate::pattern::patterns::Patterns;

        let render = |src: &str| {
            let ast = parse(src.to_string()).unwrap();
            let lowered = lower(&ast).expect("lower failed");
            let state = SchedulerState::new();
            *state.instruments.lock().unwrap() = Instruments::from_program(&ast);
            *state.patterns.lock().unwrap() =
                Patterns { bindings: lowered.bindings, ..Default::default() };

            let clock = Clock::with_cps(44100.0, 1.0);
            let mut seq = Sequencer::new(0, 2, ReplayMode::None);
            seq.set_sample_rate(44100.0);
            pass(&mut seq, &clock, &state, None);

            // Past the voice's own fade-in, so this measures the placement
            // rather than the ramp towards it.
            let frames: Vec<(f32, f32)> = (0..4410).map(|_| seq.get_stereo()).collect();
            frames[2205..].iter().fold((0.0f32, 0.0f32), |(l, r), (a, b)| {
                (l.max(a.abs()), r.max(b.abs()))
            })
        };

        let (left, right) = render("fn tone(n) = sin(n)\nplay([220], tone, pan: -1)\n");
        assert!(left > 0.9, "the voice should be in the left channel, got {left}");
        assert!(right < 1e-4, "the right channel should be empty, got {right}");

        // The same program without the lane is centred, which is what says the
        // lane did it rather than the pipeline being lopsided all along.
        let (left, right) = render("fn tone(n) = sin(n)\nplay([220], tone)\n");
        assert!((left - right).abs() < 1e-4, "unpanned should be centred: {left} vs {right}");
    }

    /// Re-running with no clock movement must not re-schedule the same notes.
    #[test]
    fn repeated_passes_do_not_double_trigger() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let state = state_with_kick(vec![Some(220.0)]);
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);

        let mark = pass(&mut seq, &clock, &state, None);
        let peak_once = peak_over(&mut seq, 2205);

        let mut seq2 = Sequencer::new(0, 2, ReplayMode::None);
        seq2.set_sample_rate(44100.0);
        let mark2 = pass(&mut seq2, &clock, &state, mark);
        assert_eq!(mark, mark2, "watermark should not move without the clock");
        assert!(peak_over(&mut seq2, 2205) < peak_once * 0.5,
                "second pass should have scheduled nothing");
    }

    /// Dragging the tempo down pulls the horizon back behind the watermark.
    /// The notes out there have already been pushed, so the pass must wait for
    /// the horizon to catch up rather than schedule that window again.
    #[test]
    fn slowing_down_does_not_re_push_the_lookahead_window() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let state = state_with_kick(vec![Some(220.0), Some(330.0), Some(440.0), Some(550.0)]);
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);
        let mut live = Vec::new();

        let mark = schedule_pass(&mut seq, &clock, &state, None, &mut live);
        let pushed = live.len();
        assert!(pushed > 0, "the first pass should have pushed a voice");

        clock.set_cps(0.25);
        let after = schedule_pass(&mut seq, &clock, &state, mark, &mut live);

        assert_eq!(live.len(), pushed, "no voice should have been pushed twice");
        assert_eq!(
            after.unwrap().bars,
            mark.unwrap().bars,
            "the watermark must survive a tempo change",
        );
    }

    /// A stalled scheduler skips missed events instead of firing them late.
    #[test]
    fn a_stale_watermark_is_clamped_to_now() {
        let clock = Clock::with_cps(44100.0, 1.0);
        clock.advance(44100 * 10); // ten seconds elapsed
        let state = state_with_kick(vec![Some(220.0)]);
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);

        // Watermark claims we only got as far as bar 0, ten bars ago.
        let stale = Mark { epoch: clock.epoch(), bars: 0.0 };
        let mark = pass(&mut seq, &clock, &state, Some(stale)).unwrap();
        assert!(mark.bars >= 10.0, "should jump to the present, got {mark:?}");
    }

    /// An empty pattern set is cheap and silent, which is the state before the
    /// first eval.
    #[test]
    fn empty_patterns_schedule_nothing() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let state = SchedulerState::new();
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);

        let mark = pass(&mut seq, &clock, &state, None);
        assert!(mark.is_some());
        assert!(peak_over(&mut seq, 4410) == 0.0);
    }

    /// Everything a reporter was told, in order.
    fn recording(state: &SchedulerState) -> Arc<Mutex<Vec<Diagnostic>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        state.on_error(move |d| sink.lock().unwrap().push(d));
        seen
    }

    /// A binding whose instrument cannot be built, which is what a program with
    /// a typo inside an `fn` reaches the scheduler as.
    fn state_with_broken_instrument() -> SchedulerState {
        let s = SchedulerState::new();
        let ast = parse("fn kick808(f) = sin(onsset)\n".to_string()).unwrap();
        *s.instruments.lock().unwrap() = Instruments::from_program(&ast);
        *s.patterns.lock().unwrap() = Patterns {
            bindings: vec![Binding {
                target: "kick808".into(),
                source: Pattern::steps(vec![Some(50.0), Some(50.0)]).into(),
                lanes: Vec::new(),
                start: 0.0,
                bars: None, repeat: None, choice: None, button: None, rate: Rate::Fixed(1.0) }],
            ..Default::default()
        };
        s
    }

    /// The reported behaviour: an instrument that will not build used to log
    /// `scheduler: kick808: unbound name: onsset` once per step and play on
    /// silently. It must stop instead, and say so.
    #[test]
    fn a_broken_instrument_halts_playback_and_is_reported() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let state = state_with_broken_instrument();
        let seen = recording(&state);
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        let mut live = Vec::new();

        let mark = schedule_pass(&mut seq, &clock, &state, None, &mut live);
        assert!(mark.is_none(), "a failed pass must not claim a watermark");
        assert!(live.is_empty(), "nothing should have been pushed");

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "one failure, one report: {seen:?}");
        assert_eq!(seen[0].stage, Stage::Scheduler);
        assert!(
            seen[0].message.contains("kick808") && seen[0].message.contains("unbound name"),
            "the message should name the instrument and the fault: {}",
            seen[0].message,
        );
    }

    /// Ceasing means ceasing: the bindings are dropped and the stop flag is up,
    /// so the loop's next tick cuts the lookahead window and no later pass
    /// finds the same broken instrument to report again.
    #[test]
    fn a_failure_stops_the_patterns_rather_than_reporting_every_step() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let state = state_with_broken_instrument();
        let seen = recording(&state);
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);

        pass(&mut seq, &clock, &state, None);
        assert!(state.patterns.lock().unwrap().is_empty(), "the bindings should be gone");
        assert!(state.take_stop(), "the pushed voices should have been marked for cutting");

        // Several ticks on, with the clock moving under it.
        for _ in 0..4 {
            clock.advance(44100 / 4);
            pass(&mut seq, &clock, &state, None);
        }
        assert_eq!(seen.lock().unwrap().len(), 1, "the failure should be reported once");
    }

    /// A pattern naming an instrument that does not exist is the same kind of
    /// fault, reached by a different route: nothing to build rather than
    /// something that will not build.
    #[test]
    fn a_missing_instrument_is_reported_too() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let state = SchedulerState::new();
        let seen = recording(&state);
        *state.patterns.lock().unwrap() = Patterns {
            bindings: vec![Binding {
                target: "ghost".into(),
                source: Pattern::steps(vec![Some(1.0)]).into(),
                lanes: Vec::new(),
                start: 0.0,
                bars: None, repeat: None, choice: None, button: None, rate: Rate::Fixed(1.0) }],
            ..Default::default()
        };
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);

        assert!(pass(&mut seq, &clock, &state, None).is_none());
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].message.contains("ghost"), "got: {}", seen[0].message);
    }

    /// A scheduler nobody is listening to must still stop — the reporter is
    /// installed by the app, and every test above this one runs without it.
    #[test]
    fn a_failure_without_a_reporter_still_halts() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let state = state_with_broken_instrument();
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);

        assert!(pass(&mut seq, &clock, &state, None).is_none());
        assert!(state.patterns.lock().unwrap().is_empty());
    }

    /// A working pattern reports nothing, which is what says the reports above
    /// come from the fault rather than from playing at all.
    #[test]
    fn a_good_pass_reports_nothing() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let state = state_with_kick(vec![Some(220.0)]);
        let seen = recording(&state);
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);

        assert!(pass(&mut seq, &clock, &state, None).is_some());
        assert!(seen.lock().unwrap().is_empty());
    }

    /// The bug this guards: a stop that clears the patterns still leaves the
    /// voices already pushed into the lookahead window sounding.
    #[test]
    fn stop_cuts_a_sounding_voice() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let state = state_with_kick(vec![Some(220.0)]);
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);

        let mut live = Vec::new();
        schedule_pass(&mut seq, &clock, &state, None, &mut live);
        assert_eq!(live.len(), 1, "the pass should have pushed one voice");

        // The voice lasts a full bar; interrupt it 50ms in.
        assert!(peak_over(&mut seq, 2205) > 0.5, "voice should be sounding");

        silence(&mut seq, &mut live);
        assert!(live.is_empty(), "stop should forget the voices it cut");

        // Render past the fade out, then listen for what should be silence.
        let _ = peak_over(&mut seq, 1323); // 30ms, comfortably past a 20ms fade
        assert_eq!(peak_over(&mut seq, 22050), 0.0, "nothing should sound after stop");
    }

    /// Voices in the lookahead window have not started yet; stop must cancel
    /// them rather than let them fire on schedule.
    #[test]
    fn stop_cancels_a_voice_that_has_not_started() {
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);

        let items = parse("sin(220)\n".to_string()).unwrap();
        let net = crate::swync_graph::realizer::realize(
            &crate::lowerer::lower::lower(&items).unwrap().graph,
        )
        .unwrap();

        let id = push_voice(&mut seq, 0.1, 0.5, net).unwrap();
        let mut live = vec![Live { id, end_secs: 0.6 }];

        silence(&mut seq, &mut live);
        assert_eq!(peak_over(&mut seq, 44100), 0.0, "the voice should never sound");
    }

    /// A stop landing mid-pass must abort it, or the pass pushes voices the
    /// stop has already walked past.
    #[test]
    fn a_pending_stop_aborts_the_pass() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let state = state_with_kick(vec![Some(220.0)]);
        state.request_stop();

        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);
        let mut live = Vec::new();

        let mark = schedule_pass(&mut seq, &clock, &state, None, &mut live);
        assert!(mark.is_none(), "an aborted pass must not claim a watermark");
        assert!(live.is_empty(), "nothing should have been pushed");
        assert_eq!(peak_over(&mut seq, 4410), 0.0);
    }

    /// The live list is a window, not a log: finished voices fall out of it.
    #[test]
    fn finished_voices_are_retired() {
        let clock = Clock::with_cps(44100.0, 1.0);
        let state = state_with_kick(vec![Some(220.0), Some(330.0), Some(440.0), Some(550.0)]);
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);
        let mut live = Vec::new();

        // Each step is a quarter bar, so voice n runs [n/4, (n+1)/4).
        let mut mark = None;
        for _ in 0..4 {
            mark = schedule_pass(&mut seq, &clock, &state, mark, &mut live);
            clock.advance(44100 / 4);
        }
        assert!(live.len() > 1, "several voices should be in flight");

        retire(&mut live, clock.now_secs());
        assert!(
            live.iter().all(|v| v.end_secs > clock.now_secs()),
            "only unfinished voices should remain",
        );
    }
}

#[cfg(test)]
mod start_position_tests {
    use super::*;
    use crate::parser::parser::parse;
    use crate::pattern::pattern::Pattern;
    use crate::pattern::patterns::Binding;
    use crate::pattern::rate::Rate;
    use fundsp::sequencer::ReplayMode;

    fn state_with(steps: Vec<Option<f64>>) -> SchedulerState {
        let s = SchedulerState::new();
        let ast = parse("fn k(n) = sin(n)\n".to_string()).unwrap();
        *s.instruments.lock().unwrap() = Instruments::from_program(&ast);
        *s.patterns.lock().unwrap() = Patterns {
            bindings: vec![Binding {
                target: "k".into(),
                source: Pattern::steps(steps).into(),
                lanes: Vec::new(),
                start: 0.0,
                bars: None, repeat: None, choice: None, button: None, rate: Rate::Fixed(1.0) }],
            ..Default::default()
        };
        s
    }

    fn steps_over(state: &SchedulerState, clock: &Clock, secs: f64) -> Vec<f64> {
        let from = clock.now_bars();
        let horizon = clock.bars_at(clock.now_secs() + secs);
        state
            .patterns
            .lock()
            .unwrap()
            .query(Span::new(from, horizon))
            .iter()
            .map(|e| e.event.value)
            .collect()
    }

    /// The reported bug: with the app open a while, an eval joined the pattern
    /// wherever the clock happened to be.
    #[test]
    fn without_a_reset_a_pattern_starts_mid_cycle() {
        let clock = Clock::with_cps(44100.0, 0.5);
        clock.advance(44100 * 5); // 2.5 bars in
        let state = state_with(vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0)]);

        assert_eq!(steps_over(&state, &clock, 2.0), vec![3.0, 4.0, 1.0, 2.0]);
    }

    /// With the reset, the same pattern starts at step one.
    #[test]
    fn a_reset_starts_the_pattern_at_its_first_step() {
        let clock = Clock::with_cps(44100.0, 0.5);
        clock.advance(44100 * 5);
        let state = state_with(vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0)]);

        clock.reset();
        assert_eq!(steps_over(&state, &clock, 2.0), vec![1.0, 2.0, 3.0, 4.0]);
    }

    /// The reported bug: the scheduler reads the clock a tick after the eval
    /// that reset it, so with bar 0 pinned to the eval's "now" the first step
    /// was already behind the window and the pattern was heard from its second.
    #[test]
    fn a_pattern_starts_at_step_one_even_a_tick_after_the_eval() {
        let clock = Clock::with_cps(44100.0, 0.5);
        clock.advance(44100 * 5);
        // Three steps, as in the report: dropping the first is audible as the
        // pattern starting on the second note.
        let state = state_with(vec![Some(1.0), Some(2.0), Some(3.0)]);

        clock.reset();
        // The audio thread renders on while the scheduler sleeps out its tick.
        clock.advance((44100.0 * TICK.as_secs_f64()) as u64);

        let first = steps_over(&state, &clock, LOOKAHEAD_SECS)
            .first()
            .copied()
            .expect("the first pass after an eval must schedule something");
        assert_eq!(first, 1.0, "the pattern must start on its first step");
    }

    /// Silence must not claim the lookahead window: the eval that fills it is a
    /// hair behind the pass that saw nothing, and its first steps land there.
    #[test]
    fn an_empty_pass_does_not_swallow_the_next_evals_first_step() {
        let clock = Clock::with_cps(44100.0, 0.5);
        let empty = SchedulerState::new();
        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);

        // A pass with nothing playing, in the sliver between reset and publish.
        clock.reset();
        let mark = schedule_pass(&mut seq, &clock, &empty, None, &mut Vec::new());
        assert!(
            mark.unwrap().bars <= 0.0,
            "an empty pass must not claim past bar 0, got {mark:?}",
        );

        // Now the bindings arrive and the next pass runs against that mark.
        let state = state_with(vec![Some(1.0), Some(2.0), Some(3.0)]);
        let mut live = Vec::new();
        schedule_pass(&mut seq, &clock, &state, mark, &mut live);
        assert_eq!(live.len(), 1, "the first step should have been pushed");
    }

    /// A one-shot published from silence, as `run_code` publishes one: the
    /// reset leaves the origin a lead-in *short* of bar 0, and that is the
    /// number the patterns are handed. The binding has to sound its whole first
    /// bar from there and nothing after it.
    #[test]
    fn a_one_shot_sounds_its_first_cycle_and_then_stops() {
        let clock = Clock::with_cps(44100.0, 0.5);
        clock.advance(44100 * 5);
        clock.reset();

        let s = SchedulerState::new();
        let ast = parse("fn k(n) = sin(n)\n".to_string()).unwrap();
        *s.instruments.lock().unwrap() = Instruments::from_program(&ast);
        *s.patterns.lock().unwrap() = Patterns {
            bindings: vec![Binding {
                target: "k".into(),
                source: Pattern::steps([Some(1.0), Some(2.0)]).into(),
                lanes: Vec::new(),
                start: 0.0,
                bars: Some(1.0), repeat: None, choice: None, button: None, rate: Rate::Fixed(1.0) }],
            origin: clock.now_bars(), epoch: 0, choices: Vec::new() };

        // One bar is two seconds at this tempo.
        assert_eq!(steps_over(&s, &clock, 2.0), vec![1.0, 2.0]);
        clock.advance(44100 * 4);
        assert!(steps_over(&s, &clock, 8.0).is_empty(), "the one-shot should be over");
    }

    /// Stop, wait, play: the silence must not advance the pattern.
    #[test]
    fn stopping_and_restarting_begins_again() {
        let clock = Clock::with_cps(44100.0, 0.5);
        let state = state_with(vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0)]);

        clock.reset();
        let first = steps_over(&state, &clock, 2.0);

        // Stop resets, three seconds of silence pass, then play again.
        clock.reset();
        clock.advance(44100 * 3);
        clock.reset();

        assert_eq!(steps_over(&state, &clock, 2.0), first);
        assert_eq!(first[0], 1.0);
    }

    /// A stale watermark from before a reset must not stall the loop. Without
    /// the guard `horizon <= from` holds forever and nothing is ever scheduled.
    #[test]
    fn a_watermark_from_before_a_reset_is_discarded() {
        let clock = Clock::with_cps(44100.0, 0.5);
        clock.advance(44100 * 60); // a minute in: bar 30
        let state = state_with(vec![Some(1.0)]);

        let mut seq = Sequencer::new(0, 2, ReplayMode::None);
        seq.set_sample_rate(44100.0);
        let mut live = Vec::new();

        // The scheduler has been running, so it holds a large watermark.
        let stale = schedule_pass(&mut seq, &clock, &state, None, &mut live);
        assert!(stale.unwrap().bars > 29.0, "watermark should be far along");

        clock.reset();
        let after = schedule_pass(&mut seq, &clock, &state, stale, &mut live)
            .expect("the pass must still claim a watermark");
        assert!(
            after.bars < 1.0,
            "the watermark must restart near zero, got {after:?}"
        );

        // Voices are still being pushed, and near the present rather than at
        // the stale watermark. (Rendered audio is no use here: the test's
        // sequencer starts at time 0 while the clock is a minute in — in the
        // app the two are aligned because they start together.)
        assert!(!live.is_empty(), "the scheduler must still be pushing voices");
        let now = clock.now_secs();
        for voice in &live {
            assert!(
                voice.end_secs > now && voice.end_secs < now + 5.0,
                "voice ends at {}, which is not near now ({now})",
                voice.end_secs
            );
        }
    }

    /// Re-evaluating while playing keeps the beat — the whole point of a
    /// free-running clock. Only an explicit reset moves the origin.
    #[test]
    fn a_re_eval_without_a_reset_keeps_the_beat() {
        let clock = Clock::with_cps(44100.0, 0.5);
        clock.reset();
        clock.advance(44100 * 3); // 1.5 bars into the performance
        let state = state_with(vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0)]);

        // No reset: an edit mid-performance picks up where the groove is.
        assert_eq!(steps_over(&state, &clock, 1.0), vec![3.0, 4.0]);
    }
}
