/**
 * The swync tokenizer, mirroring `src-tauri/src/parser/lex.rs`.
 *
 * A stream tokenizer rather than a Lezer grammar: swync's lexical structure is
 * flat enough that a generated parser would buy nothing but a build step.
 *
 * The one rule worth stating explicitly — **swync's only string is the path in
 * `load("...")`, and it has no escape sequences**. The JavaScript mode this
 * replaces treated the backtick rest token as the start of a template literal,
 * which swallowed the remainder of the file, and would read a backslash trigger
 * as escaping the next character. Both are ordinary one-character tokens here,
 * and a string runs to the next `"` or the end of the line — never past it.
 *
 * **This file imports nothing**, and describes its two inputs and its stream
 * structurally rather than by the types `metadata.ts` and CodeMirror give them.
 * That is what lets the tutorial site run this tokenizer over its code blocks
 * at build time: the editor's colours and the website's are one tokenizer, and
 * a website that has no editor in it pays for none of CodeMirror or Tauri to
 * say so. What is CodeMirror's about highlighting — the tag table, the theme,
 * the language — is in `language.ts`, which wraps what is here.
 */

/** As much of CodeMirror's `StringStream` as the tokenizer reads, written out
 *  so a caller outside the editor knows what it has to hand over. Signatures
 *  are `StringStream`'s exactly, since the editor passes the real thing. */
export interface TokenStream {
  eatSpace(): boolean;
  match(
    pattern: string | RegExp,
    consume?: boolean,
    caseInsensitive?: boolean,
  ): boolean | RegExpMatchArray | null;
  next(): string | void;
  skipToEnd(): void;
  /** At the start of a line. Read for one thing only: an enum's members
   *  separate on a line break as well as on a comma, so this is how the
   *  break-separated form is recognised. */
  sol(): boolean;
}

/** The metadata the tokenizer reads: the keyword list, and nothing else.
 *  `LanguageMetadata` satisfies it. */
export interface TokenizerMetadata {
  keywords: readonly string[];
}

/** Membership in the backend's builtin table. `BuiltinIndex` satisfies it, and
 *  so does a bare `Set` of the names — which is all the website has. */
export interface TokenizerIndex {
  has(name: string): boolean;
}

/** Matches `lex.rs`'s number regex, anchored for `StringStream.match`. */
const NUMBER = /^(\d+(\.\d+)?|\.\d+)([eE][+-]?\d+)?/;
/**
 * Mirrors `lex.rs`'s string regex: double quotes, no escapes, and no newline
 * inside — so an unclosed quote colours to the end of its line rather than
 * swallowing the rest of the file.
 */
const STRING = /^"[^"\n]*"?/;
const IDENT = /^[a-zA-Z_][a-zA-Z0-9_]*/;
/**
 * Mirrors `lang::note` with its octave spelled. A binding may still shadow a
 * note, which the highlighter cannot know, so this is a spelling test rather
 * than a resolution.
 */
const NOTE = /^[a-g][sf]?[0-9]$/;
/**
 * Mirrors `lang::note` without one: a pitch class, which is a note only where a
 * sequence has already given an octave — see `octaves` below for how that is
 * tracked. Checked *after* {@link DURATION}, since `e` is the eighth note and
 * the lowerer refuses it as a pitch rather than choosing.
 */
const PITCH_CLASS = /^[a-g][sf]?$/;
/**
 * Mirrors `lang::DURATIONS` and `lang::TUPLET`. Whole-string and single-letter.
 * A binding shadows one exactly as it shadows a note, which the highlighter
 * cannot know, so this is a spelling test rather than a resolution.
 */
const DURATION = /^[whqest]$/;
/**
 * Used until the backend's keyword list arrives, so a file is highlighted
 * correctly on the very first frame rather than rendering every keyword as a
 * plain variable. The backend list wins once loaded.
 */
const FALLBACK_KEYWORDS = ["fn", "let", "if", "else", "for", "in", "use", "as", "enum", "match"];
/** Longest first, so `..=` beats `.`, `>>` and `>=` beat `>`, `==` and `=>`
 *  beat `=`. */
const OPERATORS = [
  "..=",
  ">>",
  "=>",
  "==",
  "!=",
  "<=",
  ">=",
  "+",
  "-",
  "*",
  "/",
  "%",
  "=",
  "<",
  ">",
  "|",
];

export interface TokenizerState {
  /** The definer the previous token was, so the next identifier is a name being
   *  defined rather than one being used. Which one it was matters: only `enum`
   *  has to remember the name afterwards. */
  defining: "fn" | "enum" | null;
  /**
   * The previous token was a method dot.
   *
   * What may follow one is narrower than what may stand on its own: a name
   * after a dot is being called on what precedes it, and neither a pitch nor a
   * written note value is reachable that way. Without this, `Scale.e` colours
   * `e` as the eighth note and `xs.c4` as a pitch — readings the lowerer does
   * not have either, since both resolve as call names rather than through
   * `Expr::Var`.
   */
  afterDot: boolean;
  /**
   * The enums declared so far in the stream, which is what makes `Scale.major`
   * colourable at all — the tokenizer has no other way to know that `Scale` is
   * an enum rather than a list somebody is calling a method on.
   *
   * Only the ones written *above*, and that is the right answer rather than a
   * limitation: items lower in order, so an enum used before it is declared is
   * an unbound name (`an_enum_must_be_declared_before_it_is_used`). Colouring
   * a forward reference would promise something that does not compile.
   *
   * An array rather than a `Set` because CodeMirror's default `copyState`
   * slices arrays and would share a `Set` by reference — the per-line copies
   * have to be independent, or deleting an enum would leave its name colouring
   * every line below until the whole document was tokenized again. Membership
   * is a linear scan over a handful of names.
   */
  enums: string[];
  /** The previous identifier named an enum, so a `.` right after it opens a
   *  member rather than a method call. */
  receiver: boolean;
  /** The previous token was the `.` of an enum receiver, so this identifier is
   *  a member name. */
  member: boolean;
  /**
   * `{` nesting, and the depth an enum's body was opened at.
   *
   * Counted because a member's value may itself be braced — `x = if c { 1 }
   * else { 2 }` is a constant like any other — so "inside an enum" is a depth
   * rather than a flag. `null` when no enum body is open.
   */
  braces: number;
  enumBody: number | null;
  /** `(` and `[` nesting, so a member's value written over lines is not read as
   *  more members: `x = [0,\n 2]` separates nothing. */
  groups: number;
  /** The name after `enum` has been read, so the `{` that follows opens a body
   *  rather than a block. */
  awaitBody: boolean;
  /** The next identifier at member depth is a member being declared, rather
   *  than a name inside the value of one. Set where members separate — the
   *  opening brace, a comma, a line break — and cleared by the name itself. */
  expectMember: boolean;
  /**
   * One entry per open `[`: whether a note in that list has spelled an octave
   * yet, which is what makes a following bare letter a pitch rather than a
   * name. Pushed on `[` and popped on `]`, so a group inherits nothing and a
   * `let a = 3` two lines down is not coloured by a list that has closed.
   *
   * A stack rather than a boolean because the lowerer restores the register at
   * a closing bracket and the colours should go back with it, and each level
   * opens on whatever the one outside it had — the same inheritance
   * `Expr::List` does, so a group's notes colour like the line they sit in.
   *
   * CodeMirror's default `copyState` slices arrays, so the per-line copies do
   * not share this one.
   */
  octaves: boolean[];
}

export function startState(): TokenizerState {
  return {
    defining: null,
    afterDot: false,
    enums: [],
    receiver: false,
    member: false,
    braces: 0,
    enumBody: null,
    groups: 0,
    awaitBody: false,
    expectMember: false,
    octaves: [],
  };
}

/**
 * The token function, bound to one metadata snapshot.
 *
 * Names are CodeMirror's — `comment`, `number`, `keyword` and the rest resolve
 * through `StreamLanguage`'s built-in tag table, and the `swync`-prefixed ones
 * through the `tokenTable` in `language.ts`. See the note there for why the
 * swync-specific names must carry that prefix.
 */
export function swyncTokenizer(
  meta: TokenizerMetadata,
  index: TokenizerIndex,
): (stream: TokenStream, state: TokenizerState) => string | null {
  const keywords = new Set(meta.keywords.length ? meta.keywords : FALLBACK_KEYWORDS);

  /** Where a member of the open enum would be written: at the body's own brace
   *  depth, outside any list or call in a member's value. */
  const atMemberDepth = (state: TokenizerState) =>
    state.enumBody !== null && state.braces === state.enumBody && state.groups === 0;

  return (stream, state) => {
    // Before `eatSpace`, which moves off the start of the line. A member may be
    // separated by a line break instead of a comma, and this is the only place
    // that break is visible — the tokenizer is handed one line at a time and
    // never sees the character itself.
    if (stream.sol() && atMemberDepth(state)) state.expectMember = true;

    if (stream.eatSpace()) return null;

    // Everything one token tells the next is read into a local and cleared
    // here, so a branch below carries it forward only by saying so. Clearing at
    // the point of return instead means every new branch has to remember to
    // clear every flag — which is how `afterDefiner` came to be reset in six
    // separate places, one of them a line on its own between two rules.
    const { defining, afterDot, member, receiver } = state;
    state.defining = null;
    state.afterDot = false;
    state.member = false;
    state.receiver = false;

    // Comments run to end of line; there is no block comment form.
    if (stream.match("//")) {
      stream.skipToEnd();
      return "comment";
    }

    // Before operators: a number may begin with `.` (`.5`).
    if (stream.match(NUMBER)) {
      return "number";
    }

    // The path in a `load`. Matched before the trigger branch so the
    // backslash in a Windows-style path is part of the string rather than a
    // pattern step.
    if (stream.match(STRING)) {
      return "string";
    }

    // The two pattern literals. Neither can start an identifier, so their
    // position relative to the IDENT branch does not matter — they are kept
    // together because they are read together.
    if (stream.match("`")) {
      return "swyncRest";
    }

    if (stream.match("\\")) {
      return "swyncTrigger";
    }

    // Before the operator table, which does not hold it: the quote is a prefix
    // and nothing else, so there is no `'=`-style pair for a longest match to
    // settle.
    if (stream.match("'")) {
      return "swyncQuote";
    }

    // `match` is typed as `RegExpMatchArray | true | null`; a regex argument
    // always yields the array form.
    const ident = stream.match(IDENT) as RegExpMatchArray | null;
    if (ident) {
      const name = ident[0];

      if (defining) {
        // Remembered so that every later `Scale.` can be recognised. Written
        // down here because this is the one place the name is unambiguous — a
        // word straight after `enum` is a declaration and nothing else.
        if (defining === "enum" && !state.enums.includes(name)) {
          state.enums.push(name);
          // The `{` after this name opens a body rather than a block.
          state.awaitBody = true;
        }
        return "def";
      }
      // A member being declared. Ahead of the note, duration and builtin
      // readings for the same reason a member at a use site is: the name was
      // chosen by whoever wrote the enum, and `enum Section { verse, chorus }`
      // has no more to do with the `chorus` UGen than it does with a pitch.
      if (state.expectMember && atMemberDepth(state)) {
        state.expectMember = false;
        return "swyncEnumMember";
      }
      if (keywords.has(name)) {
        // Both introduce a name of their own, so both colour the word after
        // them as a definition rather than as whatever it would otherwise
        // look like — an enum called `Bell` is not the `bell` builtin.
        state.defining = name === "fn" || name === "enum" ? name : null;
        return "keyword";
      }
      // The two halves of `Scale.major`, told apart because they are read for
      // different things. The enum names where to look and is the same word on
      // every line that reaches into it; the member is which one, and is the
      // half a reader is actually scanning for. Colouring them alike made the
      // pair one shape to find and then gave no help inside it.
      //
      // Ahead of every other reading, and that ordering is the point — a member
      // may be spelled like anything at all. `Kit.bell` is not the `bell`
      // filter, and `Scale.e` is not the eighth note.
      if (member) return "swyncEnumMember";
      if (state.enums.includes(name)) {
        state.receiver = true;
        return "swyncEnum";
      }
      // Pitches and written values are skipped after a dot: a name there is
      // being called on what precedes it, and neither reading is reachable
      // through a call — see `afterDot`.
      if (!afterDot) {
        if (NOTE.test(name)) {
          // A spelled octave opens the register for the letters after it.
          if (state.octaves.length) state.octaves[state.octaves.length - 1] = true;
          return "swyncNote";
        }
        if (DURATION.test(name)) return "swyncDuration";
        // Only a pitch once something has said which octave — before that it is
        // an ordinary name, and colouring it otherwise would turn every
        // parameter called `f` pink.
        if (PITCH_CLASS.test(name) && state.octaves[state.octaves.length - 1]) {
          return "swyncNote";
        }
      }
      if (index.has(name)) return "swyncBuiltin";
      // A name in call position is a user function; anything else is a value.
      return stream.match(/^\s*\(/, false) ? "fnName" : "variable";
    }

    // A `[` opens an octave register inheriting the one around it, and `]`
    // closes it — see `TokenizerState.octaves`. Only the list brackets: the
    // others group expressions rather than steps.
    if (stream.match("[")) {
      state.octaves.push(state.octaves[state.octaves.length - 1] ?? false);
      state.groups++;
      return "bracket";
    }
    if (stream.match("]")) {
      state.octaves.pop();
      state.groups--;
      return "bracket";
    }
    // Braces are counted apart from the other two: they are what an enum body
    // is delimited by, and one opened inside a member's value has to be told
    // from the body's own.
    if (stream.match("{")) {
      state.braces++;
      if (state.awaitBody) {
        state.awaitBody = false;
        state.enumBody = state.braces;
        state.expectMember = true;
      }
      return "bracket";
    }
    if (stream.match("}")) {
      if (state.enumBody === state.braces) state.enumBody = null;
      state.braces--;
      return "bracket";
    }
    if (stream.match("(")) {
      state.groups++;
      return "bracket";
    }
    if (stream.match(")")) {
      state.groups--;
      return "bracket";
    }
    // Before the single-character rule, and before the operators: `::*` and
    // `::` are their own tokens in `lex.rs`, and `*` is not a multiplication
    // when it follows one.
    if (stream.match("::*") || stream.match("::")) return "punctuation";
    if (stream.match(",")) {
      // The other way members separate. At member depth by definition — a comma
      // inside a member's list or call is behind a `groups` the check counts.
      if (atMemberDepth(state)) state.expectMember = true;
      return "punctuation";
    }
    if (stream.match(/^[;:]/)) return "punctuation";

    for (const op of OPERATORS) {
      if (stream.match(op)) return "operator";
    }

    // The method dot, last because both longer readings are already gone: `..=`
    // was taken by the operator table just above and `.5` by the number rule at
    // the top. It reached the unknown-character rule below until enums needed
    // to know it had been written.
    if (stream.match(".")) {
      state.afterDot = true;
      // `receiver` is the identifier before this dot, so this is the moment
      // `Scale` and `.` become one thing and the next name is a member.
      state.member = receiver;
      return "punctuation";
    }

    // Unknown character: consume it so the tokenizer always advances.
    stream.next();
    return null;
  };
}
