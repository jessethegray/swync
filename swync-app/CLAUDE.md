# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Swync: a live-coding language and its editor, shipped as a Tauri 2 desktop app. The frontend (`src/`) is React 19 + TypeScript + Vite, with CodeMirror 6 as the editor. The backend (`src-tauri/src/`) is Rust and holds the whole language — lexer, parser, lowerer, audio-graph realizer, pattern scheduler — plus the audio engine (`fundsp` for DSP, `cpal` for the output and input streams).

The repo directory is `brap`; the product, npm package and Rust crate are all `swync`.

## Commands

```bash
npm install && npm run tauri dev
```

That is the only way to run the app — `npm run dev` alone serves the frontend with no backend, so every `invoke` fails.

| | |
|---|---|
| Typecheck + build frontend | `npm run build` (`tsc && vite build`) |
| Bundle the app | `npm run tauri build` |
| Rust tests | `cargo test` from `src-tauri/` |
| One Rust test | `cargo test the_test_name` from `src-tauri/` (names are sentences: `cargo test master_volume_scales_the_output`) |

There is no linter and no frontend test runner. The Rust suite is the test suite — ~725 tests, mostly `#[cfg(test)]` modules beside the code, with larger ones in `lowerer/tests.rs`, `imports/tests.rs`, `files/tests.rs` and `library/tests.rs`.

Three tests fail on a fresh clone and always have: `the_example_project_compiles`, `every_example_compiles_and_realizes` and `a_program_without_imports_needs_no_folder`. The first two want fixture files under `examples/`, which is not checked in; the third is confused by a stray `src-tauri/patterns.swync` if one is sitting there — and one is checked in, so it fails on every clone, not only a dirty one. Confirm against `HEAD` before assuming a change caused them.

`.github/workflows/test.yml` runs on every pull request into `main`, and on `main` itself so that a cache exists for those pull requests to restore. It is the same suite with those three `--skip`ped, plus `npm run build` in both `swync-app/` and `website/`, a diff of the website's generated language reference against `lang.rs`, and `scripts/check-version.sh`. It counts the skips and fails if the number is not three, so a new test whose name contains one of those three strings gets caught rather than quietly filtered out. Nothing there checks formatting: this tree is not rustfmt-clean.

`[profile.dev]` in `Cargo.toml` raises `opt-level` for this crate and to 3 for dependencies. That is not incidental: an unoptimized `fundsp` misses the real-time deadline and the audio crackles. Leave it alone.

## The eval pipeline

`run_code` in `src-tauri/src/lib.rs` is the spine — the editor's play key (`Cmd/Ctrl + ,`) lands there, and reading it explains most of the backend. What it is handed is the project's `main.swync` rather than whatever tab is in front; see Projects below. In order:

1. **`parser::parse`** — `logos` lexer (`parser/lex.rs`), `chumsky` parser (`parser/parser.rs`) → `Vec<SwyncItem>`.
2. **`imports::expand`** — resolves `use` into one flat program. After this pass no modules exist, only definitions with longer names, so nothing downstream knows about files. It is also the last moment anything knows *which* file a definition came out of, so it is where a module's `load` paths are made absolute (see Libraries below).
3. **`samples::load`** — decodes what `load` names. This is the only thread allowed to touch a disk; the scheduler thread builds a voice per note and must never block on I/O.
4. **`lowerer::lower_with_samples`** — evaluates the program into two artifacts: a `SwyncGraph` (the persistent signal graph) and pattern `Binding`s.
5. **`swync_graph::realizer::realize`** — turns the graph into a `fundsp` `Net`.
6. Publish: the net is crossfaded into the engine's one slot; instruments then patterns go to the scheduler.

Every stage returns `Result<_, Diagnostic>` tagged with a `Stage`, and nothing is swapped in until all of them agree — a program that does not compile leaves whatever is playing alone. Diagnostics surface in the editor's problems panel.

**Two things make sound, by different routes.** The graph is continuous and lives in `engine.slot`, replaced by a 0.2s crossfade. Patterns are discrete: the scheduler thread (`scheduler/scheduler.rs`) free-runs from app start, wakes every 25ms, and pushes voices into a `Sequencer` 0.2s ahead of the audio clock. An eval never "triggers" the scheduler — it swaps the state the scheduler reads on its next pass. Ordering inside `run_code` matters and is commented where it does: instruments before patterns, clock reset before patterns are published.

**A performance has one origin, and an eval joins it rather than starting it.** Everything *placed* — `play_once`, `playn`, a rate curve, every section chain — counts its bars from `Patterns::origin`, so which bar that is decides whether an edit is heard where the piece already is or deals the arrangement out again from the bar the edit landed on. `origin_for` in `lib.rs` is the whole rule: an eval carries across the origin it finds already playing, and only three things start a new one — silence, an arrangement whose last bar has gone by (`Patterns::ends_by`, since a finished piece stays published), and a clock whose epoch has moved, which is what a followed `Start` does to bindings it never touches. A plain `play` reads none of this and keeps the transport's grid either way (`joins_in_progress`). The cost is deliberate: a `play_once` typed into a performance whose bar has passed is not heard until the piece is started again.

## Recording

`src-tauri/src/recorder/` — the record button's other half. The tap is in
`render` in `engine.rs`, on the block the device is about to be filled from, so
a recording is the graph, the sequencer and the master fader together and
nothing earlier.

That puts half of it on the audio callback's side of the deadline, and the
split is the whole design. `Tap` is a fixed ring of `AtomicU32` sample bits,
allocated once at startup and sized in seconds; the callback's entire part in a
recording is copying a block in and releasing one index. A writer thread drains
it every 25ms and writes the file. When nothing is being recorded the callback
does one relaxed load and returns.

Three things follow from that and are what the tests pin:

- **A ring that overruns drops the block and counts it.** The writer thread
  would have to fall four seconds behind for this to happen. Waiting instead
  would put a gap in the *output*, which everyone in the room hears, rather
  than in the file, which one person notices later.
- **The file's header is patched every second** (`wav::Writer::checkpoint`), so
  a take that ends in a crash is still playable. A WAV whose sizes are still
  zero is not a short recording, it is nothing.
- **A failure on the writer thread has no command to be the answer to.** A
  disk filling up mid-performance is collected by `recording_state`, which the
  editor polls twice a second while a take runs — the same poll that draws the
  clock on the button.

Record plays as well, and the order is load-bearing: `toggleRecording` in
`App.tsx` arms the tap and *then* evals, so the 0.2s crossfade the program
fades in on is inside the file rather than in front of it. `play` returns
whether the eval compiled for this one caller — a take that began with a
program nobody could compile is stopped at once, since what it would otherwise
hold is the silence after a typo. Stopping is the mirror — `stop_audio`, then
`FADE_OUT_DELAY`, then close the file. The wait is the point: the graph fades
out over 0.2s, and a file closed before that landed ends mid-waveform on a
click.

`settings.rs` is the app's own settings file, in the config dir beside the
remembered session, and holds the output folder, the format, and the two audio
devices (see Audio in below — an interface is on a desk, not in a piece). It is separate
from `project.rs` on purpose: a project's file travels with the piece, and an
absolute output path would be meaningless on anybody else's machine. The
formats come from `recorder::formats()` rather than a TypeScript list, for the
same reason `lang.rs` serves the builtins — a dropdown offering something the
recorder cannot write is a promise broken after the take.

`layout.rs` is the third file in that config directory, and holds the window's
size and place and how the two side panels were left. Its own file rather than
a field in the settings because of who writes it: the settings are sent whole
from the panel that shows them, while a window being dragged is written from
the backend several times a second and nothing in the frontend knows it moved.
One file holding both would have each writer flattening the other's half. The
window is put back in `setup`, before it is shown — which is why the window is
configured `"visible": false`: one that appeared at the config's size and then
jumped to the remembered one would have told the truth twice. A remembered
position on a screen that is not there today is dropped while its size is
kept, the way `usable_session` drops a file that has been deleted. The panels
are the frontend's, restored over the defaults it declares, and a view name it
no longer recognises leaves that panel on the view it starts on.

## Audio in, and changing devices

`src-tauri/src/audio_in/` is the recorder run backwards, and reading
`recorder/` first explains most of it: the same fixed ring of `AtomicU32`
allocated at startup, the same single-producer/single-consumer ordering, the
same refusal to let either side wait for the other. What is different is that
the two ends are two *devices*, whose clocks drift even when they agree on a
rate. So the reader keeps a **lead** derived from both sides' buffer sizes,
renders silence until the ring reaches it, silences and re-primes when it falls
behind, and throws away what it gets ahead by. Both are counted and reach the
settings panel, because a machine doing either constantly is one whose buffer
sizes want looking at and nothing else would say so.

Two things there are load-bearing:

- **The ring is drained once per rendered block, into a block read by index.**
  `input(0)` may be written twice in a program and both must hear the same
  thing; a node pulling from the ring would consume a frame the other could not
  then have. It is also why a sequencer voice hears the same input as the
  persistent graph.
- **`bus()` is a `OnceLock` singleton.** `realize` is a pure function of the IR
  called from three threads and a dozen tests, and what it would otherwise have
  to be handed is the same value every time. `audio_in::exclusive()` serialises
  the tests that touch it; tests with a bus of their own do not need it.

`input(channel)` refuses only what a program can get wrong on its own — a
negative, fractional, or impossibly large channel. A channel the device merely
*does not have tonight* is silence, so a piece written against an interface
still compiles on the laptop it is edited on.

`meter.rs` is both meters' half of this: peaks published from the audio
callback, one atomic per channel per block rather than per sample, reset by
whoever reads them. That reset is why **`audio_levels` must have exactly one
caller** — two would each see half the transients. The caller is the poll in
`App.tsx` that feeds `component/Meters.tsx` in the title bar, and it runs only
while something is playing or an input is open. The `out` peak is taken beside
the recorder's tap, on the block the device is about to be filled from, so a
meter and a take agree with each other and with the room.

**Changing the output device is mostly a sample-rate problem.** The device is
easy; what is hard is that everything counting frames was built for the last
one. `set_output_device` in `lib.rs` is the list, and it is the place to add to
if anything else ever starts counting frames: the graph (re-rated through the
*frontend* net, since fundsp sends re-rated units over on `commit` precisely
because re-rating may allocate and the audio thread may not), the clock (which
rebases rather than re-divides, or bar time jumps by however long the app has
been open), the sequencer (via `SchedulerState::set_sample_rate`, since the
scheduler thread owns the `Sequencer` outright), the recorder's tap, and the
input, which is a second device feeding the same graph. It is refused outright
mid-take: a WAV names one rate for the whole file.

The audio thread no longer parks forever — it holds the stream and waits on a
channel, because a `cpal::Stream` is not `Send` and the thread that built it is
the only one that may drop it. The backend is behind a `Mutex` for the same
reason: it outlives the streams that render it, and only one backend can be
taken from a `Net`.

**A device that will not start takes about nine minutes to say so.** Measured,
twice: an output another application was holding, and an input whose microphone
permission was never granted. `cpal`'s own start timeout is documented as one
not every backend honours and CoreAudio does not honour it, so the two
constants in `devices.rs` are what actually bind — and they bound *the wait for
an answer*, not the open. Three things follow, and each is why some piece of
this is shaped the way it is:

- **A switch to a different device builds the new stream before dropping the
  old one.** Letting go first would make those nine minutes silence in a room.
  Reopening the device we are already on — which is what putting one back
  is — still lets go first, since some hosts refuse a second stream on an open
  device.
- **A switch that times out is taken back rather than given up on**, by queuing
  a request for the device we were on. Nothing was re-rated, because the caller
  never got a rate, so a device that started a minute late would be rendering
  against a clock counting in the old one.
- **Startup asks for the remembered input without waiting** (`Input::request`),
  and the input thread owns what `status` reports rather than the caller — so a
  caller that stops waiting has given up on the answer, not on the device.

While a device is wedged the thread is stuck in the OS and further switches
queue behind it, each timing out in turn. What matters is that the device that
was already playing keeps playing throughout.

Devices are identified by `cpal::DeviceId`, not by name (`devices.rs`). Both
are kept: the id is what a remembered choice is matched on, so two identical
interfaces on one desk stay two devices, and the name is what a sentence about
a missing one has to use. `src-tauri/Info.plist` carries
`NSMicrophoneUsageDescription` and is not optional — macOS *terminates* a
process that opens an input stream without one.

## MIDI out

`src-tauri/src/midi/` — a pattern played by gear outside the machine, written
where an instrument would go:

```
play(bass, midiout("deluge", 1), vel: [1, .6, .8], chan: [1, 1, 10])
```

**A MIDI port is named in the program; an audio device is chosen in the
panel.** That is the whole reason `ports.rs` is not a copy of `devices.rs`, and
it is a claim about what belongs to what: which interface is on the desk is a
fact about the desk, while which synth a part is written for is a fact about
the piece. So a port is matched by a case-insensitive substring of its name —
real port names are long and vendor-shaped, and a program is typed live — or by
its number in the platform's list.

Which raises the question the panel alone answers badly: how does anybody know
what to type? **The editor completes them** — `midiout("` offers this
machine's ports, with each one's number beside it, exactly as every other name
in the language is found (`src/swync/ports.ts`, and Frontend shape below). The
settings panel still lists them, but for the question you ask when you are
*not* writing: whether the thing you just plugged in showed up. That is also
why the panel offers nothing to click.

A port that is not connected is a **warning**, not an error. `run_code` returns
`Result<Vec<Diagnostic>, Diagnostic>` for this and nothing else so far: the
`Ok` is what a run that happened had to say about itself. `Diagnostic` grew a
`Severity` to carry it, and the problems panel draws a warning amber and does
not call the run a failure. The trade is the same one `input(channel)` already
settled — a piece written for a rack has to stay editable on a train.

`Binding.target` is a `Target`, which is either an instrument's name or a
`Destination`. Putting it there rather than making MIDI a second kind of
binding is what makes `playn(riff, midiout("deluge"), 4).then(chorus)` mean
what it looks like: every arrangement combinator, every lane, and `rate` sit
above the target and learn nothing about it.

**Timing is the part with a design in it.** The scheduler still decides the
notes on its 25 ms pass, a fifth of a second early, but it hands them to
`out.rs`'s own thread with the audio time each is due at. That thread wakes
every millisecond and does nothing else, because unlike a voice — which is
handed to the sequencer with a start time and placed to the sample — a MIDI
message is only ever sent *now*, and 25 ms of jitter on a snare is audibly
loose playing.

It cannot wait on the audio clock directly, either: that advances a whole
buffer at a time, so reading it in a loop gives a staircase whose step is the
device's buffer. So the thread keeps an **anchor** — one pairing of an audio
time with an `Instant` — predicts audio time from elapsed wall time between the
callback's steps, and reconciles the two slowly (`CORRECTION`) rather than
snapping, since a sound card's clock really does run at a different rate from
the system's. A disagreement past `RESYNC_SECS` is a device switch or a stall
rather than drift, and rebuilds the anchor outright.

The correction pulls *towards* the audio clock, which has a consequence that
bites in tests and never in the app: against a clock that is not advancing, it
cancels wall time out and the prediction settles short rather than climbing, so
later messages never come due. A running app cannot do that — the audio
callback is what advances the clock — but a test must play the callback's part,
and `a_note_reaches_a_real_port` says so.

The **send offset** (settings panel, `midi_offset_ms`) is not a fudge factor:
audio time counts frames *rendered*, and a rendered frame is still in the
device's buffers, then a converter, then whatever is at the far end of the
cable. None of it is knowable from here. It is in `settings.rs` rather than the
project for the reason the audio devices are, and `set_settings` applies it as
well as writing it because it is dragged while notes are playing.

**Held notes are tracked and released individually**, never with an
all-notes-off: what that would also silence is everything else using the port.
Both ends of a note are queued together at note-on time, so a note whose off
was still to be decided cannot exist. A stop clears the queue and releases
exactly what is sounding, and so does the MIDI thread's own disconnect — a note
left on is a drone that outlives the process, on gear that has no idea the
thing playing it has quit. A note retriggered while still sounding is released
first, because the wire has no way to say *which* middle C to stop.

Three tests are `#[ignore]`d because they ask the platform rather than a
fixture: `what_this_machine_actually_reports` prints the ports,
`a_note_reaches_a_real_port` sends a note through a loopback bus (the IAC
Driver on a Mac, loopMIDI on Windows) and reads the bytes back, and
`the_bus_hears_a_real_port` does the same in reverse — it opens the loopback as
an output, sends, and checks the input bus heard it. Everything else about MIDI
runs against a made-up port list or calls `receive` directly, because the suite
has to pass on a machine with no MIDI on it.

## MIDI in

`src-tauri/src/midi/input.rs`. Two quite different things arrive down one wire
and leave by different roads, and the split is the whole design.

**A controller is a value that is always there.** `cc("push", 74)`,
`bend("keys")` and `aftertouch("keys")` are graph nodes, read at audio rate —
so they live in atomics and are never locked against, exactly as `audio_in`
does. **A note is a thing that happened at a moment.** It cannot be read, only
delivered, once, to the one thread allowed to push a voice — so notes queue
behind a mutex, which is safe precisely because the audio callback never
touches them.

**A port is interned, not looked up.** A node reads its controller on the audio
callback and cannot hash a port name, so what it holds is a **slot**: a small
integer, fixed for the process, indexing a table allocated at startup.
`slot_for` hands them out keyed on the selector *as written* and **touches no
hardware** — which is what lets a thousand tests lower a `cc` without opening
anything, and lets a program compile the same on a laptop as on the rig.
Opening is a separate step, `ensure_open`, taken only by `run_code`.

Slots are finite (`MAX_PORTS`) and never released, which is right for a program
and wrong for a suite — so `midi::input::exclusive()` clears the table as well
as taking the lock. That is why the guard and the reset are one function. **Any
test that lowers a program naming a MIDI port has to hold it**, including tests
that are not about MIDI at all: `lang`'s two `receives_tests` call every name in
the table, four of which intern. Forgetting it does not fail the test that
forgot — it leaks a slot into whatever is running beside it — so `slot_for`
asserts the guard is held rather than leaving that to be noticed. A run with
`--test-threads=1` names any offender outright.

**Smoothing is in the node, not the bus.** The bus holds what arrived, which is
the truth; each read site decides what to do between arrivals. Ten
milliseconds, because seven bits arriving a few hundred times a second is a
staircase and a staircase on a cutoff zippers. `ControlNode` is therefore *not*
stateless the way `InputNode` is — and that matters, because the scheduler
builds one per note, so a `cc` inside an instrument starts each note where the
knob is rather than sliding up to it.

### Playing a keyboard

`play(midiin("keys"), lead)` — the mirror of `midiout`, in `play`'s other slot,
which is why `midiin("keys").play(lead)` works without anything being added for
it (`a.f(b)` is `f(a, b)`). `Binding.source` is a `SourceOf`: notes written
down, or notes as they are played.

They leave `schedule_pass` by different roads because **a keyboard cannot be
queried**. The scheduler works a fifth of a second ahead; a key pressed now
would be asked about for a window already gone past. So `Patterns::query`
skips live bindings and `play_live_notes` picks them up instead, pushing each
note as close to now as the sequencer will take it.

That is also why the scheduler's tick is no longer fixed. A pattern does not
care when the thread runs — it is placed to the sample — but every millisecond
between a key press and the push is latency under somebody's fingers. So
`LIVE_TICK` is 2 ms and applies only while a live binding exists; every session
that plays no keyboard wakes as rarely as it always did.

**A key coming up is a release, not a cut**, and that distinction is what
`realizer::Gate` exists for. `env`'s fifth argument is its gate time, baked in
as a constant at build — so a voice built for a held key has its release
scheduled at a time that never arrives, and ending the sequencer event instead
fades it out over `FADE_OUT_SECS`. That is twenty milliseconds: a click where a
two-second release was written, which is exactly what was reported from the
field. So a live voice goes through `realize_gated`, whose envelope reads its
gate from an `AtomicU32` per voice; the note-off writes the moment into it in
the voice's own time and leaves the event open for `tail_secs` so the release
can finish.

Only an envelope written to last **the whole note** is gated — `realize_gated`
matches the constant against the length the voice was built for, so
`env(.., dur)` gates on the key while an `env(.., 0.05)` blip in the same
instrument keeps its own length. Overriding both would make every short shape
sustain until the key came up.

A **stop** still cuts, and deliberately: stop means the room goes quiet now,
not in two seconds. `letting_a_key_go_does_not_cut_the_instrument_short` and
`a_stop_cuts_a_held_key_rather_than_releasing_it` are the pair that pin the
difference, and the first measures *past* where a cut would have landed —
taken from the moment of release it would pass either way, since a cut is a cut
precisely because everything before it is fine.

`dur` inside a live voice is `HELD_SECS`, the longest a key may be held. There
is no honest number at build time, and this one is at least an upper bound —
but times *derived* from `dur` are then minute-scale, so an instrument written
`env(.., dur/2, ..)` decays over thirty seconds on a keyboard. It is kept large
rather than musical because the gating rule compares against it: a small `dur`
would collide with ordinary envelope times written out in full.

The push cap is also a safety net: a note-off can be lost to a pulled cable,
and what that would otherwise leave is a voice droning until the app is quit.

Velocity reaches an instrument **only if it declares a `vel` parameter**
(`Instruments::declares`). An instrument that never named it never sees it,
which is what lets one written long before any of this play from a keyboard.

A keyboard takes no rate and no `playn`: it has no passes to count, and both
would be a claim the program makes and the music does not keep. Both are
refused rather than ignored.

## MIDI clock

The two directions are **not** mirrors, and the asymmetry is the design.

**Sending is in the program.** `midiclock("deluge")` — that synth's arps and
delays have to line up with the part written for it, which is true wherever
the piece is played. Ticks are sent by `midi::out`'s thread, which already
wakes every millisecond: at 120 bpm a tick is every 21 ms and at 180 every 14,
so the scheduler's 25 ms pass could not place them. They are counted against
**bar time** rather than an interval (`Player::clock`), which is what keeps
them in step with the music rather than merely regular — and means a tempo
change, including one under an `accel`, is followed with no code here knowing
about it. A stall past `MAX_CATCHUP_TICKS` resynchronises rather than spraying
the backlog, which a drum machine reads as a burst of tempo.

**Following is in the panel** (`midi/follow.rs`, `settings.midi_clock_source`).
Whether *this* machine is the slave tonight is a fact about the rig: the same
piece is master in the studio and slave when somebody else's box is running the
room. So nothing in the language mentions it.

What arrives is twenty-four ticks a quarter note and nothing else — no tempo,
no bar number. Two separate things have to be made of that, and confusing them
is the trap:

- **Tempo** is how fast they come, measured **across a window** of a quarter
  note rather than smoothed tick by tick. MIDI jitter is not noise around a
  true time, it is a *delay*: a tick sits behind something else on the wire and
  the next arrives at its own correct time, so one interval is long and the
  next short by the same amount. A window spans both and is already right.
  Measured: a whole missed tick moves an exponential average by 5.7 bpm and
  the window by 1.5; ordinary jitter costs 0.5.
- **Phase** is where in the bar we are, from `Start` and the tick count.
  Tempo alone drifts a whole beat away over minutes even when it is nearly
  right. `Clock::nudge_bars` is what corrects it and exists only for this —
  `set_cps` deliberately *holds* bar position across a tempo change, which is
  right for a tempo drag and exactly wrong here. The correction is weak and
  has a dead band, because bar time is what every playing pattern is placed
  against and a correction anybody can hear is worse than the drift it fixes.

A clock that stops arriving does **nothing**: the transport carries on at the
tempo it was following, and `Follow::status` says `Lost` so the panel can. The
alternative is silence in a room, and the last tempo is the best guess there
is. Turning following off is the same — nothing about ceasing to follow is a
reason to stop playing.

A box already running when swync starts listening never sends a `Start`, so
its first tick starts the follower; waiting would mean ignoring it until it
happened to stop. An interval outside `MIN_BPM`..`MAX_BPM` is clamped, since a
wire hiccup that reads as 3000 bpm takes seconds of audible wrongness to walk
back from.

`the_clock_goes_out_and_comes_back` is the fourth `#[ignore]`d hardware test:
it sends clock to a loopback bus and follows it back on a second transport.
Measured 119.35 bpm for a clock sent at 120. It also has to advance its fake
audio clock by *real elapsed time* — a fixed advance per `sleep` runs slow,
which reads as 82 bpm at the far end, and that is a mistake worth not making
twice.

## Controls in the panel

`src-tauri/src/controls.rs`, `lowerer/controls.rs`, `src/ControlsPanel.tsx`. `slider("cutoff", 200, 5000)` and `toggle("mute")` draw a control in the right panel and read it from inside the graph at audio rate; `trigger("fill", fill)` draws a button that starts a section. It is **MIDI in read from the screen instead of a wire**, and nearly all of it is the same design: one atomic per control, written by one side and read by the audio callback without a lock; a name interned to a **slot**, because a node cannot hash a string on the callback; and interning that touches no hardware and no window, so a thousand tests can lower a slider and a program compiles the same wherever it is opened.

**The panel has no way to add one**, and that is the design rather than an omission. A control exists by being written at the place it is used, so the control and the thing it controls are one line of text and a piece that travels takes its controls with it. It is the claim `midiout` makes about a port and the opposite of the one an audio device makes.

**One table holds every kind**, and `Kind` is the only thing that differs. A slider and a toggle share a slot, an atomic, the session's memory of where they were left, and one `NodeKind::Control` that reads and smooths — because everything that makes a control a control is the same for both. What differs is the travel: a slider has a range, a toggle has two ends, and only the panel has to care since it is the only thing that draws one. A toggle's ends are smoothed like everything else, which is what keeps switching a part in from clicking.

One name is one control, so a name declared as both is the same collision two ranges are, and gets the same answer with its own words — a slot has one travel.

Four things carry weight:

- **Which sliders exist is read off the syntax, before lowering** (`controls::declare_in`, over `parser::walk`). An instrument's body is not lowered until the scheduler builds a voice from it, so a `slider` inside a `fn` would otherwise reach the panel only once a note had played — or never. That is the same argument `samples::paths_in` makes about `load`, which is why the walk is now one function they share rather than two copies of an exhaustive match.
- **Slots are the session's memory, and that is what makes a slider usable.** A name keeps its slot and its position for the life of the process, so dialling a filter in, editing the line above it and evaluating leaves the filter where it was. The number written in the program is therefore a *starting point*, used the first time a session sees the name; a range edited narrower clamps whatever was dialled in under it. Quitting forgets.
- **One name is one control, and a slot holds a *fraction*.** Written into two instruments it is one control moving both. Two different *ranges* under one name are not a disagreement: each place maps the shared 0-to-1 position into the range written there, so at halfway `slider("cutoff", 200, 5000)` reads 2600 and `slider("cutoff", 0, 1)` reads 0.5. That is `cc`'s design — one physical knob, mapped at each read site — and it is why `PanelNode` carries its own `lo`/`hi` and the graph node takes `[slot, lo, hi]`. Two things fall out: a range edited between runs needs no clamping, since a fraction is inside every range; and the panel draws the range declared *first*, exact for that place and proportional elsewhere. Only a clash of *kind* still warns, because a slot has one travel.
- **A slider is both a signal and a number**, and which one depends on where it is written. `Value::Slider` carries the node *and* the position, and only the places that demand a compile-time number unwrap it (`expr::slider_number`, and `realizer::constant` for a parameter baked into a unit at build). It is deliberately **not** folded into `as_data`, which every arithmetic operator runs both sides through — `slider("level") * 0.5` has to build a multiply in the graph, and folding it there would go quiet under the finger. The mirror of that is `expr::signal_node`: anything asking "is this audio?" by matching `Value::Signal` has to ask it instead, since a variant match does not fail to compile — it drops the slider, which sounds like a line that does nothing.

### A trigger

`trigger("fill", fill)` fires a section when it is pressed, and **the section is lowered when the program runs rather than when the button is hit**. That is the same move `wthen` makes and it is made for the same reason: lowering at press time would put a compile on a UI event and a program's code on a thread with a deadline, while leaving it un-lowered would mean the scheduler had to run the language. Writing every binding up front and gating them is what keeps the audio side knowing nothing about buttons.

What it learns instead is **one number per button**: the bar it was armed at, `NEVER` until somebody presses it. `Binding.button` marks the section's bindings, and `windows` counts their `start` from that bar instead of from the origin — the one place in the language a start is measured from something other than the origin. Infinity for unpressed is not a trick: every question asked of it is "has this bar come yet?", and against infinity the answer is no, forever, with no branch anywhere to forget.

Two things there are load-bearing:

- **A press is quantised past the lookahead, not merely onto the grid** (`controls::arm_at`). The scheduler works a fifth of a second ahead, so a beat inside that horizon has already been decided and some of its notes pushed; arming there asks a window to open in the past and yields a section that starts late and clipped. So the horizon is crossed first and the grid found after. At ordinary tempos that is simply the next beat, and at fast ones it is occasionally the beat after — the honest cost of quantising something that has to be decided before it is heard.
- **A button's section never "joins in progress".** A plain `play` inside a fired section has `start == 0` and no end, which is exactly the shape `joins_in_progress` waves through so a re-eval does not re-phase a running loop. A button's section has not been running — it has not started at all — and without excluding it there, the one thing a button exists to prevent is what it would do: sound from the moment it was lowered. `an_unbounded_section_on_a_button_does_not_join_in_progress` is the test that pins it.

Pressing again overwrites the bar, which is what makes a second press restart rather than queue. What it cannot take back is a voice already pushed into the sequencer, so up to a lookahead of the old pass may still sound past the new one's start — the difference between cutting a section off and never having played it.

That last split is what the panel's **"on run"** badge is about. A baked reading cannot follow a drag: the parameter is not a port, and what the graph holds is the number the slider stood at when the program compiled. So the slot is marked as it is read that way — from the lowerer, and from the realizer on whichever thread built the voice — and `App.tsx` runs the program again when such a slider is *released*. Not while it moves: a run crossfades the graph and republishes every pattern, so one per pointer event would stutter the music continuously, which is a worse answer than catching up when you let go.

The panel polls only for that badge, slowly and only while it is showing, because a slider inside an instrument is not read either way until the first note plays. Everything else it knows comes from the run, and the *position* is the frontend's own: between runs it is the only writer, which is what keeps the poll from dragging a thumb back under somebody's finger.

**Where a slider was written is answered by searching the project, not by the compiler** (`App.tsx`, `revealSlider`). `imports::expand` folds every file into one program and is the last pass that knows which file a definition came from; threading that back down to an expression inside a `fn` body would undo a boundary that exists so nothing downstream has to know about files. A name written once — the ordinary case — is found exactly by looking for it.

## Bars, passes, and the one place "cycle" survives

Musical time is counted in **bars**. A **pass** is one trip through a pattern: a list of shares is one pass and fills the bar in any signature, while a pass written in note values is as long as its values add up to and rotates against the bar when they disagree. The two words are not interchangeable, and the tests say which they mean — `a_three_beat_pass_fills_a_three_four_bar` against `a_three_beat_pass_takes_three_quarters_of_a_four_four_bar` is the whole distinction in two cases.

How long a bar is comes from `Meter` in `scheduler/clock.rs`, a project setting saved in `swync-project.json`. **It reaches only two places**, and both are conversions *out of* beats: `to_pattern_timed` in `lowerer/play.rs`, where a metrical pass becomes a fraction of a bar, and the `bpm` builtin, which answers in bars per second. Everything else — the scheduler, `Rate`, `Pattern::query`, every `Binding` — already counted bars and never asks how many beats made one, which is why a signature could be added without touching them.

The clock keeps `cps` as a field name, and that is deliberate: it is bars per second, but `bps` reads as *beats* per second and the two differ by the whole signature. It is the only survival of the older word, and it survives as an abbreviation rather than as a unit.

Two orderings are load-bearing. `set_bpm` converts through whatever meter is running, so **`set_meter` comes first** when a project opens — `open_project` says so. And a signature change moves the transport but does not re-lower anything: what is playing keeps the bar it was lowered against until the next eval, the same gap a tempo drag leaves on the persistent graph.

## Language surface lives in one table

`src-tauri/src/lang.rs` holds every callable name with its arity, parameter names, `receives`/`returns` kinds and doc string. The lowerer dispatches off these tables, and the `language_metadata` Tauri command serves the same data to `src/swync/metadata.ts`, which drives highlighting, completion, signature help and the docs panel.

So **adding a builtin means adding one entry to `UGENS` (or `LIST_BUILTINS`, etc.)** — the editor picks it up with no TypeScript change. `ValueKind` is mirrored by hand in `metadata.ts`; the Rust test `every_builtin_receives_what_it_declares` keeps the declarations honest against the compiler.

## Projects, patterns, imports

**A project is played from its `main.swync`.** Play — the button, and `Cmd/Ctrl + ,` — runs that file, not the tab in front, so a project is a program in several files with one entry point the way a crate has a `main.rs`. `Cmd/Ctrl + Shift + ,` runs the file in front instead, which is how a module is auditioned while it is being written. Three things follow. The file is created empty by New Project and opened as the new project's first tab (`project::create_main`, with `create_new` so adopting a folder never writes over a piece — and `newProject` opens whatever `main_file` then names, which for an adopted folder is the piece that was already there); a project without one — every project made before it existed — plays the tab in front exactly as it always did, which is what `main_file` answering `None` means; and the file play runs may not be open, so `App.tsx` tracks the run's `Source` by *path* as well as by tab id, since the problems panel has to name a file nothing has on screen. `main.swync` is read from its tab when it has one, because an edit you can see is an edit you expect to hear — the one exception to imports reading what is saved.

**Opening a project closes the tabs that belong to another one.** `chooseProject` in `App.tsx` keeps a tab whose file is inside the folder being opened — that is the same file, still being edited, in the project it lives in — and closes the ones left over from the project you came from, because they are what ⌘S and `Cmd/Ctrl + Shift + ,` are aimed at while a different project's tree is on screen. Composer tabs close with them: `loadPatterns` mints fresh ids for a project it has not read before, so a composer holding the old project's id would be left saying the pattern no longer exists. What is *only* in the app never closes — a dirty buffer is the last copy of that work, the same reason a trashed file keeps its tab, and an untitled tab that has been typed in has never been on disk at all — so what a sweep can take is a clean, saved file from somewhere else. Reopening the project that is already open is not a move, and keeps its own composers.

A project is a folder with a `swync-project.json` (name, bpm, meter, volume), written debounced as you change things. What belongs to the machine rather than the piece — the recording folder and format — is in the app config dir instead; see Recording above. `src-tauri/src/project.rs` and `files/` own it. Two other files may sit beside it — a `swync-library.json` naming what the project exports as, and a hidden `.swync/libraries/` holding vendored libraries — both covered below.

Drawn patterns from the right-hand panel are a real file, `patterns.swync` at the project root, folded into every eval as an implicit `use patterns::*`. The panel also sends its patterns *with* the eval rather than relying on the write having landed, so `run_code` takes `Option<Vec<GraphicalPattern>>`: `None` means "the panel has nothing to say, use the disk", `Some([])` means "the panel read this project and it has no patterns" — only the second may hide a file on disk.

The project's folder is **watched** (`src-tauri/src/watcher.rs`, `notify`), which is why the tree has no refresh button. One `project-changed` event per settled 200ms burst reaches `App.tsx`, which bumps `projectVersion`; the tree re-reads the folders it has open, and the patterns and settings files are read again with it. Two filters keep that from firing on the app's own work: hidden paths are skipped, exactly as `list_dir` skips them, and content changes are ignored because the tree shows names. Anything else — including events the platform could not classify — refreshes, since an extra directory listing costs nothing and a missed file is the whole point of the watch.

That the tree now re-reads unprompted is what makes two things load-bearing. `useProjectTree` keeps every open folder open across a version bump, so nothing moves under a pointer reaching for it. And `fromWire` keeps a drawn pattern's id when the same project's file names that row again — the id is what a composer tab holds, and minting a fresh one on every re-read would take the pattern out from under an open tab.

`use` paths are routes, not names, and only ever go downward. Renaming or moving a file therefore rewrites the `use` lines that pointed at it (`files/reroute.rs`); a move that no rewrite can honestly follow still happens, and the broken imports are named in the problems panel. Imports read what is *saved*, so a module must be written to disk before a file that uses it is played.

Two rules hold across `files/`: nothing is overwritten (a collision is refused, never merged — except in `copy_path`, which takes the next free name beside it, since pasting a file back into the folder it came from is the ordinary case for a copy), and nothing is destroyed (deletes go to the platform trash).

The tree's Cut is `move_path` and its Copy is `copy_path`, which is the same act twice over: both land something somewhere and both then correct `use` paths. They correct opposite sides. A move rewrites every *importer* in the project, because the file they named has gone. A copy rewrites *the copy's own* imports and nothing else — the file everything imported is still where it was, and what changed is where the new file is reading routes from. That is why `copy_path` surveys only what it is about to copy.

## Libraries

`src-tauri/src/library/` — a shareable folder of modules, installed once and reachable from every project. A **pack** is a `.swyncpack`: a zip of `manifest.json` plus a `root/` whose contents are copied into a **store**, and a store is nothing but another folder a `use` resolves in. That is the whole design — `use kit::kick` already means `kit/kick.swync` beside the file, and an installed library is that same shape somewhere else, so importing, renaming and lowering learn nothing new.

Three things carry the weight:

- **`Resolver::locate` asks each root in turn and answers each in full before moving on** — beside the file, then `<project>/.swync/libraries/`, then the app config dir's store. A project file therefore beats an installed library of the same name at every step, so installing something can never change what a working project means. The roots reach `Workspace` via `set_libraries`, filled in by `run_code`: only the Rust side knows where the config dir is.
- **A pack owns exactly one top-level name.** Install refuses any entry under `root/` that is not `<name>.swync` or inside `<name>/`. That single check is why two libraries can never write the same file, and why there is no version resolution anywhere in here to get wrong. `install` returns `Outcome::Conflict` rather than replacing, having written nothing, so the editor can ask.
- **A module's `load` paths are rewritten to absolute during expansion** (`imports/rename.rs`, `Scope::relocating_samples`). Without this a library's samples resolve against whatever file imported it, which is the wrong folder — and it is the only reason a pack can carry audio at all. The program's own paths are left as written.

`library/` deliberately breaks `files/`'s trash rule: an installed pack is app-managed and reinstallable, and leaving a hundred megabytes of somebody else's samples in the Trash is worse than no undo. `export` audits before it writes, refusing an absolute `load` path, one that reaches out of the library, or a top-level `use` naming something the library does not carry — all three work on the author's machine and nowhere else.

## Frontend shape

`src/App.tsx` is deliberately the center — tabs, project state, transport, recording, panel wiring and all the `invoke` calls live there; the panel components are mostly presentational. Native menu items arrive as Tauri events (`file-new`, `project-open`, …) listened to in `App.tsx`.

`src/swync/` is the CodeMirror extension bundle. `swyncExtensions()` must be called once and memoized — CodeMirror reconfigures when the extension array's identity changes, which would discard completion state on every keystroke. Values that change (drawn pattern names, the docs callback, the `Symbols` cache) are passed as getters or long-lived objects for the same reason.

Completion reads the buffer with regexes, because the text being completed is half-written and the real parser would reject it. Three things it cannot get that way:

- **What a `use kit::*` brought in.** Those names live in a file the frontend has never read, so the `module_symbols` command runs the real expander and reports the spellings *this file* would write (`kick`, `kit::kick`, `k::kick`). `src/swync/symbols.ts` asks only when the document's `use` lines change, and answers nothing when the file does not expand — which is most keystrokes, and is the right answer while it is half-typed.
- **Which argument of a call the cursor is in.** `src/swync/callsite.ts` holds `callAt`, shared with signature help rather than written twice. It is what makes `play(pat, ` offer only playable `fn`s and `play(pat, kick, ` offer that instrument's lanes — both rules live in `lowerer/play.rs` and are otherwise invisible until the program is run.
- **What this machine's MIDI ports are called.** `src/swync/ports.ts` — and it is the answer to the question the MIDI design otherwise leaves hanging: a port is named in the program, so how does anybody know what to type? `midiout("` offers them. It is `Symbols` for hardware, and differs only in *when* it goes stale: a `use` line changes when the document does, while a port list changes when somebody plugs something in, which no edit reports. So it refreshes on a clock (`STALE_MS`) rather than on an edit, warmed from the update listener whenever the document says `midiout`.

`src/swync/indent.ts` is the last thing the frontend cannot read off the buffer alone: which line breaks end a statement. It mirrors `cont_next` in `parser/lex.rs`, so a line opening with `.` or `>>` is indented one step from the line that began the statement. It applies through `indentOnInput` rather than on Enter — a break after `some()` ends a statement until the `.` is typed, and the `.` is the only moment the answer changes.

**`dragDropEnabled` is on, and that is why `src/projectDrag.ts` exists.** With it on, the webview's drag handling belongs to wry, which claims every drag crossing the window — so the frontend is handed the *paths* of files dropped in from the Finder, and no `dragstart` fires anywhere in the page. With it off, a dropped file arrives as a browser `File` with no path, which is the one thing about it the project tree needs. Only one of the two is available, so the tree moves its own rows with pointer events (`useRowDrag`) and takes dropped files from Tauri's event (`useFileDrop`), both aiming at whatever folder is under the pointer via `folderAt` rather than at whichever row's handler an event reached. The cost of the trade is dragging text out of the editor, which CodeMirror offers over HTML5 drag and drop and which no longer arrives.

Icons are imported as components via `vite-plugin-svgr` (`import Icon from "./icon.svg?react"`), so they take a `className` and inherit `currentColor`. Styling is Tailwind 4 via the Vite plugin.

## Conventions

Comments in this codebase explain *why*, in prose, and are load-bearing — constants, orderings and refusals carry the reasoning that would otherwise be lost. Match that: a change that invalidates a comment's reasoning should update the reasoning. Test names are full sentences describing the behavior being pinned.

`README.md` is the user-facing manual, including the full function reference. It is the place to look for what a language feature is supposed to do, and the place to update when that changes.
