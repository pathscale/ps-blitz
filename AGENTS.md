# Working agreement — ps-blitz

The operating contract for **any** coding agent working in this repository. Codex, Cursor
and Gemini CLI read `AGENTS.md` natively; Claude Code loads it through the `@AGENTS.md`
import in [`CLAUDE.md`](CLAUDE.md). Never fork these rules into a per-vendor file.

**Rust** rendering stack. This is a fork of [DioxusLabs/blitz](https://github.com/DioxusLabs/blitz)
and tracks an `upstream` remote, so it carries code we did not write and will rebase onto
code we do not control.

## Invariants (do not break these)

- **No Python in our code.** Not a script, not `python3 -c`, not a heredoc. Reaching for
  it is the tell that a step is being solved by parsing when the tool that owns the answer
  could just be asked. Do not swap it for another parser either, and do not assume `jq` is
  present: it does not ship with macOS. A fixed-shape field is one `sed -nE` line; anything
  needing real parsing belongs in Rust, where it can be tested.

- **`.github/scripts/wpt_diff_to_pr.py` is upstream's, and stays.** It arrived with the
  fork and is maintained by DioxusLabs. Deleting or rewriting it buys nothing here and
  conflicts on every rebase. Leave it alone; the rule above governs code we add.
