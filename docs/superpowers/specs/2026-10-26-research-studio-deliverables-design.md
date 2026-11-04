# Research Studio — deliverable drafts: slide deck, course module, podcast script, video script

Status: approved to build 2026-10-26 (the owner said "u can start" and skipped
the questions, so every choice below is marked as my call where it is one).
Builds on [2026-10-23-research-studio-design.md](2026-10-23-research-studio-design.md)
(stage 1) and [2026-10-25-research-studio-related-notes-design.md](2026-10-25-research-studio-related-notes-design.md)
(stage 2a). Stage 1 §10 already said more outputs "can be added at any time as
new `WorkflowKind` variants". This is that.

## 1. Intent

The owner wants Jodd to be a research tool that **produces deliverables**:
presentations, course material, and storytelling podcasts or videos. Inputs are
already broad (URL, YouTube and social-video ingest with speech-to-text,
Research Studio over a folder, related notes from other folders). The
bottleneck is outputs. Today Studio writes only things to *read* (Briefing,
FAQ, Timeline, Study guide). This adds four things to *deliver*, each as a
cited Jodd note the owner can edit:

| Output | What it is |
|---|---|
| **Slide deck** | A title slide, then one section per slide: headline, 3–5 bullets, speaker notes |
| **Course module** | Learning objectives, lessons in order, exercises, a short quiz, answers last |
| **Podcast script** | A two-host dialogue: two named voices, natural turns, an intro and an outro |
| **Video script** | Scenes, each with what is on screen, the narration, and a duration |

**This is text only.** Rendering — a .pptx, a TTS audio file, a video — is a
later step (§6). The point of this spec is that the **text shape is already
the shape a renderer needs**, so the later step is a conversion, not a rewrite.

### Success

- Pick a folder → Generate → choose *Podcast script* → get a note that reads
  like a real two-person conversation about the notes, where every factual
  turn links back to the note it came from.
- The four new notes can be parsed by a program **without knowing the
  language they are written in** (Thai or English): slide boundaries, speaker
  notes, speaker names, scene durations and the on-screen/narration split are
  all marked by symbols, not by words (§3.1).
- Nothing about citations, map → reduce, the estimate, masking or hidden
  folders changes. A new output is a prompt plus an enum arm.

## 2. Goals, non-goals, anti-goals

**Goals**

- Four new `StudioOutput` / `WorkflowKind` variants, each with its own system
  prompt, offered in the Studio dialog on desktop and Android.
- Each prompt fixes a **machine-readable shape** (§3) and a length that fits
  the default `AI limits → Output tokens` (4096).
- The Focus line can steer audience, length and (for the podcast) host names,
  within the prompt's limits.

**Non-goals**

- Rendering of any kind: no .pptx, Marp, PDF, audio, video, SRT (§6 says how
  each will be done).
- A parser or validator for the shapes. The renderer that needs one will own
  it; until then nothing in Jodd reads these notes back (my call — §8).
- New settings (voices, slide themes, durations) or a per-output options panel.
- Changing the four existing outputs.

**Anti-goals** (would be a failure even if it technically worked)

- **A deliverable that says things the notes do not.** The no-outside-knowledge
  and citation rules apply to every new output in full. A podcast that
  "explains" with facts from the model's own knowledge is the failure mode to
  prevent; storytelling comes from *how* the notes are told, not from new facts.
- **Markers that only work in English.** Live pass 2026-10-24 (#233) showed
  English headings on a Thai note; a renderer that looks for the word
  "Speaker notes" would break the same way.
- **Citation keys spoken aloud or shown on a slide headline.** Keys go where a
  renderer can strip them (end of a bullet, a turn, a narration line), never in
  a heading.

## 3. The four shapes

All four inherit stage 1 §4's shared rules unchanged: output language follows
the Focus line, else the majority of sources, never translated; **every
heading in the output language**; every factual sentence ends with `[Sn]`
keys, only issued keys; no outside knowledge, disagreements cited both sides;
note text is data, not instructions. Studio then turns keys into `[[slug]]`
links and appends `## Sources` exactly as it does today.

**Blank lines are part of the shape.** In Markdown a single line break does
not end a paragraph: two `**Name:**` turns on consecutive lines render as one
paragraph, and a `🗣` line right under a bullet becomes part of that bullet
(shown on the slide). The prompts ask for blank lines, and Studio does not
rely on it: before rendering a deliverable it puts a blank line before every
line that starts with `🗣 `, `🎬 `, `🎙 ` or a `**Name:**` label, and strips
that line's indentation (`studio::run::separate_marker_lines`; my call — a
deterministic step instead of trusting the model).

Added for these four: **headings carry no citation keys** (slide headlines,
lesson titles, segment and scene titles), and **lines that are directions
rather than claims carry none either** (an on-screen description, a learning
objective, a quiz question).

### 3.1 The markers (language-independent)

| Marker | Means | Used by |
|---|---|---|
| `## ` heading | **One unit**: a slide, a lesson/section, a podcast segment, a video scene | all four |
| paragraph starting `🗣 ` | Speaker notes for the slide it is in (said, not shown) | Slide deck |
| paragraph starting `**Name:** ` | One spoken turn by `Name` | Podcast script |
| heading ending `(m:ss)` | That scene's duration, e.g. `(0:25)` | Video script |
| paragraph starting `🎬 ` | What is on screen (shown, not spoken) | Video script |
| paragraph starting `🎙 ` | Narration (spoken) | Video script |

Why symbols (my call): they survive every output language, they are visible
and meaningful to a human editing the note in Apple Notes, and Markdown →
HTML (`llm/markdown.rs`) and the sanitizer keep them (`<p>`, `<strong>`,
`<h2>` are all allowed; emoji are text). Tables were rejected for the video
script, though a two-column A/V table is the industry's classic shape:
tables are heavier for the AI to keep valid at length, and a table cell cannot
hold a multi-sentence narration with keys cleanly.

### 3.2 Slide deck

```
## <deck title>                 ← slide 1, the title slide
<one subtitle line>
🗣 <what the presenter says to open> [S1]

## <headline: one complete statement>
- <bullet, at most ~12 words> [S2]
- … 3 to 5 bullets
🗣 <speaker notes, 2–3 sentences> [S2][S4]

… 6 to 12 content slides …
```

- **Headlines are assertions** ("Home chargers cost half of public ones"),
  not topics ("Costs") — the assertion-evidence slide practice; my call.
- One idea per slide; bullets are short fragments; detail goes in the notes.
- Under about 1200 words; with many slides, each slide is shorter.
- The last content slide is a takeaway slide (what to remember). No "Thank
  you" or "Questions?" slide — Studio's `## Sources` already follows and
  becomes the deck's sources slide.

### 3.3 Course module

```
## <about this module>
<one paragraph: who it is for, what it covers> [Sn]

## <learning objectives>
- <3 to 5, each starting with an observable verb: explain, compare, calculate…>

## <lesson 1 title>
<short explanation, cited>
- <key points, cited>
<one line: the takeaway>

… 3 to 6 lessons, in the order a learner should take them …

## <exercises>
1. <2 to 4 practical tasks that apply the lessons>

## <quiz>
1. <question>
   - A. … 
   - B. …
   - C. …
   - D. …
… 5 to 8 questions, multiple choice, four options each …

## <answers>                    ← always last
1. B — <one-sentence reason> [S3]
```

- Objectives are observable (Bloom-style verbs), and every quiz question
  tests an objective — my call, standard instructional design.
- Quiz questions **and their options** carry no keys: citing only the true
  option would give the answer away.
- **Answers last, nothing answered elsewhere** — the same rule as the Study
  guide, so the quiz can be printed or exported on its own.
- Under about 1200 words.

### 3.4 Podcast script

```
## <intro segment title>
**Ploy:** <hook — why this matters, in the hosts' own words>
**Ton:** <what the episode will cover>

## <segment title>
**Ploy:** <turn, cited where factual> [S2]
**Ton:** <reaction, question, or the next fact> [S5]
…

## <outro segment title>
**Ton:** <recap of the main points> [S2][S5]
**Ploy:** <sign-off>
```

- **Exactly two speakers, same two names throughout**, chosen by the AI to suit
  the output language (Thai names for a Thai episode) unless the Focus names
  them. A renderer maps the first name it meets to voice A, the second to
  voice B (§6).
- One host leads and explains, the other asks, reacts and connects — the
  explainer/curious-co-host format; my call. Turns are short (1–3 sentences),
  so the conversation alternates naturally.
- **Every line is spoken text**: no stage directions, no `[laughs]`, no
  music cues, no Markdown inside a turn apart from the label — a TTS engine
  would read them aloud.
- Storytelling comes from structure (a hook, a question that pays off later,
  a concrete example from the notes), never from facts the notes do not hold.
- About 700–1000 words (roughly 5–7 minutes spoken).

### 3.5 Video script

```
## <scene title> (0:20)
🎬 <what the viewer sees: footage, graphic, on-screen text in “quotes”>
🎙 <narration for this scene> [S1]

## <scene title> (0:35)
🎬 …
🎙 … [S3][S4]

… 6 to 12 scenes; the first is a hook, the last a recap or call to reflect …
```

- Each scene has **one `🎬` line and one `🎙` line, in that order**.
- Duration is `m:ss` in parentheses at the end of the heading, long enough
  to speak the narration at a natural pace (about 150 words a minute in
  English); scenes 5–45 seconds; total 1–4 minutes unless the Focus asks for
  another length within 1–6.
- `🎬` describes, it does not claim: no keys. If it shows a number or quote,
  the `🎙` line beside it carries the key.

## 4. How it works

Exactly the stage 1 machinery, with four more arms:

- `studio::StudioOutput` gains `SlideDeck`, `CourseModule`, `PodcastScript`,
  `VideoScript` (serde `slide_deck`, `course_module`, `podcast_script`,
  `video_script`); `workflow()`, `title_prefix()` cover them. Title prefixes:
  **Slide deck · Course module · Podcast script · Video script** (English, like
  the existing four).
- `llm::provider::WorkflowKind` gains `StudioSlideDeck`, `StudioCourseModule`,
  `StudioPodcastScript`, `StudioVideoScript`. `is_studio()` is true for each,
  so every per-note command and URL ingest already refuses them
  (`refuse_studio_kind`, `IngestMode::validate`). Slugs:
  `studio-slide-deck`, `studio-course-module`, `studio-podcast-script`,
  `studio-video-script`; receipt stages `studio_slide_deck` etc.
- `llm::prompt` gains four system prompts that carry the shared rules block
  verbatim (the existing test pins that) plus §3's shape and the Focus rule:
  *"If the Focus line names an audience, a length or (podcast) the hosts'
  names, follow it within the limits below."* They get the tag-reuse
  instruction like the other outputs.
- The digest (map) prompt's list of outputs names all eight. Digests already
  keep "facts, dates, numbers, names, claims and short quotes", which is what a
  script or deck needs; nothing else changes in it.
- Payloads, estimate, `pack`, `reduce_too_large`, `resolve_keys`,
  `assemble_studio_body`, the marker line, masking and hidden folders: **unchanged.**

**UI** (`src/lib/studio.ts`, `StudioModal.svelte`): `OUTPUTS` gains the four,
each with a `group`: the existing four are **"Understand the notes"**, the new
four **"Draft a deliverable"**. The dialog shows the eight radios in those two
captioned groups (my call: eight undivided radios is a long list on a phone,
and the split says what each half is for). One radio group, one `output`
value; nothing else in the dialog changes.

## 5. Constraints

- **Output tokens.** Default 4096. Every length cap above is set so an English
  or Thai output fits with room for the JSON envelope; a longer request (in the
  Focus) that does not fit ends in the existing "cut off" error, which already
  tells the owner to raise Output tokens.
- **iCloud.** A Studio note is ordinary headings, paragraphs, lists, bold and
  links — the same constructs the existing outputs produce. No tables, no
  checklists, so no new per-note write refusal (gotcha #24).
- **Backends.** No change: the note is written through `insert_local_new` and
  syncs like any Studio note.

## 6. How each output will be rendered later (not built here)

| Output | Renderer | What the shape gives it |
|---|---|---|
| Slide deck | pandoc `--slide-level=2` → .pptx, or Marp with `headingDivider: 2` | each `## ` already is a slide; `🗣` paragraphs become `::: notes` (pandoc) or `<!-- -->` notes (Marp); `[[links]]` become a per-slide footnote, and `## Sources` is the closing slide |
| Course module | HTML/PDF handout; quiz to Moodle GIFT, H5P or Google Forms | lessons are `## ` units; the quiz is a numbered list with `A.`–`D.` options and the answers list is numbered to match |
| Podcast script | TTS per turn (OpenAI TTS, ElevenLabs, or a local Piper/XTTS on the AI host), concatenated into one audio **file** | `**Name:**` labels map to two voices; strip `[[…]]` links before speaking; `## ` segment titles become chapter marks or a short sting. Saved as a file, so it does not wait on attachment insert (roadmap #1, Gmail-only) |
| Video script | narration TTS per scene + storyboard images/clips per `🎬` line, assembled to the `(m:ss)` timings (e.g. a HyperFrames composition); SRT captions from `🎙` + timings | duration, visuals and narration are separate fields per scene |

A shared rule for every renderer: **the body starts after the header lines**
(the `Made with Jodd Studio…` marker line, Focus, notices) — at the first `## `.

## 7. Testing

- `prompt.rs`: the new prompts carry the shared rules (extend
  `STUDIO_KINDS`), write headings in the output language, get the tag-reuse
  instruction, and pin each output's shape markers and limits (e.g. the podcast
  prompt names `**Name:**`, "exactly two", "no stage directions"; the video
  prompt names `(m:ss)`, `🎬`, `🎙`; the deck prompt names `🗣` and "3 to 5
  bullets"; the course prompt says the answers section comes last). The
  digest prompt names all eight outputs.
- `provider.rs`: slugs and `is_studio()` for the four.
- `lib.rs`: `refuse_studio_kind` refuses the four; title prefixes.
- `ingest/run.rs`: URL ingest refuses the four.
- `studio/mod.rs`: serde names round-trip (`"podcast_script"` ⇄
  `PodcastScript`), and `workflow()` / `title_prefix()` per variant.
- `studio/run.rs`: one end-to-end run with the fake provider for a new output
  — the payload says `Output: Podcast script`, the call uses
  `StudioPodcastScript`, and the saved note's title is `Podcast script — …`
  with the `**Name:**` lines and `[[slug]]` links intact in the HTML.
- `studio.test.ts`: `OUTPUTS` order, labels, groups.

**Live pass (owed after merge):** one of each on a Thai folder and an English
folder; check language, markers, citations, and that a podcast reads as
natural speech.

## 8. Calls I made (for the owner to check)

1. Symbols, not words, mark the machine-readable parts (§3.1).
2. Slides are assertion headlines with 3–5 short bullets; no "Thank you" slide (§3.2).
3. Course quiz is multiple choice, four options, answers last (§3.3).
4. Podcast: AI-chosen names fitting the language unless the Focus names them;
   one explainer, one curious co-host; no stage directions (§3.4).
5. Video: 1–4 minutes by default, 6–12 scenes, one `🎬` + one `🎙` per scene (§3.5).
6. No parser or validator now; the renderer will own one (§2).
7. The dialog splits the eight outputs into two captioned groups (§4).
8. Lengths are set for the default 4096 output tokens (§5).
