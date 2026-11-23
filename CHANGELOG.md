# Changelog

Releases use the section whose heading is the version number as their notes.

## 0.5.12

### Added

- "Sort tabs (A→Z)" in the settings puts the tabs in alphabetical order of their names, ignoring case.
- The bottom of the sidebar shows how to hide it (`F12:menu Shift+F12:hide`).

### Changed

- The sidebar shows the effort cut to three letters (`opus-5-5 med`).

### Fixed

- Codex tabs whose model was not saved (saved by an older version, or no turn yet) read it from the Codex transcript when the server restarts.
- The Windows check in the release waits until its local download server answers.

## 0.5.11

### Added

- `Shift+F12` hides the sidebar, leaving a one-line bar at the top (version, active tab, `Shift+F12: sidebar`). `Shift+F12` again, `F12`, or a click on the bar shows it. MornDeck remembers which way it was.

### Fixed

- A tab's `model` and `effort` survive a server restart. Codex tabs showed none until their next turn, and Claude Code tabs lost the effort; the Claude Code effort is now also read from the transcript.

## 0.5.10

### Added

- Tabs report the agent's `model` and `effort` (reasoning effort) in `list_tabs` and the other `tab` results, `null` when unknown. Claude Code: the effort from each hook, the model at the start and from the transcript at each turn end (so `/model` shows up after the next reply). Codex: from the transcript at each turn end. Copilot CLI: unknown.
- When the sidebar is wide enough, each tab row shows the model and effort in grey at its right end (`opus-5-5 xhigh`).
- Tabs can be reordered: drag a tab up or down the sidebar, or use "Move up" / "Move down" in the tab menu.

### Fixed

- `choose` on a Claude Code question with several parts: after each part but the last it now returns `waiting` at once with the next part's options (it used to wait until `timeout_ms` as `working`). After the last part MornDeck presses "Submit answers" and waits for the reply.

## 0.5.9

### Added

- A new version is shown in grey at the bottom of the sidebar. The server checks GitHub a minute after it starts and then once a day, and stays quiet when it is offline. Opening Config then offers "Install update" without checking again.

### Changed

- Windows: "Check for updates" with an `mdeck.exe` from the zip (not winget) now shows how to update by hand (download the new zip, close the MornDeck screens, replace `mdeck.exe`) instead of a failing `winget upgrade`.

## 0.5.8

### Changed

- Windows: the zip now holds only the signed `mdeck.exe` and the licenses. On first run (and after an update) `mdeck.exe` downloads the matching Linux build (`mdeck-<version>-wsl-<arch>`, now on each release) into WSL with `curl` and checks it against the SHA-256 built into `mdeck.exe` before installing it. If the download fails, the checksum does not match or `curl` is missing, it says what to do and exits non-zero.

## 0.5.7

### Changed

- Windows: `mdeck` with no arguments on a PC without WSL now prints the `wsl --install` guidance and exits 0 (setup is needed, nothing failed). Commands with arguments still exit non-zero.

## 0.5.6

### Fixed

- Selecting text by dragging right to left, or past the sidebar, missed the character in the terminal's first column (just right of the sidebar line).

## 0.5.5

### Added

- **Windows version.** `MornDeck-0.5.5-windows-x64.zip` and `-windows-arm64.zip` are on the release, with a signed `mdeck.exe` (Azure Artifact Signing). MornDeck runs inside WSL 2; `mdeck.exe` installs the matching Linux `mdeck` there and starts the server. `winget install TsukumiStudio.MornDeck` works once the package is accepted into winget.

## 0.5.4

### Added

- **Cmd+click opens links on macOS.** Terminals do not put Cmd in mouse reports, so MornDeck reads whether Cmd is held at the moment of the click from macOS (`CGEventSourceFlagsState`, no permission needed). Ctrl+click and Option+click still work (and are the way over SSH).
- Pointing at a link shows "Open link: Cmd+Click" (Ctrl+Click outside macOS) next to it, and an opened link flashes (blue and white swapped).

### Fixed

- A resumed codex tab still showed `blocked` while waiting for input when a numbered list was left in the conversation on screen; only a menu with a selected item counts now.

## 0.5.3

### Added

- **`server_started` webhook.** When the server starts (first start, `mdeck restart`, first start after an update), the global and per-owner webhooks get `{"event": "server_started", "reason": "first_start" | "updated" | "restart", "version", "previous_version", "owner", "resumed", "tabs"}`. A per-owner webhook gets only that owner's tabs.

### Changed

- **Links open with Ctrl+click or Option+click**, by MornDeck itself, in the default browser. In 0.5.2 they were handed to the terminal as OSC 8 links, which Warp did not open. Links are now drawn in a lighter blue (and underlined under the mouse). Cmd+click cannot be used: terminals do not pass it to the apps inside.

### Fixed

- A tab went `auth_required` again from an old "Login expired" line still on the screen, after the agent had logged in and replied. Only the lines near the bottom count now, and the tab goes back to idle once the error is gone from there.
- A resumed codex tab stayed `blocked` while waiting for input: the lines under its input box were read as options.

## 0.5.2

### Added

- **Clickable links.** URLs in a tab (plain `http://` / `https://` text, also across wrapped lines, and OSC 8 links the app prints) are passed to your terminal as OSC 8 hyperlinks, and the one under the mouse is underlined. Cmd+click opens it in terminals with OSC 8 (iTerm2, Ghostty, WezTerm, kitty, Warp, Windows Terminal); a plain click does not. Before, MornDeck took the mouse and drew plain text, so the terminal could not open them. "Clickable links" in Config (Appearance) turns it off, e.g. in macOS Terminal.app, which has no OSC 8.

## 0.5.1

### Fixed

- **A tab no longer stays `working` when the agent's login expires.** When claude stopped with "Login expired · Please run /login" (or another API error), it sent no `Stop` hook, so the tab stayed `working`, nothing was notified, and every request sent to it waited forever. The tab now gets the new state `auth_required`:
  - claude: from the new `StopFailure` hook (`error: authentication_failed` or `oauth_org_not_allowed`), and from the screen without a hook (`Login expired ·`, `Not logged in ·`, `Invalid API key ·` at the start of a line).
  - codex: from the screen (`Your access token could not be refreshed …`), since codex has no hook for it.
  - copilot: not detected yet.
- `auth_required` is sent once per tab to the webhooks (global and per owner) as `{"event": "state", "state": "auth_required", "tab", "error", "message"}`, and always shows a popup, whatever the popup setting. It is not sent again for the same tab until the agent finishes a reply. `list_tabs`, `mdeck tabs`, the sidebar (`!`) and the waiting calls (`send`, `get_answer`, ...) show it, with the error in `question`.
- Other claude API errors (rate limits and so on) now end the turn as `done`, with the error as the answer, instead of leaving the tab `working`.

### Changed

- The claude hooks are now v3 (with `StopFailure`). The server updates the installed hooks and the Claude Code plugin when it starts; running claude tabs pick them up after `mdeck restart` (or `/reload-plugins`).

## 0.5.0

### Added

- **Linux version.** Each release now has statically linked Linux binaries for x86_64 and aarch64 (`mdeck-<version>-linux-<arch>.tar.gz` with a `.sha256` file). See "Linux" in the README. It is tested in CI on Ubuntu (server, tabs, hooks, API, restarts); popups (through `notify-send`) and other distributions have not been checked on a real machine yet.
- On Linux the hooks run `mdeck hook-send` instead of `nc -U`, because some distributions ship `nc` without `-U`. The macOS hooks are unchanged.
- `mdeck running` prints the version of the running server and exits with 0, or exits with 1 when no server runs. Unlike `mdeck api`, it never starts a server.
- A Windows version (a launcher that runs the Linux version inside WSL, installed with winget) is being prepared and is not part of this release.

### Changed

- THIRD_PARTY_LICENSES now covers the crates of the Linux build and musl libc too.

## 0.4.8

### Fixed

- **Compatibility:** a codex permission prompt in the "Field 1/1" form (such as "Allow Computer Use to use …?") left the tab `working` with no `question`, and no `choices` webhook was sent, because codex sends no `PermissionRequest` hook for it. MornDeck now finds the prompt on the screen, sets the tab to `waiting`, and sends `choices` with `question` (`{"prompt": ..., "details": [...]}`) and `options`. The tab goes back to `working` when the prompt is gone. This covers fields with options; a field that asks for typed text is not detected.
- **Compatibility:** AutoYes did not answer these prompts because their options do not start with `Yes` / `No`. It now chooses `Allow` (this time only) when the options are `Allow` / `Allow for this session` / `Always allow` / `Cancel`; it never chooses the options that remember the permission. AutoYes now also treats `Cancel`, `Deny` and `Decline` as the declining option, so a menu such as `Yes` / `Cancel` is answered too.

## 0.4.7

### Changed

- The tab menu (`F12`, select a tab, `→`) is easier to read. Its heading names the tab ("Settings for docs"), names and values sit in two columns, and only the selected value shows `◀ ▶` (change it with `←` `→`). AutoYes "on" has the same color as the `»` mark; "off" and "none" are gray. A line under the items says what the selected item does, and the key hints match the selected row. "Delete tab" is now "Delete tab…" and "Close (F12)" is "Close".
- While a tab is selected in the sidebar, the row shows `⚙` (one cell away from the vertical line) and a "→ Menu" label appears right of the row and at the start of the bottom bar, so it is clear that `→` opens the menu. The bottom bar says "Enter show tab" there, and only "Menu  F12 close" while the menu is open. The word "Tab" no longer appears where it could be read as the Tab key.

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
