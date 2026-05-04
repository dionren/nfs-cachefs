# Agent instructions

**Read [`CLAUDE.md`](CLAUDE.md) first.** It is the canonical agent guide
for this repo and contains everything you need: hard kernel/protocol
constraints, repo layout, build/run, performance baseline, and the
"don't do this" list.

This file (`AGENTS.md`) exists only because tools like Codex / Cursor /
Aider / OpenAI o-series agents look for `AGENTS.md` by convention while
Claude Code looks for `CLAUDE.md`. Rather than maintain two copies that
drift apart, the project keeps a single source of truth in `CLAUDE.md`
and points here.

After reading `CLAUDE.md`, also skim:

- `README.md` — user-facing install / configure / run
- `docs/architecture.md` — protocol, cull algorithm, cache sizing,
  performance baseline, design rationale

Do **not** duplicate `CLAUDE.md` content into this file. If you need to
update agent guidance, update `CLAUDE.md` and leave this pointer alone.
