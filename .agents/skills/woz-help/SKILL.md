---
name: woz-help
description: Ask which skill or flow fits your situation. A router over the skills in this repo.
disable-model-invocation: true
---

# Ask Matt

You don't remember every skill, so ask.

A **flow** is a path through the skills. Most paths run along one **main flow**, and two **on-ramps** merge onto it. Everything else is standalone, or a vocabulary layer that runs underneath.

## The main flow: idea → ship

The route most work travels. You have an idea and want it built.

1. **`/woz-grill-with-docs`** — sharpen the idea by interview. Start here when you **have a codebase**: it's stateful, retaining what it learns in `CONTEXT.md` and ADRs. (No codebase? Use `/woz-grill-me` — see Standalone. Both run the same `/woz-grilling` primitive; `grill-with-docs` is the one that leaves a paper trail.)
2. **Branch — can you settle every question in conversation?** If a question needs a runnable answer (state, business logic, a UI you have to see), detour through a prototype, bridged by **`/woz-handoff`** in both directions (see Crossing sessions):
   - **`/woz-handoff`** out, then open a fresh session against that file,
   - **`/woz-prototype`** to answer the question with throwaway code,
   - **`/woz-handoff`** back what you learned, and reference it from the original idea thread.
3. **Branch — is this a multi-session build?**
   - **Yes** → **`/woz-to-spec`** (turn the thread into a spec), then **`/woz-to-tickets`** to split it into tracer-bullet tickets, each declaring its **blocking edges**. On a local tracker that's one file per ticket under `.scratch/<feature>/issues/`, worked blockers-first by hand; on a real tracker the edges become native blocking links, so any ticket whose blockers are done can be grabbed — kick off **`/woz-implement`** per ticket, **clearing context between each one**.
   - **No** → **`/woz-implement`** right here, in the same context window.

   Either way, **`/woz-implement`** builds each issue by driving **`/woz-tdd`** internally — one red-green slice at a time — then closes out by running **`/woz-code-review`**, a two-axis review (Standards + Spec) of the diff, before committing. Reach for **`/woz-tdd`** on its own when you just want to build a concrete behaviour test-first without a full spec, and **`/woz-code-review`** on its own whenever you want to review a branch or PR against a fixed point.

### Context hygiene

Keep steps 1–3 in **one unbroken context window** — don't compact or clear until after `/woz-to-tickets` — so the grilling, spec, and tickets all build on the same thinking. Each `/woz-implement` then starts fresh, working from the ticket.

The limit on this is the **[smart zone](https://www.aihero.dev/ai-coding-dictionary/smart-zone)**: the window (~120k tokens on state-of-the-art models) within which the model still reasons sharply. If a session approaches it before `/woz-to-tickets`, don't push on degraded — `/woz-handoff` and continue in a fresh thread.

## On-ramps

A starting situation that generates work, then merges onto the main flow.

- **Bugs and requests piling up** → **`/woz-triage`**. It moves issues through triage roles and produces agent-ready issues, which **`/woz-implement`** later picks up.

  Triage is only for issues **you didn't create** — bug reports, incoming feature requests, anything that arrives raw. Tickets that `/woz-to-tickets` produced are already agent-ready, so **don't triage them**.

- **Something's broken** → **`/woz-diagnosing-bugs`**. For the hard ones: the bug that resists a first glance, the intermittent flake, the regression that crept in between two known-good states. It refuses to theorise until it has a **tight feedback loop** — one command that already goes red on *this* bug — then fixes with a regression test. Its post-mortem hands off to **`/woz-improve-codebase-architecture`** when the real finding is that there's no good seam to lock the bug down.

- **A huge, foggy effort — a greenfield project or a huge feature build, too big for one session** → **`/woz-wayfinder`**, the most cognitively demanding flow here. When the way from here to the destination isn't visible yet, it charts a **shared map** of **decision tickets** on the issue tracker and resolves them one at a time — producing **decisions, not deliverables** — until the fog is pushed back and the way is clear. Where **`/woz-grill-with-docs`** sharpens an idea you can hold in one session, wayfinder is for the idea you can't — and it's slower and denser, so save it for exactly that, never a well-scoped feature.

  When the map clears, **it hands off, it doesn't build**: merge onto the main flow at **`/woz-to-spec`**, which collapses the map's linked decisions into a buildable plan, then `/woz-to-tickets` and `/woz-implement` as usual. Looping the map straight into `/woz-implement` skips that collapse and throws the linked detail away — go straight to `/woz-implement` only when the effort turned out genuinely small.

## Codebase health

Not feature work — upkeep.

- **`/woz-improve-codebase-architecture`** — run whenever you have a spare moment to keep the codebase good for agents to operate in. It surfaces **deepening opportunities**; picking one _generates an idea_ you can take into the main flow at `/woz-grill-with-docs`. It's the survey that finds the candidates; **`/woz-codebase-design`** (below) is the bench you design the chosen one on.

## Vocabulary underneath

Two model-invoked references that run *beneath* the other skills — each the single source of truth for its vocabulary. Reach for them directly when the **words**, not the process, are the problem; or let the skills above pull them in.

- **`/woz-domain-modeling`** — sharpen the project's *domain* language: challenge a fuzzy term, resolve an overloaded word ("account" doing three jobs), record a hard-to-reverse decision as an ADR. It's the active discipline `/woz-grill-with-docs` drives to keep `CONTEXT.md` a clean glossary.
- **`/woz-codebase-design`** — the deep-module vocabulary (module, interface, depth, seam, adapter, leverage, locality) for designing a module's *shape*: a lot of behaviour behind a small interface at a clean seam. `/woz-tdd` and `/woz-improve-codebase-architecture` both speak it.

## Crossing sessions

- **`/woz-handoff`** — when a thread is full or you need to branch off (e.g. into a `/woz-prototype` session), this compacts the conversation into a markdown file. You don't continue in place — you **open a new session and reference that file** to carry the context across. It's the bridge between context windows, in either direction. Use it when you want a **fresh session** but need the **current conversation preserved**.
- **`/compact`** (built-in) — stay in the **same conversation**, letting the earlier turns be summarized. Use it at **intentional breaks between phases**, when you don't mind losing the verbatim history. Don't compact mid-phase — the agent can lose its way. `/woz-handoff` forks; `/compact` continues.

## Standalone

Off the main flow entirely.

- **`/woz-grill-me`** — the same relentless interview as `/woz-grill-with-docs`, but for when you have **no codebase**. Stateless: it saves nothing locally, builds no `CONTEXT.md`. Reach for it to sharpen any plan or design that doesn't live in a repo.
- **`/woz-prototype`** — a small, throwaway program that answers one design question: does this state model feel right, or what should this UI look like. Throwaway from day one — keep the answer, delete the code. It's the detour in step 2 of the main flow, but reach for it any time a design question is hard to settle on paper.
- **`/woz-research`** — delegate reading legwork to a **background agent**: it investigates a question against **primary sources**, then leaves a cited Markdown file in the repo. Keep working while it reads. The file it produces is something to take *into* the main flow at `/woz-grill-with-docs` — research feeds the thinking, it doesn't replace it.
- **`/woz-teach`** — learn a concept over multiple sessions, using the current directory as a stateful workspace.
- **`/woz-writing-great-skills`** — reference for writing and editing skills well.

## Precondition

**`/woz-setup-project`** — run before your first engineering flow to configure the issue tracker, triage labels, and doc layout the other skills assume. Custom issue trackers also work.