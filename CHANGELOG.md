# Changelog

Releases use the section whose heading is the version number as their notes.

## 0.4.6

### Fixed

- The highlight of the selected tab (and of the cursor and hover rows) in the sidebar crossed the vertical line between the sidebar and the terminal. It now stops before the line.

## 0.4.5

### Changed

- **Compatibility:** the claude hooks are installed as a Claude Code plugin by default (`Enter` on the Claude Code row in the settings, or the new `mdeck hooks install`). Claude Code records the plugin in `settings.json` (`enabledPlugins`, `extraKnownMarketplaces`). If the plugin cannot be installed, mdeck writes the hooks to `settings.json` instead, shows why, and `mdeck doctor` repeats the reason.
- **Compatibility:** claude hooks already in `settings.json` move to the plugin the first time the 0.4.5 server starts (it runs `claude plugin` and may take a few seconds). They are removed from `settings.json` only after the plugin is enabled; if the plugin cannot be installed they stay, and the server does not retry on every start. To keep `settings.json`, run `mdeck hooks settings` after upgrading and before `mdeck restart`. The settings screen and `mdeck doctor` now say "installed in settings.json" or "installed as plugin".
- `mdeck hooks settings` (or `s` on the Claude Code row) keeps using `settings.json`; mdeck remembers that choice. `mdeck hooks plugin` (or `p`) switches back.
- `mdeck doctor` gives advice that fits the case when no hook has arrived: a policy that blocks hooks, hooks that are missing, or agents that need a restart.
- A plugin install or removal that does not finish in 30 seconds is stopped. mdeck runs the `claude` command only from PATH or the usual install folders, never from the current folder.
- **Compatibility:** webhooks are tested one at a time. `test_webhooks` now requires `url` and sends only to that webhook; a call without `url` fails with `invalid` instead of sending to every webhook. In the settings, a selected webhook row shows "Edit", "Test" and "Delete"; the "Test webhooks" row that sent to every webhook is gone. Global webhooks (registered without `owner`) are not listed in the settings; test them with `test_webhooks`.
- Tabs with AutoYes on show `»` between the state and the name in the sidebar.
- The "Popup when done" notification now comes from MornDeck itself instead of Script Editor, so clicking it opens nothing. The first popup asks for macOS permission to show MornDeck notifications; manage it in System Settings → Notifications → MornDeck. After upgrading, run "Test sound and popup" once to answer that question. If MornDeck cannot show it (a build from source, or not allowed), mdeck shows it through Script Editor as before, and the test says why.

## 0.4.4

### Added

- `mdeck hooks plugin` (or `p` on the Claude Code row in the settings) installs the claude hooks as a Claude Code plugin from a local marketplace in `~/.mdeck/claude-plugin`, and removes them from `settings.json`. Use it when an organization policy (`strictPluginOnlyCustomization`) makes Claude Code skip hooks in `settings.json`. `mdeck hooks settings` switches back; `x` on the row and `mdeck uninstall` remove the plugin too. Running claude sessions load the plugin at their next start (or `/reload-plugins`). The default is unchanged.
- The settings screen and `mdeck doctor` detect organization settings that stop Claude Code hooks (`strictPluginOnlyCustomization`, `allowManagedHooksOnly`, `disableAllHooks`) and say whether the plugin helps.
- A tab where claude or copilot has been running for 20 seconds without any hook arriving shows `no hooks` in the sidebar, and `mdeck doctor` lists it. Codex tabs are left out because Codex reports only after the first instruction.
- When `open_tab` or a wait times out on such a tab, the result has a `hint` that names the cause when it is known.

### Fixed

- If the claude hooks are installed both in `settings.json` and as the plugin, each event is counted once: the hook says which source it came from, and the server drops a copy of the same event from the other source that arrives within 5 seconds. Repeated events from the same source still count.

## 0.4.3

### Changed

- **Compatibility:** `send` to a tab whose agent is asking (state `waiting`) is refused with the new error code `tab_waiting`, which carries the `question` and `options`. It used to type the text and Enter into the prompt, which could confirm the selected option. Answer with `choose`, or pass `force: true` (`mdeck send --force`) to type into the prompt anyway.

## 0.4.2

### Added

- Codex tabs report their state again. Codex runs hooks in a background server, so the tab's environment never reached them; the server now finds the tab from the session id and the working folder, and treats a Codex tab as ready once its input prompt has been on screen for a moment. The Codex hooks are updated to v3 when the server starts; Codex then asks you to review the changed hooks, and skips them until you trust them (`/hooks`). The v3 hook reports every Codex session on the Mac to the local server, which ignores the ones that are not in a tab.
- `mdeck doctor` prints the client and server versions, the hook status and whether hooks reach the server.
- `mdeck mcp install` registers the MCP server with Claude Code, Codex CLI and GitHub Copilot CLI (`mdeck mcp uninstall` removes it; `mdeck uninstall` does too).
- `mdeck guide` prints `docs/agent-guide.md`, a guide written for AI agents that control mdeck.
- `mdeck tabs`, `mdeck send <tab> <text>` and `mdeck wait <tab> [seconds]`.
- The tab a call comes from is protected: `list_tabs` marks it with `self: true`, and `send`, `press_keys`, `choose` and `close_tab` on it fail with the new error code `self_tab` unless `force` is true. `close_tab` by owner skips it and reports `skipped_self`.
- "Test sound and popup" and "Test webhooks" in the settings send a real test notification and show the result: whether the sound played, whether macOS blocks the popup and how to allow it, and the HTTP status of each webhook. Webhook tests are sent as `{"event": "test"}`.

### Changed

- `Enter` on an installed hook reinstalls it again (0.4.0 made it remove the hooks, which was easy to do by accident and left the sidebar without any state). Remove hooks with `x`.
- `answer` is null while the state is `waiting` or `blocked`; it used to hold the previous reply.
- `choose` confirms Codex menus that only move the cursor on a number key. AutoYes does the same, so it now answers Codex Yes / No menus it used to leave open.
- Over MCP, `send` and `choose` wait 90 s by default (was 120 s) and `open_tab` 50 s (was 60 s), so the call returns before the client gives up.
- Until the first screen attaches, a new server is 160x50 (was 80x24), so agents do not wrap their output as much.
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
