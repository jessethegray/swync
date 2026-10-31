//! Sending a pattern out as MIDI.
//!
//! This is the scheduler's third route to making something happen, and the
//! only one that leaves the machine. The graph is continuous and lives in the
//! engine's slot; a pattern bound to an instrument becomes a `fundsp` voice
//! pushed into the `Sequencer`; a pattern bound to a [`Destination`] becomes
//! two bytes on a wire at a moment in time. What makes the third one its own
//! thread is that the first two are *rendered* — they are handed to the audio
//! callback with a start time and it places them to the sample — while a MIDI
//! message is only ever sent *now*. Nothing downstream will place it for us,
//! so something here has to wait until the moment and then send.
//!
//! ## Why a thread of its own, at a millisecond
//!
//! The scheduler thread wakes every 25 ms, which is right for its job: it
//! works a fifth of a second ahead and hands the sequencer absolute times, so
//! when it wakes has nothing to do with when anything sounds. Sending MIDI on
//! that tick would make when it wakes the only thing that matters, and 25 ms
//! of jitter on a snare is not tight playing — it is audibly loose, and worse
//! than a hardware sequencer from 1983.
//!
//! So the notes are still *decided* on the scheduler's pass, a fifth of a
//! second early, and handed here as messages with the audio time they are due
//! at. This thread does nothing but wait for those times. At a 1 ms tick the
//! error is a millisecond and it is jitter rather than drift, which is the
//! part that matters: a snare a consistent millisecond late is a snare on the
//! beat.
//!
//! ## Audio time is not wall time, and this thread needs wall time
//!
//! Everything in the engine is stamped in *audio time* — frames rendered,
//! divided by the rate ([`Clock::now_secs`]). That number is the right one to
//! schedule against, because it is what the sequencer places voices on, and
//! MIDI that agrees with the audio is the whole point. But it does not
//! advance smoothly: the audio callback adds a whole buffer at a time, so
//! reading it in a loop gives a staircase whose step is the buffer — 10 ms on
//! a 512-frame device. Waiting on it directly would quantize every message to
//! that staircase and throw away most of what the 1 ms tick bought.
//!
//! So the thread keeps an **anchor**: one pairing of an audio time with an
//! `Instant`, from which the audio time *now* is predicted by ordinary
//! elapsed wall time. Between callbacks the prediction moves smoothly, which
//! the staircase does not, and the two are reconciled slowly rather than at
//! once — see [`CORRECTION`]. A sound card's clock really does run at a
//! slightly different rate from the system's, so an anchor left alone drifts;
//! one snapped to every reading inherits the staircase it was built to avoid.
//!
//! Two things reset it outright rather than correcting: a disagreement larger
//! than [`RESYNC_SECS`], which is a device switch or a stall rather than
//! drift, and having no anchor at all.
//!
//! ## The offset is not a fudge factor
//!
//! Audio time counts frames *rendered*, and a rendered frame has not been
//! heard yet — it is still in the device's buffers, and then in a converter,
//! and the gear at the other end of the MIDI cable has its own delay before
//! it makes a sound. None of that is knowable from here, and all of it is
//! specific to the desk. [`Out::set_offset`] is where a person puts the answer
//! once they have heard it, and it is in the settings file rather than the
//! project's for the reason the audio devices are: it describes the room, not
//! the piece.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::Arc;
use std::time::{Duration, Instant};

use midir::{MidiOutput, MidiOutputConnection};

use crate::midi::ports::{self, Match, PortInfo, Selector, CLIENT_NAME};
use crate::pattern::patterns::{LaneArg, CHANNEL, VELOCITY};
use crate::scheduler::clock::Clock;

/// How often the thread wakes. The floor on how late a message can be, and so
/// the whole of this thread's timing accuracy — see the module docs for why it
/// is not the 25 ms the scheduler runs at.
const TICK: Duration = Duration::from_millis(1);

/// How much of the disagreement between the anchor's prediction and the audio
/// clock is taken back each tick.
///
/// It is a pull *towards* the audio clock, and that has one consequence worth
/// knowing before it is discovered by surprise: against a clock that is not
/// advancing at all, the correction cancels wall time out and the prediction
/// settles a fixed distance ahead of zero rather than climbing — so messages
/// due beyond that point never come due. In a running app that cannot happen,
/// because the audio callback is what advances the clock and a stopped
/// callback is a stopped app. In a *test* it happens immediately, which is why
/// `a_note_reaches_a_real_port` runs a thread that plays the callback's part.
///
/// Small on purpose. The reading being corrected towards is quantized to the
/// audio buffer, so most of any single disagreement is which side of a
/// callback the reading landed on rather than real drift; taking all of it
/// would hand that staircase straight back to every message. At a hundredth
/// per millisecond the staircase averages out over a few buffers while a real
/// rate difference — parts per million — is followed with room to spare.
const CORRECTION: f64 = 0.01;

/// A disagreement past which the anchor is rebuilt rather than nudged.
///
/// Beyond this the two clocks have not drifted apart, something has happened
/// to one of them: the output device was switched, or this thread was starved
/// long enough to matter. Correcting a gap that size at [`CORRECTION`] would
/// take minutes, and every message sent during them would be wrong.
const RESYNC_SECS: f64 = 0.25;

/// The largest offset that can be asked for, in milliseconds, either way.
///
/// A quarter of a second in each direction covers every converter and every
/// piece of gear anyone is lining up by ear. It is bounded at all because the
/// offset is what decides how far ahead a message may be held, and an
/// unbounded one is a note that never arrives.
pub const MAX_OFFSET_MS: i64 = 250;

/// Where a pattern's notes are being sent: which port, and the channel to use
/// for a note whose `chan` lane says nothing.
///
/// The port is held as a [`Selector`] rather than as a resolved port because
/// the two questions are asked at different times by different threads. The
/// lowerer resolves it once, to *say something* — a name matching nothing is a
/// warning in the problems panel — and then throws the answer away. This
/// thread resolves it again, to *open something*, on every publish. Keeping a
/// resolved port between them would be a promise that the rack has not changed
/// since the program was compiled, and the whole reason a missing port is a
/// warning rather than an error is that it does change.
#[derive(Debug, Clone, PartialEq)]
pub struct Destination {
    pub selector: Selector,
    /// 1-16, as it is written and as it is spoken. Turned into the 0-15 the
    /// wire carries at the last possible moment, in [`status`].
    pub channel: u8,
}

/// The lowest and highest channel that can be written.
pub const MIN_CHANNEL: u8 = 1;
pub const MAX_CHANNEL: u8 = 16;

/// One note to play, in audio time, on its way from the scheduler.
///
/// Both ends of the note travel together rather than the off being scheduled
/// when the on is sent. A note whose off was still to be decided is a note
/// that hangs if anything goes wrong in between, and "anything" includes the
/// scheduler being asked to stop between the two.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub destination: Destination,
    /// The channel this note actually goes out on: its binding's, unless a
    /// `chan` lane overrode it.
    pub channel: u8,
    pub note: u8,
    pub velocity: u8,
    /// Audio time of the note-on.
    pub on_secs: f64,
    /// Audio time of the note-off. Never before `on_secs`.
    pub off_secs: f64,
}

/// The velocity a note is sent at when its binding says nothing.
///
/// The conventional default on the wire, which is what a synth's own
/// "velocity: 100" means and what a sequencer sends for an unaccented note.
/// Expressed here rather than as a fraction because it is a fact about MIDI,
/// not about the language — what the language writes is 0 to 1, and this is
/// what nothing at all comes to.
pub const DEFAULT_VELOCITY: u8 = 100;

/// The highest note and velocity the wire can carry. Seven bits each.
pub const MAX_DATA: f64 = 127.0;

/// How far before the end of its step a note is released.
///
/// A step's *whole* length is what a voice gets, and that is right there: an
/// instrument's envelope releases from the note's end, the next note is a
/// separate voice, and the two are free to overlap while one rings out. On a
/// wire it is wrong, and the way it is wrong is this bug — a note-off landing
/// in the same instant as the next note-on is a gate that never closes. Gear
/// reads the two in the order they arrive and a great deal of it will not
/// re-attack an envelope with no gap in front of it: a mono synth slides to
/// the new pitch without retriggering, and a repeated pitch does not move at
/// all. Either way an even line of notes comes out as one long note, which is
/// exactly what gets reported. Every hardware sequencer leaves a gap here and
/// this is the same gap.
///
/// Five milliseconds, and the units matter: it is a fact about gear rather
/// than about the music, so it is a *time* and not a share of the step. A
/// tenth of a whole note is a fifth of a second of silence on a pad, which
/// anybody would hear as staccato and nobody asked for, while five
/// milliseconds is longer than the three bytes of a note-off take to go down
/// the wire and shorter than anything a player can hear.
///
/// It is taken off whatever `legato` already decided rather than instead of
/// it, so a `legato` written to overlap the next note still overlaps it — five
/// milliseconds less. That is what a piece should write when it wants a mono
/// synth to glide, and it is honest about what it is asking for in a way that
/// a zero-length gap never was.
const RELEASE_SECS: f64 = 0.005;

/// The most of a note the gap may take, for the notes short enough that it
/// would otherwise take all of it.
///
/// At a sixteenth of a bar at 170 bpm a step is about 90 milliseconds and the
/// gap is a small part of it; at the speeds a `fast` can reach the step is
/// shorter than the gap, and a note shortened to nothing is a note that does
/// not sound. Half is arbitrary beyond being clearly on the right side of
/// both.
const MAX_RELEASE_SHARE: f64 = 0.5;

impl Note {
    /// Build the note a queried pattern event comes to.
    ///
    /// The pattern's own value is the MIDI note number, because that is
    /// already what pitch is counted in everywhere in this language — `c4` is
    /// 60, `semi` transposes by one, `scale` snaps to a grid of them. Nothing
    /// is converted here; a program that plays 60 to an instrument and 60 to a
    /// synth means the same note both times.
    ///
    /// A value outside the seven bits the wire carries is **clamped rather
    /// than dropped**. Both are wrong, and the difference is whether anybody
    /// finds out: an octave error that piles a line onto note 127 is ugly and
    /// audible and points straight at itself, while the same error silently
    /// playing nothing is a part that has simply stopped working, with the
    /// program that caused it looking exactly like one that should sound.
    ///
    /// The note is released a little before the end it is handed — see
    /// [`RELEASE_SECS`], which is the whole of what makes a line of notes a
    /// line rather than one held note.
    pub fn from_event(
        destination: &Destination,
        value: f64,
        lanes: &[(String, LaneArg)],
        on_secs: f64,
        off_secs: f64,
    ) -> Note {
        let lane = |name: &str| lanes.iter().find(|(n, _)| n == name).and_then(|(_, v)| match v {
            LaneArg::Num(n) => Some(*n),
            // A quoted list has already been refused at bind time for these
            // lanes — each of them is one number — so this is unreachable
            // rather than a case with an opinion.
            LaneArg::List(_) => None,
        });

        Note {
            destination: destination.clone(),
            channel: lane(CHANNEL)
                .map(|c| c.round().clamp(MIN_CHANNEL as f64, MAX_CHANNEL as f64) as u8)
                .unwrap_or(destination.channel),
            note: value.round().clamp(0.0, MAX_DATA) as u8,
            // 0 to 1 as everything else in the language is, and never actually
            // zero: velocity zero is a note-off on the wire, so `vel: 0` would
            // be a note that released itself. A note written silent is still a
            // note somebody asked for.
            velocity: lane(VELOCITY)
                .map(|v| ((v * MAX_DATA).round().clamp(1.0, MAX_DATA)) as u8)
                .unwrap_or(DEFAULT_VELOCITY),
            on_secs,
            off_secs: released(on_secs, off_secs),
        }
    }
}

/// When a note handed `on_secs`..`off_secs` is actually released.
///
/// A free function so the rule — which is the part with judgement in it — is
/// testable on its own, and so that the clamp for a step shorter than the gap
/// sits beside the gap rather than inside a struct literal.
fn released(on_secs: f64, off_secs: f64) -> f64 {
    let held = off_secs - on_secs;
    // A note whose end precedes its start is already as short as a note can
    // be, and `queue` has its own answer for it. Shortening it further would
    // only move the off further into the past.
    if !held.is_finite() || held <= 0.0 {
        return off_secs;
    }
    off_secs - RELEASE_SECS.min(held * MAX_RELEASE_SHARE)
}

/// The MIDI clock's resolution: twenty-four ticks to the quarter note.
///
/// Fixed by the specification rather than chosen here, and it is why clock is
/// this thread's job rather than the scheduler's. At 120 bpm a tick is every
/// 21 ms and at 180 it is every 14 — either way, a thread waking every 25 ms
/// could not place them, while one waking every millisecond can.
pub const PPQN: f64 = 24.0;

/// The three transport bytes. `Continue` is not sent: swync's own stop clears
/// the pattern bindings, so what follows is always a beginning rather than a
/// resumption, and saying `Continue` would tell a drum machine to pick up
/// where it left off while everything here started again from the top.
const CLOCK_TICK: u8 = 0xF8;
const CLOCK_START: u8 = 0xFA;
const CLOCK_STOP: u8 = 0xFC;

/// How many ticks may be sent at once when this thread has been late.
///
/// A stall — a device switch, a machine that went to sleep — leaves bar time
/// far ahead of the last tick sent, and catching up literally would spray
/// hundreds of ticks at a drum machine, which reads them as a burst of tempo.
/// Past this the count is simply resynchronised: the gear has already lost the
/// beat, and the quickest way back is to start counting again from where the
/// music actually is.
const MAX_CATCHUP_TICKS: i64 = 48;

/// What the scheduler and the command thread can ask of this thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Play these. Queued, not replacing what is already queued: the scheduler
    /// sends the notes it decided on this pass, and the ones from the pass
    /// before are already in flight.
    Play(Vec<Note>),
    /// Everything stops now. Pending note-ons are dropped and everything
    /// sounding is released — see [`Player::stop`].
    Stop,
    /// The ports a program wants clock on, replacing whatever the last program
    /// asked for. Empty is the ordinary case and costs nothing.
    ClockTo(Vec<Selector>),
    /// The transport moved. `true` starts the clock, `false` stops it.
    ///
    /// Separate from [`Stop`](Command::Stop) even though a stop does both,
    /// because the two are not the same event: a program can be re-evaluated,
    /// which starts the transport again without anything having stopped.
    Transport(bool),
}

/// A note this thread has sent an on for and not yet an off.
///
/// Keyed by where it is sounding rather than by which [`Note`] asked for it,
/// because that is what a note-off addresses: the wire carries a channel and a
/// number and nothing that says which pattern sent it. Two bindings playing
/// middle C on the same channel of the same port are one sounding note as far
/// as the synth is concerned, and this key is what makes that true here too.
type Sounding = (String, u8, u8);

/// The handle the rest of the app holds.
#[derive(Clone)]
pub struct Out {
    tx: Sender<Command>,
    /// Milliseconds, as a signed count. An atomic rather than another
    /// `Command` because it is dragged on a control while notes are playing,
    /// and a queue would apply each intermediate value one tick apart.
    offset_ms: Arc<AtomicI64>,
}

impl Out {
    /// A handle with nothing on the other end of it.
    ///
    /// Every send fails and is dropped, which is already what happens when the
    /// MIDI thread has gone — so nothing here needs a second path for it. This
    /// is what a `SchedulerState` has until the app hands it a real one, and
    /// what the great many scheduler tests that are about rhythm rather than
    /// about MIDI go on using.
    pub fn detached() -> Out {
        let (tx, _) = channel();
        Out { tx, offset_ms: Arc::new(AtomicI64::new(0)) }
    }

    /// A handle whose messages can be read back, for tests about what the
    /// scheduler decided to send rather than about what reached a wire.
    #[cfg(test)]
    pub fn collecting() -> (Out, Receiver<Command>) {
        let (tx, rx) = channel();
        (Out { tx, offset_ms: Arc::new(AtomicI64::new(0)) }, rx)
    }

    /// Hand these notes to the thread that will send them.
    ///
    /// Never blocks and never fails in a way worth reporting: a send that
    /// cannot land means the MIDI thread is gone, and a thread that is gone
    /// has already said so through the reporter that killed it.
    pub fn play(&self, notes: Vec<Note>) {
        if notes.is_empty() {
            return;
        }
        let _ = self.tx.send(Command::Play(notes));
    }

    /// Drop what has not been sent and release what is sounding.
    pub fn stop(&self) {
        let _ = self.tx.send(Command::Stop);
    }

    /// Send clock to these ports, and to no others.
    ///
    /// Called on every eval with whatever the program named, so a `midiclock`
    /// that has been deleted stops being sent to. Sending the empty list is
    /// what almost every program does and is the cheap path.
    pub fn clock_to(&self, ports: Vec<Selector>) {
        let _ = self.tx.send(Command::ClockTo(ports));
    }

    /// The transport started, or stopped.
    pub fn transport(&self, running: bool) {
        let _ = self.tx.send(Command::Transport(running));
    }

    /// How far behind the audio a message should be sent, in milliseconds.
    /// Clamped rather than refused: this arrives from a control, and a control
    /// dragged to its end means its end.
    pub fn set_offset_ms(&self, ms: i64) {
        self.offset_ms.store(ms.clamp(-MAX_OFFSET_MS, MAX_OFFSET_MS), Ordering::Relaxed);
    }

    fn offset_secs(&self) -> f64 {
        self.offset_ms.load(Ordering::Relaxed) as f64 / 1000.0
    }
}

/// Spawn the MIDI output thread. Call once, at startup.
///
/// Started whether or not anything will ever use it, exactly as the scheduler
/// is: what it costs while no program names a port is a thread waking a
/// thousand times a second to find an empty queue, and what starting it lazily
/// would cost is the first note of the first `midiout` in a set arriving
/// whenever the thread got round to existing.
pub fn start(clock: Clock) -> Out {
    let (tx, rx) = channel();
    let offset_ms = Arc::new(AtomicI64::new(0));
    let out = Out { tx, offset_ms: Arc::clone(&offset_ms) };
    let handle = out.clone();
    std::thread::spawn(move || run(rx, clock, handle));
    out
}

/// One pairing of audio time with wall time, from which audio time is
/// predicted between the audio callback's steps. See the module docs.
#[derive(Clone, Copy)]
struct Anchor {
    audio_secs: f64,
    at: Instant,
}

impl Anchor {
    /// What the audio clock should read now, if it advanced smoothly.
    fn predict(&self, now: Instant) -> f64 {
        self.audio_secs + now.duration_since(self.at).as_secs_f64()
    }
}

/// Everything the thread owns. A struct rather than a pile of locals so that
/// the parts with judgement in them — [`Player::due`], [`Player::stop`] — can
/// be tested without a thread, a clock or a MIDI port.
struct Player {
    /// Open connections, by port name. Held open once opened: opening a port
    /// is slow enough to be audible if it happened on the first note of every
    /// pattern, and a port swync is holding is one another application can see
    /// it is using.
    connections: HashMap<String, MidiOutputConnection>,
    /// Ports that could not be opened, so that a failure is reported once
    /// rather than a thousand times a second for the rest of the evening.
    refused: HashSet<String>,
    /// Messages waiting for their moment, each with the audio time it is due.
    /// Unsorted: it is walked in full every tick anyway, and the number of
    /// notes inside a fifth of a second is small enough that keeping it in
    /// order would cost more than it saved.
    pending: Vec<(f64, Sounding, u8)>,
    /// What is sounding right now, so that a stop can end exactly those notes
    /// and nothing else.
    sounding: HashSet<Sounding>,
    anchor: Option<Anchor>,
    /// The ports a program asked for clock on, as written. Resolved per batch
    /// like a note's, so a box plugged in mid-set starts receiving.
    clock_to: Vec<Selector>,
    /// The same, resolved to port names on the last batch. Kept apart from
    /// `clock_to` because resolving talks to the platform and a tick must not.
    resolved: Vec<String>,
    /// Whether the transport is running, which is the whole of whether ticks
    /// are being sent.
    running: bool,
    /// The last tick index sent, counted in ticks since bar zero. `None`
    /// before the first one of a run — see [`Player::clock`].
    last_tick: Option<i64>,
}

/// A note-on's velocity is its third byte; a note-off's is zero, and the
/// pending queue tells them apart by that alone. It can, because MIDI itself
/// does: a note-on of velocity zero *is* a note-off, and has been since the
/// running-status days.
const NOTE_OFF: u8 = 0;

/// The status byte for a note on the given channel, which is where 1-16
/// becomes the 0-15 the wire carries.
fn status(channel: u8, velocity: u8) -> u8 {
    let channel = channel.clamp(MIN_CHANNEL, MAX_CHANNEL) - 1;
    if velocity == NOTE_OFF { 0x80 | channel } else { 0x90 | channel }
}

impl Player {
    fn new() -> Player {
        Player {
            connections: HashMap::new(),
            refused: HashSet::new(),
            pending: Vec::new(),
            sounding: HashSet::new(),
            anchor: None,
            clock_to: Vec::new(),
            resolved: Vec::new(),
            running: false,
            last_tick: None,
        }
    }

    /// Send whatever clock ticks are due at this bar position.
    ///
    /// Counted against **bar time** rather than against an interval, which is
    /// what keeps it in step with the music rather than merely regular. A
    /// tempo change moves the bar position, so the next tick falls where the
    /// new tempo puts it and nothing here has to be told the tempo changed at
    /// all — which also means an `accel` under a pattern is followed for free.
    fn clock(&mut self, bars: f64, ticks_per_bar: f64) {
        if !self.running || self.clock_to.is_empty() || !bars.is_finite() {
            return;
        }
        let now = (bars * ticks_per_bar).floor() as i64;
        let from = match self.last_tick {
            // The first tick of a run is *not* sent: `Start` has just been
            // sent, and a receiver counts its first tick as one already
            // elapsed. Beginning at the tick after the current position is
            // what puts the downbeat where both ends agree it is.
            None => now,
            Some(last) if now - last > MAX_CATCHUP_TICKS => now,
            Some(last) => last,
        };
        for _ in from..now {
            self.broadcast(CLOCK_TICK);
        }
        self.last_tick = Some(now);
    }

    /// Send one byte to every port a program asked for clock on.
    fn broadcast(&mut self, byte: u8) {
        // Cloned so the ports can be walked while `send_raw` borrows self
        // mutably to open a connection. The list is a handful of strings and
        // this happens at most a few hundred times a second.
        let ports = std::mem::take(&mut self.resolved);
        for port in &ports {
            if let Some(connection) = self.connect(port) {
                // A failed send is dropped for the same reason a note's is:
                // the port was open a moment ago, so this is gear being
                // unplugged, and the next batch says so properly.
                let _ = connection.send(&[byte]);
            }
        }
        self.resolved = ports;
    }

    /// Move the anchor towards what the audio clock actually says.
    fn sync(&mut self, clock: &Clock, now: Instant) {
        let audio_secs = clock.now_secs();
        let Some(anchor) = self.anchor else {
            self.anchor = Some(Anchor { audio_secs, at: now });
            return;
        };
        let error = audio_secs - anchor.predict(now);
        self.anchor = Some(if error.abs() > RESYNC_SECS {
            Anchor { audio_secs, at: now }
        } else {
            Anchor { audio_secs: anchor.predict(now) + error * CORRECTION, at: now }
        });
    }

    /// Everything whose moment has come, in the order it was queued.
    ///
    /// Takes the messages out of `pending` rather than marking them, so that a
    /// message can only ever be sent once — which is the property that matters
    /// most here, since a note-off sent twice is harmless and a note-on sent
    /// twice is a stuck note.
    fn due(&mut self, audio_now: f64) -> Vec<(Sounding, u8)> {
        let mut due = Vec::new();
        self.pending.retain(|(at, sounding, velocity)| {
            if *at <= audio_now {
                due.push((sounding.clone(), *velocity));
                false
            } else {
                true
            }
        });
        due
    }

    /// Queue both ends of a note.
    ///
    /// A note-on for something already sounding sends its note-off first. The
    /// alternative is worse than it sounds: the wire has no way to say *which*
    /// middle C to stop, so the first note's off would silence the second, and
    /// the second's off would be sent to a channel already silent — one note
    /// where two were written, and the last one hanging. Retriggering is what
    /// every hardware sequencer does here and what a player expects to hear.
    fn queue(&mut self, note: &Note, port: &str) {
        let key: Sounding = (port.to_string(), note.channel, note.note);
        if self.pending.iter().any(|(_, k, v)| *k == key && *v != NOTE_OFF) {
            self.pending.push((note.on_secs, key.clone(), NOTE_OFF));
        }
        self.pending.push((note.on_secs, key.clone(), note.velocity.max(1)));
        self.pending.push((note.off_secs.max(note.on_secs), key, NOTE_OFF));
    }

    /// Drop everything not yet sent and release everything sounding.
    ///
    /// The queue goes first and unconditionally, including its note-offs:
    /// those notes are being ended here instead, at once, and a note-off left
    /// in the queue would be sent to a synth that has already stopped — where,
    /// if a player has since pressed that key by hand, it would cut their
    /// note. What is released is exactly what this thread turned on, which is
    /// why `sounding` is tracked at all rather than reaching for the
    /// all-notes-off that would also silence everything else on the port.
    fn stop(&mut self) -> Vec<(Sounding, u8)> {
        self.pending.clear();
        self.sounding.drain().map(|key| (key, NOTE_OFF)).collect()
    }

    /// Send one message, opening the port if this is the first to need it.
    fn send(&mut self, (port, channel, note): Sounding, velocity: u8) {
        if velocity == NOTE_OFF {
            // Removed before the send rather than after, so a port that fails
            // mid-message is not left with a note this thread believes it can
            // still turn off.
            self.sounding.remove(&(port.clone(), channel, note));
        } else {
            self.sounding.insert((port.clone(), channel, note));
        }

        let Some(connection) = self.connect(&port) else {
            return;
        };
        // A failed send is dropped rather than reported. The port was open a
        // moment ago, so this is gear being unplugged mid-note, and what is
        // worth saying about that is said by the next note that cannot find
        // the port at all.
        let _ = connection.send(&[status(channel, velocity), note, velocity]);
    }

    /// The connection to a port, opening it if it is not already open.
    fn connect(&mut self, port: &str) -> Option<&mut MidiOutputConnection> {
        if !self.connections.contains_key(port) && !self.refused.contains(port) {
            match open(port) {
                Some(connection) => {
                    self.connections.insert(port.to_string(), connection);
                }
                None => {
                    // Remembered so that the next thousand ticks do not each
                    // try again. A port that has come back is picked up by the
                    // next publish, which clears this.
                    self.refused.insert(port.to_string());
                }
            }
        }
        self.connections.get_mut(port)
    }
}

/// Open one port by the name the platform gave it.
///
/// The name is looked up again rather than a `MidiOutputPort` being carried
/// here, because a `MidiOutput` client is consumed by connecting through it
/// and its ports belong to it — so the port to connect to has to come from the
/// same client that is about to be spent on it.
fn open(name: &str) -> Option<MidiOutputConnection> {
    let midi = MidiOutput::new(CLIENT_NAME).ok()?;
    let port = midi
        .ports()
        .into_iter()
        .find(|p| midi.port_name(p).is_ok_and(|found| found == name))?;
    midi.connect(&port, name).ok()
}

/// Resolve where a note is going, on the thread that is about to send it.
///
/// Answers `None` for a port that is not here, silently: the sentence about
/// that was said at compile time, by the lowerer, once — see [`Destination`].
/// Saying it again per note would fill the problems panel at the rate the
/// pattern is playing.
fn port_for(ports: &[PortInfo], destination: &Destination) -> Option<String> {
    match ports::find(ports, &destination.selector) {
        Match::One(port) | Match::Ambiguous(port, _) => Some(port.name),
        Match::Missing => None,
    }
}

fn run(rx: Receiver<Command>, clock: Clock, out: Out) {
    let mut player = Player::new();

    loop {
        std::thread::sleep(TICK);
        let now = Instant::now();
        player.sync(&clock, now);

        // Commands before the queue is walked, so a stop that arrived this
        // millisecond is not beaten by a note that was due in the same one.
        loop {
            match rx.try_recv() {
                Ok(Command::Play(notes)) => {
                    // The list is asked for once per batch rather than once
                    // per note: enumerating ports talks to the platform, and a
                    // pass carries every note of the next fifth of a second.
                    let ports = ports::outputs();
                    // A port that refused is worth trying again now, because a
                    // publish is the moment something may have been plugged
                    // in — and the alternative is that a cable knocked out
                    // during a set stays dead until the app is restarted.
                    player.refused.clear();
                    for note in &notes {
                        if let Some(port) = port_for(&ports, &note.destination) {
                            player.queue(note, &port);
                        }
                    }
                }
                Ok(Command::Stop) => {
                    for (key, velocity) in player.stop() {
                        player.send(key, velocity);
                    }
                }
                Ok(Command::ClockTo(ports)) => {
                    // Resolved here, on the command, rather than per tick:
                    // enumerating talks to the platform and a tick has 21
                    // milliseconds to spare at best.
                    let available = ports::outputs();
                    player.resolved = ports
                        .iter()
                        .filter_map(|selector| match ports::find(&available, selector) {
                            Match::One(port) | Match::Ambiguous(port, _) => Some(port.name),
                            Match::Missing => None,
                        })
                        .collect();
                    player.clock_to = ports;
                    player.refused.clear();
                }
                Ok(Command::Transport(running)) => {
                    if running != player.running {
                        player.running = running;
                        player.broadcast(if running { CLOCK_START } else { CLOCK_STOP });
                        // Counting begins again from wherever the music is,
                        // rather than from a tick index left over from the
                        // last run.
                        player.last_tick = None;
                    }
                }
                Err(TryRecvError::Empty) => break,
                // The app is going away and every sender has been dropped.
                // Releasing first is not politeness: a note left on is a
                // drone that outlives the process, on gear that has no idea
                // the thing playing it has quit.
                Err(TryRecvError::Disconnected) => {
                    for (key, velocity) in player.stop() {
                        player.send(key, velocity);
                    }
                    // Gear left following a clock that has stopped arriving
                    // sits waiting for the next tick for ever. Saying so on
                    // the way out costs one byte.
                    if player.running {
                        player.broadcast(CLOCK_STOP);
                    }
                    return;
                }
            }
        }

        let Some(anchor) = player.anchor else {
            continue;
        };
        // The offset moves the *deadline*, not the message: asking for the
        // audio time ten milliseconds ago is what "send ten milliseconds late"
        // means, and it needs no special case for either sign.
        let audio_now = anchor.predict(now) - out.offset_secs();
        for (key, velocity) in player.due(audio_now) {
            player.send(key, velocity);
        }

        // Clock rides the same predicted time as the notes, so a tick and the
        // note on that beat leave together rather than a buffer apart.
        let ticks_per_bar = PPQN * clock.meter().quarters_per_bar();
        player.clock(clock.bars_at(audio_now), ticks_per_bar);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to(port_channel: u8) -> Destination {
        Destination { selector: Selector::Name("deluge".into()), channel: port_channel }
    }

    fn note(note: u8, on: f64, off: f64) -> Note {
        Note {
            destination: to(1),
            channel: 1,
            note,
            velocity: 100,
            on_secs: on,
            off_secs: off,
        }
    }

    const PORT: &str = "Deluge MIDI 1";

    /// Everything a `Player` does that has judgement in it is reachable
    /// without a port, a clock or a thread, which is the point of it being a
    /// struct — these tests are about queueing and releasing, not about MIDI.
    fn player_holding(notes: &[Note]) -> Player {
        let mut player = Player::new();
        for note in notes {
            player.queue(note, PORT);
        }
        player
    }

    #[test]
    fn a_note_is_queued_as_an_on_and_an_off() {
        let mut player = player_holding(&[note(60, 1.0, 2.0)]);
        assert_eq!(player.due(0.5), Vec::new());
        assert_eq!(player.due(1.0), vec![((PORT.into(), 1, 60), 100)]);
        assert_eq!(player.due(2.0), vec![((PORT.into(), 1, 60), NOTE_OFF)]);
    }

    #[test]
    fn a_message_is_only_ever_handed_out_once() {
        let mut player = player_holding(&[note(60, 1.0, 2.0)]);
        assert_eq!(player.due(5.0).len(), 2);
        assert_eq!(player.due(5.0), Vec::new());
    }

    /// The failure this prevents is a stuck note: without the extra off, the
    /// first note's off silences the second and the second's off arrives at a
    /// channel already quiet.
    #[test]
    fn a_note_retriggered_while_it_is_still_sounding_is_released_first() {
        let mut player = player_holding(&[note(60, 1.0, 4.0), note(60, 2.0, 5.0)]);
        assert_eq!(player.due(1.0), vec![((PORT.into(), 1, 60), 100)]);
        assert_eq!(
            player.due(2.0),
            vec![((PORT.into(), 1, 60), NOTE_OFF), ((PORT.into(), 1, 60), 100)],
            "the second note-on must be preceded by an off for the first"
        );
    }

    #[test]
    fn the_same_note_on_two_channels_is_two_sounding_notes() {
        let mut a = note(60, 1.0, 2.0);
        let mut b = note(60, 1.0, 2.0);
        a.channel = 1;
        b.channel = 10;
        let mut player = player_holding(&[a, b]);
        let due = player.due(1.0);
        assert_eq!(due.len(), 2, "neither should have released the other");
        assert!(due.iter().all(|(_, velocity)| *velocity != NOTE_OFF));
    }

    #[test]
    fn a_stop_releases_exactly_what_is_sounding() {
        let mut player = player_holding(&[note(60, 1.0, 9.0), note(64, 1.0, 9.0)]);
        for (key, velocity) in player.due(1.0) {
            // `send` without a port open still records what is sounding, which
            // is what a stop reads — the wire is not what is being tested.
            player.send(key, velocity);
        }
        let mut released: Vec<u8> = player.stop().into_iter().map(|((_, _, n), _)| n).collect();
        released.sort();
        assert_eq!(released, vec![60, 64]);
    }

    /// A note whose on has not been sent yet is dropped rather than released:
    /// nothing is sounding, and a note-off for it would reach a synth that
    /// might have that key held down by hand.
    #[test]
    fn a_stop_drops_notes_that_had_not_started() {
        let mut player = player_holding(&[note(60, 5.0, 6.0)]);
        assert_eq!(player.stop(), Vec::new());
        assert_eq!(player.due(9.0), Vec::new(), "the queue should be empty");
    }

    #[test]
    fn a_stop_leaves_nothing_to_be_released_twice() {
        let mut player = player_holding(&[note(60, 1.0, 9.0)]);
        for (key, velocity) in player.due(1.0) {
            player.send(key, velocity);
        }
        assert_eq!(player.stop().len(), 1);
        assert_eq!(player.stop(), Vec::new());
    }

    /// A note-off sent normally must leave nothing for a later stop to release,
    /// or every stop would send offs for notes that ended bars ago.
    #[test]
    fn a_note_that_ended_on_its_own_is_not_released_again_by_a_stop() {
        let mut player = player_holding(&[note(60, 1.0, 2.0)]);
        for (key, velocity) in player.due(3.0) {
            player.send(key, velocity);
        }
        assert_eq!(player.stop(), Vec::new());
    }

    #[test]
    fn a_note_off_carries_the_note_off_status_for_its_channel() {
        assert_eq!(status(1, NOTE_OFF), 0x80);
        assert_eq!(status(16, NOTE_OFF), 0x8F);
    }

    #[test]
    fn a_note_on_carries_the_note_on_status_for_its_channel() {
        assert_eq!(status(1, 100), 0x90);
        assert_eq!(status(10, 100), 0x99);
    }

    /// Channels are written 1-16 and carried 0-15, and the clamp is what keeps
    /// a channel that got past the lowerer from writing into another status
    /// byte's range — 0 would make a note-on into a note-off.
    #[test]
    fn a_channel_outside_the_range_is_clamped_rather_than_wrapped() {
        assert_eq!(status(0, 100), status(1, 100));
        assert_eq!(status(99, 100), status(16, 100));
    }

    /// Velocity zero *is* a note-off on the wire, so a note written with no
    /// velocity at all would turn itself off the moment it started.
    #[test]
    fn a_note_is_never_queued_at_a_velocity_that_would_read_as_a_release() {
        let mut quiet = note(60, 1.0, 2.0);
        quiet.velocity = 0;
        let mut player = player_holding(&[quiet]);
        assert_eq!(player.due(1.0), vec![((PORT.into(), 1, 60), 1)]);
    }

    #[test]
    fn a_note_whose_end_precedes_its_start_ends_when_it_starts() {
        let mut player = player_holding(&[note(60, 2.0, 1.0)]);
        let due = player.due(2.0);
        assert_eq!(due.len(), 2, "both ends fall in the same moment");
        assert_eq!(due[1].1, NOTE_OFF, "and the off is still last");
    }

    /// A player already sending clock somewhere, with a note of every byte
    /// that went out — `send` needs no port open to record what was sounding,
    /// and `broadcast` needs one, so clock is counted rather than captured.
    fn clocking() -> Player {
        let mut player = Player::new();
        player.clock_to = vec![Selector::Name("deluge".into())];
        player.resolved = vec![PORT.to_string()];
        player.running = true;
        player
    }

    /// Ticks are counted against **bar time**, not against an interval. That
    /// is what keeps them in step with the music rather than merely regular —
    /// and it is why a tempo change needs no code here at all.
    #[test]
    fn a_quarter_note_of_bar_time_is_twenty_four_ticks() {
        let mut player = clocking();
        let per_bar = PPQN * 4.0;
        player.clock(0.0, per_bar);
        assert_eq!(player.last_tick, Some(0));
        // A quarter of a 4/4 bar.
        player.clock(0.25, per_bar);
        assert_eq!(player.last_tick, Some(24));
    }

    /// The first tick of a run is not sent: `Start` has just gone, and a
    /// receiver counts its first tick as one already elapsed.
    #[test]
    fn counting_begins_where_the_music_is_rather_than_at_zero() {
        let mut player = clocking();
        player.clock(8.0, PPQN * 4.0);
        assert_eq!(player.last_tick, Some(8 * 96), "eight bars in, not tick zero");
    }

    /// A stall leaves bar time far ahead of the last tick sent. Catching up
    /// literally would spray hundreds of ticks at a drum machine, which reads
    /// them as a burst of tempo.
    #[test]
    fn a_long_stall_resynchronises_rather_than_catching_up() {
        let mut player = clocking();
        let per_bar = PPQN * 4.0;
        player.clock(0.0, per_bar);
        player.clock(10.0, per_bar);
        assert_eq!(player.last_tick, Some(960), "it should be where the music is");
    }

    #[test]
    fn nothing_is_sent_while_the_transport_is_stopped() {
        let mut player = clocking();
        player.running = false;
        player.clock(1.0, PPQN * 4.0);
        assert_eq!(player.last_tick, None, "a stopped transport counts nothing");
    }

    #[test]
    fn nothing_is_sent_when_no_program_asked_for_clock() {
        let mut player = Player::new();
        player.running = true;
        player.clock(1.0, PPQN * 4.0);
        assert_eq!(player.last_tick, None);
    }

    /// A signature with a different bar length changes how many ticks fill a
    /// bar, and nothing else — the quarter note is still twenty-four.
    #[test]
    fn a_bar_holds_as_many_ticks_as_it_holds_quarter_notes() {
        let mut player = clocking();
        player.clock(0.0, PPQN * 3.0);
        player.clock(1.0, PPQN * 3.0);
        assert_eq!(player.last_tick, Some(72), "three quarters to a 3/4 bar");
    }

    /// The bug this whole gap exists for: a note-off in the same instant as
    /// the next note-on is a gate that never closes, and a line of even notes
    /// reaches the gear as one held note.
    #[test]
    fn a_sent_note_is_released_before_the_next_one_begins() {
        let a = Note::from_event(&to(1), 60.0, &[], 0.0, 0.5);
        let b = Note::from_event(&to(1), 64.0, &[], 0.5, 1.0);
        assert!(a.off_secs < b.on_secs, "{} should precede {}", a.off_secs, b.on_secs);
    }

    /// The gap is a fact about gear, so it is a time rather than a share of
    /// the step — a tenth of a whole note would be a fifth of a second of
    /// silence on a pad, which is staccato nobody wrote.
    #[test]
    fn the_gap_is_the_same_length_whatever_the_note_is() {
        let short = Note::from_event(&to(1), 60.0, &[], 0.0, 0.25);
        let long = Note::from_event(&to(1), 60.0, &[], 0.0, 4.0);
        assert!((short.off_secs - (0.25 - RELEASE_SECS)).abs() < 1e-9, "got {short:?}");
        assert!((long.off_secs - (4.0 - RELEASE_SECS)).abs() < 1e-9, "got {long:?}");
    }

    /// A step shorter than the gap would otherwise be shortened to nothing,
    /// and a note of no length is a note that does not sound.
    #[test]
    fn a_step_shorter_than_the_gap_keeps_half_of_itself() {
        let note = Note::from_event(&to(1), 60.0, &[], 1.0, 1.002);
        assert!(note.off_secs > note.on_secs, "got {note:?}");
        assert!((note.off_secs - 1.001).abs() < 1e-9, "got {note:?}");
    }

    /// `legato` writes how long the note is held and the gap comes off what it
    /// decided, so a note written to overlap the next one still overlaps it —
    /// which is what a piece asks for when it wants a mono synth to glide.
    #[test]
    fn a_note_written_to_overlap_the_next_one_still_overlaps_it() {
        // `query` has already stretched the event, so what arrives here is the
        // stretched end: a step of 0.5 held for one and a half of itself.
        let overlapping = Note::from_event(&to(1), 60.0, &[], 0.0, 0.75);
        assert!(overlapping.off_secs > 0.5, "got {overlapping:?}");
    }

    /// Nothing about the gap may turn a note into one the queue reads
    /// backwards.
    #[test]
    fn a_note_whose_end_precedes_its_start_is_left_where_it_is() {
        let backwards = Note::from_event(&to(1), 60.0, &[], 2.0, 1.0);
        assert_eq!(backwards.off_secs, 1.0);
    }

    /// The anchor exists to predict audio time between the audio callback's
    /// steps, so what it must not do is reproduce them.
    #[test]
    fn the_anchor_predicts_audio_time_from_elapsed_wall_time() {
        let at = Instant::now();
        let anchor = Anchor { audio_secs: 10.0, at };
        let later = anchor.predict(at + Duration::from_millis(500));
        assert!((later - 10.5).abs() < 1e-6, "got {later}");
    }
}
