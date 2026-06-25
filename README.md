# dogma

A terminal UI for [Nerve](https://github.com/ClickHouse/nerve) — drive your agent sessions from the comfort of a TUI.

![dogma — a terminal UI for Nerve](https://grigorypervakov.github.io/dogma/dogma.png)

<sub>Rendered with fake, anonymized data via `cargo run --example screenshot` (the same `TestBackend` the snapshot tests use). The image is rebuilt from the real UI on every push by the [screenshot workflow](.github/workflows/screenshot.yml) and published to GitHub Pages — it is never committed to the repo.</sub>

## Features

- **Keyboard-driven navigation** — tab bar, `:` command palette with autocomplete (singular/plural aliases), `?` for help.
- **Live chat** — token streaming, markdown with syntax-highlighted code, collapsible tool/thinking blocks, full-screen block zoom, sub-agent side panel.
- **Interactive** — answer `AskUserQuestion` polls inline, watch a live task / background-jobs panel, see the chat reframe while the agent streams.
- **Tabs** — chat, notifications (answer & dismiss, red alert when pending), tasks, plans, skills.
- **Survives long sessions** — the password is prompted once and kept in memory to silently reissue the auth token when it expires (~24h). Nothing is written to disk.

## Build & run

```sh
cargo build --release
./target/release/dogma            # connects to http://127.0.0.1:8900 by default
./target/release/dogma --help     # other options
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
