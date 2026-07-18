---
name: docs-page
description: Write, update, or review Anima docs pages under docs/ in the house style. Use when a change adds or alters an engine concept (the AGENTS.md keep-docs-current rule), or when asked to "document X", "add a docs page", "update the docs", "explain X in the docs", or to review docs for style. Enforces timeless-present voice (no code-state/roadmap/change-journey language), paragraph budgets, mandatory examples, citations for named techniques, and Diátaxis role discipline — with a build + link-check + style-check verify loop.
---

# Writing Anima docs pages

The docs are a Hugo site (`docs/`, hugo-book theme, Diátaxis layout): a self-contained wiki where
every engine concept gets one page. AGENTS.md ("Keep current") makes this part of "done": a change
that adds or alters a concept updates the matching page and its hub row in the same change.

## Map

```
docs/content/
  _index.md        landing          overview.md   one-screen engine tour
  explanations/    the bulk — 20 subsystem subfolders, each with a hub _index.md
  how-to/          task recipes     reference/    terse lookup tables
  tutorials/       guided walkthroughs
```

Each subfolder hub `_index.md` holds a `| Page | Covers | Code |` table — the authoritative page
list for that subsystem. Build artifacts (`docs/public/`, `docs/resources/`) are gitignored.

## Product naming

The product and project are **Anima** in prose. **Saffron** names the wider family. Use Anima as the
subject when describing what the engine, editor, renderer, or project does.

Preserve exact literals and identifiers, including `saffron-*` crate, binary, and package names;
paths, URL schemes, environment variables, and configuration values; and quoted wire or process
output such as `ENGINE_NAME = "Saffron Anima"`. Never rewrite code, commands, paths, or output merely
to satisfy the prose name.

## The one law: timeless present

A page describes the system as it exists, the way an encyclopedia describes a river. The reader
has no access to — and no interest in — the repo's history, its plans, or its future. To the
docs, `plans/`, phases, coding sessions, commits, past implementations, and unbuilt features
**do not exist**.

This is also the anti-rot rule: every "not yet", "deferred", or "v1" is a claim about a moment
in time, and the docs outlive the moment. Status notes go stale silently and turn into lies.

**Banned outright** (the style checker errors on these):

- Status framing: *currently, not yet, for now, at the moment, as of writing, deferred*
  (roadmap sense), *planned, roadmap, follow-up, out of scope, non-goal, awaiting, waits on,
  will be added, remaining step/work, not implemented, still missing*.
- Version/stage framing: *v1, v2, Phase N, this phase, later step/stage/refinement/extension,
  endgame, on-ramp, reserved for X* (a future feature), *the seam left for*, *when it gains*,
  *once X lands*.
- Plan/session vocabulary: any reference to `plans/`, *the locked decision, scoped in,
  tracked in, landed, landed together, wired end to end, the first of the engine's*.
- Change-journey narration: *previously, used to, no longer, anymore, is now, now that,
  the old X, it replaced, replaces the old, is gone, historical, legacy, compatibility alias,
  already had, has been X the whole time*.

The technical senses stay, of course: *deferred rendering*, a capture *deferred to
end-of-frame*, *deferred structural ops* — the checker allowlists these; extend the allowlist
in `scripts/check_style.py` when a legitimate phrase trips it.

**Rewrites, not deletions with a hole.** Say what the system *is*; the past and future versions
of the sentence usually contain a present-tense fact worth keeping:

| Banned | Write instead |
|---|---|
| "Contact shadows are directional-only in v1" | "Contact shadows apply to the directional light." |
| "It was re-rendered every frame; the renderer now caches it" | "The renderer caches the cube map, keyed by a content hash." |
| "correctness-validated, awaiting real ray-tracing hardware" | `> [!NOTE]` "This path requires a ray-tracing-capable GPU and is feature-gated off without one." |
| "screen-space adaptive tessellation is tracked in `plans/displacement/`" | (delete — unbuilt features are not documented) |
| "the engine's endgame for material authoring" | "Materials are authored in a node graph." |
| "An occluded/dim-when-behind mode is deferred." | (delete) |

**Absences.** Document a missing capability only where a reader would otherwise hunt for it, and
state it as a design fact pointing at what to use instead — never as an apology or a promise:
"there's no `sa set-shader` (yet)" → "Shaders are selected by the material's PSO key; there is no
per-entity shader command." One `> [!NOTE]` maximum per page, present tense only.

**Hardware gating.** A feature that needs specific hardware states the *gate* ("requires
`VK_KHR_ray_query`; feature-gated off otherwise") — a permanent fact — never its validation
status or what it is waiting for.

## Voice: purely informative

- **Concept first.** Lead with the idea and why it exists, never "file X does Y" narration or
  quoted code comments. The "In the code" table carries the pointers.
- **No design-defense.** "This is not a duplicate path", "both stay", "rather than fake it" argue
  with an invisible reviewer. State what each path does; the reader never suspected a duplicate.
- **No milestones or marketing.** "The engine's first compute pipeline", "comes for free",
  "visually lossless", "far better than", "the live proof that" — delete or replace with the
  measurable fact ("adds no draw calls", "≈3× cheaper at half resolution").
- **No rhetorical tics.** "X carries weight", "the load-bearing decision", "crucially" — say the
  consequence plainly.

**Scrub the AI tells** (the humanizer pass, tuned for technical prose):

- Copulas: is/are/has. Never "serves as", "stands as", "acts as", "functions as".
- Em dashes: at most one per paragraph, never two in one sentence, never a parenthetical nested
  inside an em-dash aside. A colon, a comma, or a new sentence usually does the job.
- No rhetorical triads. Three items are fine when exactly three things exist; padding to three
  for rhythm ("no editor, no control plane, no toolchain") is filler.
- No negative-parallelism flourishes ("it's not just X — it's Y", "no A, no B"). A plain
  technical contrast ("returns the test result, not the depth") is fine; the ban is on rhythm,
  not disambiguation.
- Bold is for definition-list leads and real warnings. Never mid-sentence emphasis of ordinary
  words ("is **not** instanced").
- No trailing "-ing" analysis clauses ("…, ensuring/highlighting/reflecting …"). Give the
  consequence its own sentence with a subject.
- No aphorism punchlines ("the Chebyshev term is the leak-killer") and no AI-vocabulary filler:
  crucial(ly), pivotal, seamless, robust, testament, delve, leverage, "in order to", "it is
  important to note".
- Vary sentence length. A paragraph of same-shape sentences reads machine-made; so does a page
  where every paragraph is exactly three sentences.

## Readability budgets

- **One idea per paragraph. ≤ 4 sentences and ≤ 90 words per paragraph** (the checker warns
  past 90). If a paragraph explains two mechanisms, it is two paragraphs.
- **Sentences ~30 words.** A sentence needing three commas and a dash is a list — make it one.
- **At most one parenthetical per sentence, never nested.** Move the aside to its own sentence
  or a footnote-style follow-up.
- **Enumerations are bullets or tables**, not comma chains. Five flag names in a table cell is a
  sub-table or a linked reference row.
- **Hub "Covers" cells ≤ 12 words** — an index entry, not a summary paragraph.
- Define a term of art in one clause at first use ("froxel — a frustum-aligned voxel"), then use
  it freely.

## Every page teaches by example

- **Explanation** pages need at least one concrete artifact: a small code excerpt with real
  symbols, an `sa` invocation, a JSON payload, a worked number ("16×8×16 probes at 1.5 m"), a
  ` ```mermaid ` diagram, or `$$…$$` math. A page that is prose plus a symbols table is not done
  (the checker warns).
- **How-to** pages are numbered steps a competent user can execute blind: every step has a
  command or click-path, commands show expected output (`# draws=1 batches=1`), prerequisites at
  the top, a **Verify** section at the end. No mechanism explanations — link the explanation page.
- **Tutorials** guarantee success: one path, zero branching, every command with expected output,
  ends with something on screen. "Edit the engine somewhere" is not a step.
- **Reference** pages are complete lookup tables with terse notes. A completeness claim ("every
  command") is a contract — verify against the registering source (`register_builtin_commands`)
  in the same change, or drop the claim.

## Cite what you name

The wiki is self-contained: a reader must be able to follow every page from the page itself plus
its links. Well-known external sources are welcome — and **required** the moment you name one:

- A named technique, paper, or spec gets a link at first mention: DDGI → Majercik et al. (JCGT),
  ReSTIR → Bitterli et al., "Karis tonemap" → the UE4 course notes, GTAO → Jiménez et al.,
  RFC 8252 → the RFC, `VK_EXT_memory_budget` → the Khronos registry, "UE5's Persona" → the Epic
  docs, Jolt/Luau/winit/VMA → their project docs.
- The sentence still explains the concept in one clause — the link is for depth, not a
  prerequisite. Name-dropping without a link ("Lengyel's method", "Playdead's clip_aabb") is the
  failure mode; so is linking instead of explaining.
- Internal mentions of another engine concept link that page at first mention; every page ends
  with a Related list.

## Page template

```markdown
+++
title = 'Short noun phrase'   # MUST equal the body H1
weight = N                    # row position in the hub table
math = true                   # ONLY if the page uses $…$ / $$…$$
+++

# Short noun phrase

<1–2 sentences: the concept and why it exists — not what some file does>

## How it works
<concept-first prose within the budgets above; at least one concrete artifact>

## In the code
| What | File | Symbols |
|---|---|---|
| ... | `short_filename.ext` | `symbolA`, `symbolB` |

## Related
- [Sibling](../slug/) — one-line why
```

## Titles

- Short noun phrase, sentence case. **No leading "The/A/An"**, no `-ing` opener, no code or
  parentheses ("Main loop", not "The main loop and run()").
- How-to / tutorial titles are tasks: bare-infinitive verb ("Import a model").
- Front-matter `title` and body `# H1` stay identical — hugo-book does not render the title, so
  the H1 is required; a second H1 means a doubled heading (the checker errors on both).
- Retitling: change `title` + H1 only. **Never rename the file/slug or change `weight`** — links
  resolve by slug.

## Consistency across pages

- **One concept, one page.** Before creating a page, grep `docs/content` for the concept — a
  near-duplicate page (same H1, overlapping body) is a bug; merge instead.
- **A fact lives on one page**; other pages link it. When a fact changes (a default, a format
  version, a backend), grep `docs/content` for the old value and update *every* mention in the
  same change — sibling pages that contradict each other are worse than no page.
- Keep provenance facts intact when versions move (dynamic rendering is *1.3 core* regardless of
  what version the engine targets) — update target claims only.
- Hub tables, `explanations/_index.md`, and `overview.md` must agree with the leaf pages they
  index (subsystem lists, stack claims like the window backend or script VM).

## Workflow

1. **Ground in source first.** Read the implementing code before writing. Anchor on symbol
   names, never line numbers. If a hub row cites a symbol, confirm it exists.
2. **Write or update the leaf page.** New page: copy `docs/archetypes/explanation.md` or run
   `hugo new explanations/<sub>/<slug>.md --kind explanation`.
3. **Update the hub row** in the subfolder's `_index.md` (and `explanations/_index.md` +
   `overview.md` tables if a whole new subfolder appears).
4. **Verify** (below). Never ship without all three checks.

## Verify

```sh
cd docs && hugo --gc                                      # must exit 0, no ERROR
python3 <skill-dir>/scripts/check_links.py docs/public    # expect "BROKEN LINKS: none"
python3 <skill-dir>/scripts/check_style.py docs/content   # expect 0 errors; triage warnings
```

Run all three from the repo root (build first — the link checker reads `docs/public`). Hugo does
NOT validate plain markdown links, and nothing but `check_style.py` guards the voice rules. The
style checker's **errors** (banned language, title/H1 drift) block; its **warnings** (long
paragraphs, example-free pages) need a look — fix or consciously accept, never ignore silently.
If the page uses math or mermaid, load it in `just run-docs` and confirm both render.

## Commit

Docs-only commits use the `agent-commit --guide` format: `docs: <what>` (lowercase after colon,
< 72 chars), factual bullets, plain words, **no co-author or AI-attribution lines**.
