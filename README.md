# dogma

A terminal UI for [Nerve](https://github.com/ClickHouse/nerve) — drive your agent sessions from the comfort of a TUI.

![dogma — a terminal UI for Nerve](https://grigorypervakov.github.io/dogma/dogma.png)

<sub>Rendered with fake, anonymized data via `cargo run --example screenshot` (the same `TestBackend` the snapshot tests use). The image is rebuilt from the real UI on every push by the [screenshot workflow](.github/workflows/screenshot.yml) and published to GitHub Pages — it is never committed to the repo.</sub>

## Features

- **Keyboard-driven navigation** — tab bar, `:` command palette with autocomplete (singular/plural aliases), `?` for help.
- **Live chat** — token streaming, markdown with syntax-highlighted code, collapsible tool/thinking blocks, full-screen block zoom, sub-agent side panel.
- **Interactive** — answer `AskUserQuestion` polls inline, watch a live task / background-jobs panel, see the chat reframe while the agent streams.
- **Tabs** — chat, notifications (answer & dismiss, red alert when pending), tasks, plans, skills. Lists show active items first, sorted by time; press `a` to reveal answered/done/declined ones.
- **Multiple instances** — drive several Nerve servers from one UI. Their sessions, notifications, tasks, plans and skills merge into single time-sorted lists, each row tagged by a colored sigil + name (`● local` / `◆ vm`). Starting a new chat asks which instance it lands on; an unreachable instance shows offline and reconnects on its own.
- **Survives long sessions** — the password is prompted once (or read from config) and kept in memory to silently reissue the auth token when it expires (~24h). Tokens are never written to disk.

## Build & run

```sh
cargo build --release
./target/release/dogma                       # http://127.0.0.1:8900 by default
./target/release/dogma --server vm=http://my-dev-vm:8900            # one named instance
./target/release/dogma --server lh=http://127.0.0.1:8900 \
                       --server vm=http://my-dev-vm:8900            # several at once
./target/release/dogma --help                 # other options
```

Instances can also be declared in `~/.dogma/config.toml` (a repeated `--server` flag overrides it):

```toml
[[servers]]
name = "lh"
url  = "http://127.0.0.1:8900"
# password = "…"   # optional — prompted if omitted; keep the file chmod 600

[[servers]]
name = "vm"
url  = "http://my-dev-vm:8900"
```

Requires a running Nerve API server. Press `?` inside the app for the full keymap.

## ⚠️ This code was written by an AI and never reviewed by a human

Every line of dogma — source, tests, and this README — was written by an AI agent
(Claude), end to end, inside a terminal session. **No human has read, audited, or
reviewed any of it.**

It compiles, the test suite passes, and `clippy` is clean — but that is the *only*
quality bar this code has ever cleared. There has been no human design review, no
security review, no second pair of eyes.

Treat it accordingly: read the source yourself before you trust it, point it only
at a local Nerve instance you control, and don't run it against anything you can't
afford to lose. It's an experiment in what an agent can build unsupervised — enjoy
it as one.

## License

MIT — see [LICENSE](LICENSE).
