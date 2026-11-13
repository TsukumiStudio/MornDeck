# MornDeck

English | [日本語](README.ja.md)

MornDeck is a tabbed terminal that runs inside your terminal, with a sidebar that tracks AI coding agents. It runs on macOS 11 or later (Apple Silicon and Intel) and on Linux (x86_64 and aarch64).

![MornDeck screen](docs/screenshot.svg)

## Features

- Open several tabs in one terminal and switch between them from the sidebar.
- Processes keep running after you close the terminal. Run `mdeck` again, even over ssh, to return to the same screen.
- An API and an MCP server let another program open tabs, send text, read the screen and wait for a reply.
- The sidebar shows whether Claude Code, Codex CLI and GitHub Copilot CLI (below: claude, codex, copilot) are working, waiting for input or done. A sound, a popup or a webhook can tell you when they finish (all off by default).

## Install

```bash
brew install --cask tsukumistudio/tap/morndeck
```

Other tools are also called `mdeck` (Markdown slide tools on crates.io, npm and PyPI). MornDeck is unrelated to them, so install it with the tap name (`tsukumistudio/tap`). Two commands with the same name cannot be installed together.

Update with "Check for updates" in the settings; it also restarts the server on the new version. After a plain `brew upgrade --cask morndeck`, the old server keeps running until you run `mdeck restart`. Either way the tabs come back and the agents resume their conversations.

### Windows (in preparation)

The Windows version is not released yet. When it is, it will install with `winget install TsukumiStudio.MornDeck`.

- It needs Windows 10 version 2004 (build 19041) or later, or Windows 11, on x64 or arm64, with WSL 2 and a Linux distribution (`wsl --install`).
- MornDeck itself runs inside WSL, and so do the agents: install claude, codex and copilot in the Linux distribution, not in Windows.
- Run `mdeck` from PowerShell or Windows Terminal, not from a WSL shell. It installs the matching Linux `mdeck` into WSL (`~/.local/bin/mdeck`), starts the server from Windows so that WSL keeps running, and opens the screen. Closing the window keeps the tabs running in WSL.
- MornDeck uses the default WSL distribution. Set `MDECK_WSL_DISTRO` to use another one. The distribution needs `curl` (for webhooks, update checks and the usage gauge).
- Popups appear as Windows notifications from "MornDeck". There are no completion sounds on Windows.
- "Check for updates" runs `winget upgrade` in a new window. Close the MornDeck screens when it asks; it then reopens MornDeck, on the new version if winget has it.

### Linux

Each release on GitHub (from 0.5.0) has `mdeck-<version>-linux-x86_64.tar.gz` and `mdeck-<version>-linux-aarch64.tar.gz`, each with a `.sha256` file. The binary is statically linked, so it runs on any distribution (x86_64 or aarch64).

```bash
v=<version>; a=$(uname -m)
curl -fLO "https://github.com/TsukumiStudio/MornDeck/releases/download/v$v/mdeck-$v-linux-$a.tar.gz"
curl -fLO "https://github.com/TsukumiStudio/MornDeck/releases/download/v$v/mdeck-$v-linux-$a.tar.gz.sha256"
sha256sum -c "mdeck-$v-linux-$a.tar.gz.sha256"
tar -xzf "mdeck-$v-linux-$a.tar.gz"
install -D -m 755 "mdeck-$v-linux-$a/mdeck" ~/.local/bin/mdeck   # ~/.local/bin must be on PATH
```

- `curl` is needed for webhooks, update checks and the usage gauge. Popups use `notify-send` (package `libnotify-bin` on Debian and Ubuntu); without it there are no popups. There are no completion sounds.
- Put `mdeck` in its final place before you first run it: the hooks call it by that path. If you move it later, run `mdeck kill`, then `mdeck`, and press `Enter` on each row under "Hooks" in the settings.
- To update, download the new tarball and run the same `install -D -m 755 …` command with it (plain `cp` fails with "Text file busy" while the server runs), then run `mdeck restart` (the tabs come back). "Check for updates" in the settings tells you when there is a new version.
- A Windows version is in preparation (see above). Until it is out, the tarball also works inside WSL, but the tabs stop when WSL shuts down.
- The Linux version is tested on Ubuntu (x86_64 and aarch64) in CI: the server, tabs, hooks, the API and restarts. Popups, the look in different terminals and desktops, and the agents on other distributions have not been checked on a real machine yet.

## Getting started

1. Run `mdeck`.
2. Press `F12`, then `c` to open the settings ("Config"). Under "Hooks", select the agents you use and press `Enter`.
3. Start `claude` or another agent in a tab.

## Basic controls

Press `F12` (fn+F12) to open the sidebar menu. Choose with `↑` `↓` (or `k` `j`) and press `Enter` to act. `F12` again closes whatever is open.

| Key | Action |
| --- | --- |
| `↑` `↓` | Select a tab (the tab shows while you move) |
| `Enter` | Return to the selected tab, or run the selected item ("Config", "Exit", "+") |
| `c` | Settings ("Config") |
| `e` | Close the screen ("Exit"; tabs and the server keep running) |
| `F12` | Close the menu |

Open a new tab with "+" at the end of the tab list. The mouse works too: click a tab to switch, drag the sidebar border to resize, and scroll in a tab.

The settings cover updates, installing and removing hooks, the sidebar width, the usage gauge, the notification sound and popup, and webhooks. "Test sound and popup" sends a real test. The popup comes from MornDeck: the first one makes macOS ask whether to allow MornDeck notifications (System Settings → Notifications → MornDeck), and clicking a popup opens nothing. It needs the MornDeck.app from Homebrew; a build from source, or a Mac where macOS does not allow it, shows the popup through Script Editor instead. Selecting a webhook shows "Edit", "Test" and "Delete"; pick one with `←` `→` and press `Enter`. "Test" sends a test event to that webhook only, and "Delete" asks again before removing it.

## Tab settings

> **To change a tab's settings, press `F12`, select the tab, and press `→` (right arrow).** While a tab is selected, the sidebar shows `⚙` on it and the bottom bar shows `→ Menu`. `Enter` only returns to the tab; the menu ("Settings for <tab>") opens with `→`. Right-clicking the tab does the same.

The menu has:

| Item | Change with |
| --- | --- |
| Name | `Enter`, type, `Enter` (empty clears it) |
| Webhook (`owner`) | `←` `→` |
| AutoYes | `←` `→` (the sidebar marks AutoYes tabs with `»` before the name) |
| Delete tab… | `Enter`, then choose "Delete" |

`mdeck kill` stops the server and every tab. The tab list is saved and comes back the next time you run `mdeck`. `mdeck restart` does both at once.

## Hooks

MornDeck learns each agent's state through its hooks. Install them from the settings (`F12`, `c`, Hooks, `Enter` on each agent). `mdeck hooks install` does the same for claude from the command line.

| Agent | Where |
| --- | --- |
| claude | a Claude Code plugin, `mdeck@mdeck`, installed from `~/.mdeck/claude-plugin` |
| codex | `~/.codex/hooks.json` (`$CODEX_HOME/hooks.json` if set) |
| copilot | `~/.copilot/hooks/mdeck.json` (used only by MornDeck) |

- For claude, mdeck runs `claude plugin` to add the plugin. Claude Code then records it in `settings.json` (`enabledPlugins`, and `extraKnownMarketplaces` with the path of `~/.mdeck/claude-plugin`) and under `~/.claude/plugins/`, and reads the plugin in place from `~/.mdeck/claude-plugin`. If you sync `settings.json` to other machines, consider `mdeck hooks settings`. If that fails (no `claude` command, an organization that limits marketplaces), it writes the hooks to `~/.claude/settings.json` (`$CLAUDE_CONFIG_DIR/settings.json` if set) instead and shows why; `mdeck doctor` repeats the reason, and the server does not retry on every start.
- `mdeck hooks settings` (or `s` on the Claude Code row) uses `settings.json` instead of the plugin, and mdeck keeps that choice. `mdeck hooks plugin` (or `p`) goes back to the plugin.
- Hooks already in `settings.json` from an earlier version move to the plugin when the server starts. mdeck removes them from `settings.json` only after the plugin is enabled; if the plugin cannot be installed they stay where they are. If both end up installed, mdeck counts each event once.
- Claude Code loads plugins when it starts, so a running claude session reports again only after a restart (`mdeck restart` restarts every tab) or `/reload-plugins` in it.
- When mdeck itself writes `settings.json`, `hooks.json` or `mdeck.json`, it adds only MornDeck's hooks; other settings are kept. The file is reformatted, and the content from before the first change is kept as `<file name>.mdeck-bak`.
- When the hook format changes, the server rewrites installed hooks to the new format when it starts, and rewrites the plugin files if they changed or went missing.
- The claude and copilot hooks do nothing outside a MornDeck tab. Codex runs hooks in a background process that cannot tell which tab a session is in, so while the MornDeck server runs, the codex hook sends the hook events of every Codex session on this Mac (including the Codex desktop app) to the local socket. The server uses the sessions it can match to a tab, and neither keeps nor logs the rest.
- Codex asks you to review hooks when they are installed or changed. Until you trust them in Codex (`/hooks`), Codex skips them and the tab shows no state.
- `CLAUDE_CONFIG_DIR` and `CODEX_HOME` (absolute paths) are read from the environment of the server, that is, of the shell that first ran `mdeck`. `mdeck hooks` and `mdeck uninstall` read them from the shell they run in. On macOS the claude gauge always reads the default keychain item, whatever `CLAUDE_CONFIG_DIR` says; on Linux it reads `.credentials.json` in the claude settings folder (`~/.claude`, or `$CLAUDE_CONFIG_DIR`).
- To remove the hooks, press `x` on the agent's row in the settings, or run `mdeck uninstall`. `Enter` on an installed row reinstalls them.
- If the sidebar shows no state, run `mdeck doctor`: it prints the versions, how the hooks are installed, whether any hook has reached the server and what to do if none has, and the tabs where an agent runs but no hook has arrived (the sidebar marks those `no hooks`).

### Organization settings (Claude Team and Enterprise)

An organization can manage Claude Code settings for its members. Two of them affect MornDeck:

- `strictPluginOnlyCustomization` with `"hooks"`: Claude Code skips hooks in `settings.json` but runs hooks from plugins. The plugin, mdeck's default, works. If the hooks are in `settings.json`, the settings screen shows "blocked by org policy" and `mdeck doctor` names the setting; `mdeck hooks plugin` fixes it.
- `allowManagedHooksOnly` or `disableAllHooks`: Claude Code runs no hooks from users or plugins. mdeck cannot show claude state.

mdeck reads these from Claude Code's cached server-managed settings (`~/.claude/remote-settings.json`), `managed-settings.json` and `settings.json`. A policy delivered as an MDM profile is not detected, but the plugin works the same way.

## Usage gauge

The bottom of the sidebar shows how much of the weekly limit (claude, codex) and the monthly limit (copilot) you have used. To do this MornDeck reads the following:

| Agent | What it reads | Where it is sent |
| --- | --- | --- |
| claude | The OAuth token in the keychain item `Claude Code-credentials` (Linux: `~/.claude/.credentials.json`) | `api.anthropic.com/api/oauth/usage`, every 5 minutes |
| copilot | A token from `COPILOT_GITHUB_TOKEN` / `GH_TOKEN` / `GITHUB_TOKEN`, the keychain item `copilot-cli`, or `~/.copilot/config.json` | `api.github.com/copilot_internal/user`, every 5 minutes |
| codex | The session logs in `~/.codex/sessions` (`$CODEX_HOME/sessions` if set) | Nowhere |

- Tokens go only to the hosts above. They are not stored or logged.
- Turn off "Usage gauge" in the settings and MornDeck reads no token or log and calls neither API.

Apart from these two, MornDeck connects out only for "Check for updates" (github.com) and to the webhooks you register. It sends no usage data.

## AutoYes

Turn on AutoYes in a tab's menu and MornDeck answers Yes to Yes / No permission prompts.

- It also answers Yes to prompts for deleting files or running `git push`. Turn it on only in tabs whose work you trust.
- It answers only permission prompts: every option starts with `Yes`, `No`, `Allow`, `Always allow`, `Cancel`, `Deny` or `Decline` (in English), and one of them declines. It picks the first `Yes`, or else `Allow` (this time only, as in codex's Computer Use prompt). It never picks `Allow for this session` or `Always allow`.
- With the claude hooks installed, it leaves questions and plan approvals to you. Without hooks, or with codex and copilot, it cannot tell them apart and answers any such menu.

## API

```bash
mdeck api '{"cmd":"open_tab","name":"work","cwd":"/path/to/repo","agent":"claude"}'
mdeck api '{"cmd":"send","tab":"work","text":"fix the tests"}'
```

`tab` is a tab id or name. Arguments marked `?` are optional.

| Command | Arguments | Action |
| --- | --- | --- |
| `list_tabs` | | List tabs |
| `open_tab` | `cwd`, `name?`, `owner?`, `agent?`, `args?`, `run?`, `yolo?`, `auto_yes?`, `wait?`, `timeout_ms?` | Open a tab. `agent` is `claude` (default), `codex` or `copilot`. With `run`, runs that command instead |
| `close_tab` | `tab` or `owner`, `force?` | Close a tab. With `owner`, closes every tab of that owner |
| `update_tab` | `tab`, `name?`, `owner?`, `auto_yes?` | Change a tab's settings |
| `send` | `tab`, `text`, `wait?`, `request_id?`, `force?`, `timeout_ms?` | Type the text, press Enter and wait for the reply |
| `press_keys` | `tab`, `keys`, `force?` | Send raw keys |
| `choose` | `tab`, `n`, `wait?`, `force?`, `timeout_ms?` | Pick an option by number |
| `get_answer` | `tab`, `timeout_ms?` | The current reply and options. With `timeout_ms`, waits until a reply or options appear |
| `get_screen` | `tab` | The text on the screen |
| `subscribe` | | Stream events (tabs opened and closed, state changes, options, AutoYes), one JSON object per line |
| `list_webhooks` | `owner?` | List webhooks |
| `add_webhook` | `url`, `owner?`, `headers?` | Add a webhook |
| `remove_webhook` | `url`, `owner?` | Remove a webhook |
| `test_webhooks` | `url`, `owner?` | Send a test event to one webhook |

`yolo: true` starts the agent with no permission prompts and no sandbox (`claude --dangerously-skip-permissions` and the like). The agent can then run anything, so use it only where damage does not matter.

Commands that wait return this shape:

```json
{"ok": true, "tab": {...}, "state": "done", "seq": 3, "answer": "Fixed.", "timed_out": false}
```

| Key | Meaning |
| --- | --- |
| `state` | `done`: `answer` is the reply. `waiting`: read `question` and `options`, then answer with `choose` (for codex prompts that send no hook, such as the Computer Use permission, `question` is `{"prompt": ..., "details": [...]}` read from the screen). `blocked`: a startup prompt stopped the agent. Otherwise `starting`, `idle`, `working`, or `null` when no agent runs in the tab |
| `seq` | A number that goes up with each reply. `send` returns the number its reply will have as `expect_seq` |
| `timed_out` | `true`: call `get_answer` with `timeout_ms` to keep waiting |

On failure a command returns `{"ok": false, "error": {"code": "...", "message": "..."}}`. `code` is one of `invalid` (bad arguments), `no_such_tab`, `busy` (the agent is working), `not_ready` (the agent is starting), `tab_waiting` (the agent is asking; answer with `choose`), `not_found`, `self_tab` (the tab the caller runs in), `failed`, `unknown_cmd`.

While the version is 0.x, the arguments and results of the API and MCP may change. Changes are listed in the [CHANGELOG](CHANGELOG.md).

## Using it from AI agents

Claude Code, Codex CLI, GitHub Copilot CLI and other MCP clients can open tabs, send instructions to the agents in them and wait for the replies.

```bash
mdeck mcp install   # register the MCP server with the agent CLIs that are installed
mdeck guide         # print the guide for agents
```

[docs/agent-guide.md](docs/agent-guide.md) is written for the agent to read: how to connect, the basic flow, what each state means, how to wait, and what each error means. An agent with a shell can also use `mdeck tabs`, `mdeck send <tab> <text>` and `mdeck wait <tab>` without any setup.

mdeck refuses to send to, choose in or close the tab the caller runs in (error `self_tab`) unless `force` is true.

## MCP

The tools are the API commands (except `subscribe`). `mdeck mcp install` writes the configuration below for you.

```json
{"mcpServers": {"mdeck": {"command": "/opt/homebrew/bin/mdeck", "args": ["mcp"]}}}
```

On an Intel Mac the path is `/usr/local/bin/mdeck`.

From another machine, start it over ssh:

```json
{"mcpServers": {"mdeck": {"command": "ssh", "args": ["<user>@<host>", "/opt/homebrew/bin/mdeck", "mcp"]}}}
```

A key that can use MCP can run anything on that Mac (see [Security](#security)). To limit the key to `mdeck mcp`, prefix it with `command=` in `~/.ssh/authorized_keys` on the Mac:

```
restrict,command="/opt/homebrew/bin/mdeck mcp" ssh-ed25519 <public key>
```

## Webhook

MornDeck POSTs JSON to the registered URLs at these moments. Failed deliveries are not retried.

| When | JSON |
| --- | --- |
| The agent finished a reply | `{"event": "state", "state": "done", "tab": {...}, "answer": "...", "seq": 3}` |
| The agent showed options | `{"event": "choices", "tab": {...}, "question": {...}, "options": [{"n": 1, "label": "Yes", "selected": true}]}` |
| AutoYes answered | `{"event": "auto_yes", "tab": {...}, "label": "Yes", "options": [...]}` |

```bash
mdeck api '{"cmd":"add_webhook","owner":"mytool","url":"https://example.com/hook","headers":{"Authorization":"Bearer ..."}}'
```

With `owner`, the webhook receives only the events of tabs with the same `owner`. `test_webhooks` sends `{"event": "test", "owner": ..., "message": ...}`.

## Security

- MornDeck listens only on `~/.mdeck/mdeck.sock`, a Unix socket that only you can read and write. It opens no network port.
- Anyone who can use this socket, the API or MCP can run anything on the Mac as you (`run` in `open_tab`, `send`, `press_keys`). An ssh key that may use MCP is as powerful as a shell.
- Webhooks send the reply text to the receiver. To an `http://` URL, the text and the header secrets travel unencrypted.
- `~/.mdeck/events.log` records API calls, replies and hook payloads; each string value is cut at 300 characters. `mcp.log` records the `SSH_CONNECTION` (client address) of each MCP session. Webhook header values are not logged, and webhook URLs are logged only up to the host.
- Webhook URLs and header values are stored as plain text in `~/.mdeck/webhooks.json` and `owner_webhooks.json`, readable only by you.
- When the server starts, including when `mdeck api` or `mdeck mcp` starts it, saved tabs resume their agent conversations with the same `yolo` and AutoYes settings.

## Uninstall

```bash
mdeck uninstall
brew uninstall --cask --zap tsukumistudio/tap/morndeck
```

On Linux, run `mdeck uninstall`, then `rm ~/.local/bin/mdeck` and `rm -r ~/.mdeck` (logs and webhook secrets).

`mdeck uninstall` removes the hooks (the claude plugin and its marketplace entry too) and the MCP registration and stops the server. Run it before `--zap`. `--zap` moves `~/.mdeck/` to the Trash; it holds the logs and webhook secrets, so empty the Trash to erase them. The `.mdeck-bak` copies of your settings from before the hooks are kept.

## Files

Settings, tabs and logs are in `~/.mdeck/`.

## Build from source

Requires Rust; rustup installs the version pinned in `rust-toolchain.toml`.

```bash
cargo build --release   # target/release/mdeck
zsh build.sh            # dist/MornDeck.app (ad-hoc signed)
```

Released builds are signed with a Developer ID and notarized by Apple.

## License

[Unlicense](LICENSE)

The licenses of the bundled libraries are in [THIRD_PARTY_LICENSES](THIRD_PARTY_LICENSES); `mdeck --licenses` prints them.

MornDeck is an unofficial tool, not affiliated with Anthropic, OpenAI or GitHub. Claude, Codex and GitHub Copilot are trademarks of their respective owners.
