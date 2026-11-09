# Operations

- After committing a change, release without asking: bump `Cargo.toml`, tag `v<version>`, push, wait for the Release workflow, then `brew upgrade --cask morndeck` on the local Mac.
- Restart the local server (`kill` the `mdeck server` process) after upgrading. If a tab runs claude / codex, ask first: restarting ends it.
- Run `brew` with `HOMEBREW_NO_AUTOREMOVE=1`; plain `brew uninstall` removes unrelated formulae that only look unneeded.
- The Release workflow signs and notarizes, publishes the Release, and updates the Cask in `tsukumistudio/tap`.
