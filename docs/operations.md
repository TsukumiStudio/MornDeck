# Operations

- Local iteration (since 2026-11-11): do not release each change. Build `cargo build --release` and restart the running server on it with `mdeck api '{"cmd":"restart","exe":"<repo>/target/release/mdeck"}'`; attached screens reconnect, tabs come back and claude / codex resume (work in progress stops, including an agent running inside MornDeck). Return to the installed version with `"exe":"/opt/homebrew/bin/mdeck"`. Release (bump `Cargo.toml`, tag `v<version>`, push, wait for the Release workflow) only when asked.
- Restart the local server (`kill` the `mdeck server` process) after upgrading. If a tab runs claude / codex, ask first: restarting ends it.
- Run `brew` with `HOMEBREW_NO_AUTOREMOVE=1`; plain `brew uninstall` removes unrelated formulae that only look unneeded. Add `HOMEBREW_NO_ASK=1` to `brew upgrade` so Homebrew 7+ does not wait for a y/N confirmation.
- The Release workflow signs and notarizes, publishes the Release, and updates the Cask in `tsukumistudio/tap`.
