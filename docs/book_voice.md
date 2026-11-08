# The Book of Pith: voice and style guide

This guide is for anyone, human or agent, writing chapters of *The Book of
Pith*. The house style follows No Starch Press technical books, specifically
*Network Programming with Go* and *Rust for Rustaceans*. The reference chapters
are the Introduction, Chapter 8 (errors) and Chapter 14 (memory) in the book
draft. When this guide and those chapters disagree, the chapters win.

## The voice in one paragraph

A senior engineer walks a capable colleague through real code. They are calm,
concrete and honest. They explain why before how, show the program running,
break it on purpose, and admit what isn't finished. They never sell, never
gush, and never talk down. The reader should finish each section able to do
something they couldn't do before, and understand why it works.

## Principles

1. **Show it running.** Every idea comes with a complete program, its exact
   output, and an explanation of that output. Never describe behavior you
   haven't shown.
2. **Every output is real.** Compile and run every listing with the current
   toolchain, and paste what it printed, error codes included. Never invent
   output, never tidy an error message, never write "output similar to".
   If a listing can't run, say so in the text.
3. **Break things on purpose.** Each chapter shows at least one real mistake,
   such as a compile error, a leak or a crash, then explains it and fixes it.
   Use the compiler's actual diagnostic.
4. **Explain why.** Each feature gets the reason it exists, the problem it
   solves, and the trade-off it makes. A reader should be able to defend the
   design, not just use it.
5. **Be honest about limits.** If something is missing, awkward or surprising
   in the current compiler, say so plainly, using "at the time of writing".
   Examples: runtime errors have no stack trace; locals are released at
   function return, not at block end; E306 still warns after a `weak` fix.
   Don't hide quirks, and don't apologize for them either.
6. **Calm, not cute.** At most one light aside per section, preferably in
   parentheses. No exclamation marks in prose. No "awesome", "magic", "simply",
   "just", "easy" or "obviously".

## Person and tone

- **"We"** when author and reader explore together: "Next, we'll look at..."
- **"You"** for what the reader does or can now do: "You'll need these tools
  for every server in Part III."
- **"I"** only in the Introduction, for the author's own reasons.
- Never "the user" for the reader, and never "one".
- Assume a working programmer who knows one language, probably Python. Define
  any term the first time it appears, in italics: "a *reference count*: the
  number of places currently holding it."
- Use one analogy for a hard idea, state it once, and move on. Example: the
  last person to leave a meeting room turns off the lights.

## Chapter structure

Every chapter follows this order:

1. **Roadmap opener (one paragraph).** Start with a sentence about the problem,
   then lay out the chapter in order: "We'll start with... Next, we'll look
   at... Then... Finally, we'll cover..."
2. **Why it matters (one paragraph).** A concrete server situation the chapter
   prepares the reader for, such as an account-signup endpoint, a server that
   runs for weeks, or a p99 latency spike. Use numbers where you can.
3. **The verification NOTE.** Use this wording: "Every listing in this chapter
   was compiled and run with the Pith toolchain built from the repository's
   main branch. The output shown is what it printed."
4. **Sections (H3), each built around one or two listings.** Section titles
   are descriptive noun phrases or gerunds in title case: "Counting
   References", "Breaking Cycles with weak", "When to Return a Result and
   When to Crash". No questions or puns, except in the chapter title itself.
5. **What You've Learned.** One paragraph in past tense that walks the chapter
   in order: "In this chapter, you first learned... You then saw... Next,
   you... Finally, you...". End with one sentence pointing to the next
   chapter.

**There are no exercises, no "Try It Yourself", and no "Key Takeaways".**

## Listings

Each listing follows a fixed sequence:

1. **Introduce it first.** The prose before the code says what the listing
   does: "Listing 14-2 shows both cases."
2. **The code.** A complete, runnable program in a `pith` code block. A
   partial listing must say what it leaves out, in its caption or in a
   comment such as `# Node and make_family are the same as in Listing 14-3`.
3. **The caption, in italics, immediately after the code:**
   `*Listing 14-3: A parent and child that keep each other alive (family.pith)*`.
   Number listings by chapter (8-1, 8-2, and so on); the Introduction uses I-1.
   Captions are sentence case and include the file name.
4. **The command and its output** in a plain code block, with the shell
   prompt: `$ pith run family.pith`.
5. **The explanation**, referring to callouts.

Use *Table N-M: caption* the same way for tables.

### Callouts

- Mark lines with ❶ ❷ ❸ ❹ ❺, aligned in a column to the right of the code,
  separated by spaces. Don't make the markers `#` comments.
- In prose, put the callout right after the phrase it explains: "Reassigning
  `b` ❷ drops the only reference to the first buffer." Never "At ❷, ...".
- Explain callouts in order, with no more than five per listing. Split long
  listings instead.

## Asides

- **NOTE** for useful side facts, written as a blockquote starting
  `> NOTE:`. One or two per chapter beyond the verification note.
- **WARNING** only for mistakes that lose data, corrupt state or crash, written
  `> WARNING:`. At most one per chapter.
- Asides are two to four sentences each and never contain listings.

## Sentences and formatting

- Paragraphs are usually two to four sentences. Most sentences are under 25
  words.
- Write in plain prose. Use bulleted lists only for parallel items such as a
  list of limitations. Never write a section as a list of fragments.
- Put code identifiers, commands and file names in backticks in prose. Long
  file paths go in italics: *docs/limitations.md*.
- Use the em dash sparingly.
- Cross-reference by chapter number: "Chapter 18 uses exactly this pattern."
- Write numbers with units and thousands separators: "1,105 ms", "2.6MB",
  "20 million".

## Accuracy

- Every claim about Pith must come from the repository: the compiler, *docs/*,
  the README, or a listing you ran. When unsure, run a probe program.
- Benchmark numbers need their date, machine and caveats (from *bench/README.md*).
- Mark anything you couldn't verify, or that needs the author's input, as an
  open item in brackets: "[Author's note: ...]" or "[Open question: ...]".
  Never fill those gaps with invented facts.
- Never present a quirk as the intended design without checking. When unsure,
  describe what happens and leave a note for the author.

## How to verify a chapter

1. Write each listing to a scratch file, using the file name from its caption.
2. From the repository root, run it with `./target/release/pith run <file>`,
   `pith test` for `test` blocks, or `pith lint` (which needs
   `make self-host` first). Commands must run from the repository root, or
   the IR driver isn't found.
3. Paste the output exactly, replacing absolute paths with the bare file name.
4. Make sure every callout number in the prose matches its marker in the code.

## Checklist before handing off a chapter

- [ ] Roadmap opener, why-it-matters paragraph, verification NOTE
- [ ] Every listing introduced, captioned with a file name, run, and its
      output pasted
- [ ] At least one deliberate mistake, showing the real diagnostic
- [ ] Callouts placed after the phrase, in order, no more than five per listing
- [ ] At least one "at the time of writing" limitation, if one exists
- [ ] No exercises, no exclamation marks, no "simply", "just" or "easy"
- [ ] Ends with "What You've Learned" and a pointer to the next chapter
- [ ] Open items marked in brackets instead of guessed
