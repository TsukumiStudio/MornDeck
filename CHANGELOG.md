# Changelog

Releases use the section whose heading is the version number as their notes.

## 0.4.2

### Added

- Codex tabs report their state again. Codex runs hooks in a background server, so the tab's environment never reached them; the server now finds the tab from the session id and the working folder, and treats a Codex tab as ready once its input prompt has been on screen for a moment. The Codex hooks are updated to v3 when the server starts.
- `mdeck doctor` prints the client and server versions, the hook status and whether hooks reach the server.
- `mdeck mcp install` registers the MCP server with Claude Code, Codex CLI and GitHub Copilot CLI (`mdeck mcp uninstall` removes it; `mdeck uninstall` does too).
- `mdeck guide` prints `docs/agents.md`, a guide written for AI agents that control mdeck.
- `mdeck tabs`, `mdeck send <tab> <text>` and `mdeck wait <tab> [seconds]`.
- The tab a call comes from is protected: `list_tabs` marks it with `self: true`, and `send`, `press_keys`, `choose` and `close_tab` on it fail with the new error code `self_tab` unless `force` is true. `close_tab` by owner skips it and reports `skipped_self`.
- "Test sound and popup" and "Test webhooks" in the settings send a real test notification and show the result: whether the sound played, whether macOS blocks the popup and how to allow it, and the HTTP status of each webhook. Webhook tests are sent as `{"event": "test"}`.

### Changed

- `Enter` on an installed hook reinstalls it again (0.4.0 made it remove the hooks, which was easy to do by accident and left the sidebar without any state). Remove hooks with `x`.
- `answer` is null while the state is `waiting` or `blocked`; it used to hold the previous reply.
- `choose` confirms Codex menus that only move the cursor on a number key.
- Over MCP, `send` and `choose` wait 90 s by default (was 120 s) and `open_tab` 50 s (was 60 s), so the call returns before the client gives up.
- A server with no screen attached is 160x50 (was 80x24), so agents do not wrap their output as much.
- MCP tool descriptions explain every state, the `options` fields and `expect_seq`.

### Fixed

- A prompt you sent, wrapped over several lines, could come back as `options` on a `done` result.
- A Codex tab kept its state after Codex exited.

## 0.4.1

### Added

- `mdeck restart` restarts the server on the installed version, for example after `brew upgrade`. It also works against a 0.3.x server.

### Fixed

- After a server restart, a tab could forget its agent conversation and not resume it on the next restart. The hook that reports the old agent process ending could arrive after the resumed conversation started, and was taken as the end of that conversation.
- A resumed conversation is now remembered from the moment the server starts, not only once the agent reports in.

## 0.4.0

### Added

- `mdeck kill` stops the server and every tab. `mdeck uninstall` also removes the hooks.
- Hooks can be removed from the settings (`Enter` on an installed agent).
- "Usage gauge" in the settings turns the usage gauge off; while off, MornDeck reads no token and calls no usage API.
- `mdeck --licenses` prints the licenses of the bundled libraries. The same text ships as `THIRD_PARTY_LICENSES` in the app and the release.
- `mdeck --help`, `-h` and `-V`.
- `CLAUDE_CONFIG_DIR` and `CODEX_HOME` are honored for the hooks and the codex gauge.
- `add_webhook` returns a `warning` for an `http://` URL that is not on this Mac.
- English README.

### Changed

- The screen, the API error messages and the MCP tool descriptions are now in English.
- The `Enter` key on an installed hook now removes it instead of reinstalling it.
- A request that is not valid JSON gets `{"ok": false, "error": {"code": "invalid", ...}}` instead of a string `error`.
- An empty `tab` argument is `invalid`; it used to match the first unnamed tab.
- External commands (`curl`, `security` and others) run by absolute path, and `curl` ignores `~/.curlrc`.
- The claude gauge is read only when the claude config folder exists.

### Fixed

- Installing hooks replaced a symlinked `settings.json` with a regular file and could widen its permissions. The link and the permissions are now kept, and the original content is saved once as `.mdeck-bak`.
- `events.log` recorded webhook URLs in full. They are now logged only up to the host, and curl receives the URL through a private file instead of its arguments.
- A very small terminal made the screen panic in a loop.
- The codex gauge could not read a session log whose last 512 KB started in the middle of a character.
- The usage gauge fill crossed the "now" line while under pace.
- Server panics are now recorded in `events.log`.
- A server started from an ssh session could not restart itself after an update.

## 0.3.3

- Hooks from another conversation inside a tab no longer change the tab's state or reply.
- `/clear` no longer counts as the end of the session.
- Log lines written at the same time no longer mix.

Earlier versions have no notes; see the commit history.
