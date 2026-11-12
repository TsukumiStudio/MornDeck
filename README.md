# MornDeck

English | [日本語](README.ja.md)

MornDeck is a tabbed terminal that runs inside your terminal, with a sidebar that tracks AI coding agents. It runs on macOS 11 or later (Apple Silicon and Intel).

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

## Getting started

1. Run `mdeck`.
2. Press `F12`, then `c` to open the settings ("Config"). Under "Hooks", select the agents you use and press `Enter`.
3. Start `claude` or another agent in a tab.

## Keys

`F12` (fn+F12) moves the focus to the sidebar.

| Key | Action |
| --- | --- |
| `↑` `↓` | Select a tab |
| `Enter` | Return to that tab |
| `→` | Tab menu (name, webhook, AutoYes, delete) |
| `c` | Settings ("Config") |
| `e` | Close the screen ("Exit"; tabs keep running) |
| `F12` | Cancel |

Open a new tab with "+" in the sidebar. The mouse works too: click, right-click and drag.

The settings cover updates, installing and removing hooks, the sidebar width, the usage gauge, the notification sound and popup, and webhooks. "Test sound and popup" and "Test webhooks" send a real test notification and show whether it got through.

`mdeck kill` stops the server and every tab. The tab list is saved and comes back the next time you run `mdeck`. `mdeck restart` does both at once.

## Hooks

MornDeck learns each agent's state through its hooks. Installing them from the settings rewrites these files:

| Agent | File |
| --- | --- |
| claude | `~/.claude/settings.json` (`$CLAUDE_CONFIG_DIR/settings.json` if set) |
| codex | `~/.codex/hooks.json` (`$CODEX_HOME/hooks.json` if set) |
| copilot | `~/.copilot/hooks/mdeck.json` (used only by MornDeck) |

- Only MornDeck's hooks are added; other settings are kept. The file is reformatted.
- The content from before the first change is kept as `<file name>.mdeck-bak`.
- When the hook format changes, the server rewrites installed hooks to the new format when it starts.
- The hooks do nothing outside a MornDeck tab.
- `CLAUDE_CONFIG_DIR` and `CODEX_HOME` (absolute paths) are read from the environment of the server, that is, of the shell that first ran `mdeck`. `mdeck uninstall` reads them from the shell it runs in. The claude gauge always reads the default keychain item, whatever `CLAUDE_CONFIG_DIR` says.
- To remove them, press `x` on the agent's row in the settings, or run `mdeck uninstall`. `Enter` on an installed row reinstalls them.
- If the sidebar shows no state, run `mdeck doctor`: it prints the versions, the hook status and whether any hook has reached the server.

## Usage gauge

The bottom of the sidebar shows how much of the weekly limit (claude, codex) and the monthly limit (copilot) you have used. To do this MornDeck reads the following:

| Agent | What it reads | Where it is sent |
| --- | --- | --- |
| claude | The OAuth token in the keychain item `Claude Code-credentials` | `api.anthropic.com/api/oauth/usage`, every 5 minutes |
| copilot | A token from `COPILOT_GITHUB_TOKEN` / `GH_TOKEN` / `GITHUB_TOKEN`, the keychain item `copilot-cli`, or `~/.copilot/config.json` | `api.github.com/copilot_internal/user`, every 5 minutes |
| codex | The session logs in `~/.codex/sessions` (`$CODEX_HOME/sessions` if set) | Nowhere |

- Tokens go only to the hosts above. They are not stored or logged.
- Turn off "Usage gauge" in the settings and MornDeck reads no token or log and calls neither API.

Apart from these two, MornDeck connects out only for "Check for updates" (github.com) and to the webhooks you register. It sends no usage data.

## AutoYes

Turn on AutoYes in a tab's menu and MornDeck answers Yes to Yes / No permission prompts.

- It also answers Yes to prompts for deleting files or running `git push`. Turn it on only in tabs whose work you trust.
- It answers only prompts whose options all start with `Yes` / `No` in English.
- With the claude hooks installed, it leaves questions and plan approvals to you. Without hooks, or with codex and copilot, it cannot tell them apart and answers any menu whose options all start with `Yes` / `No`.

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
| `close_tab` | `tab` or `owner` | Close a tab. With `owner`, closes every tab of that owner |
| `update_tab` | `tab`, `name?`, `owner?`, `auto_yes?` | Change a tab's settings |
| `send` | `tab`, `text`, `wait?`, `request_id?`, `force?`, `timeout_ms?` | Type the text, press Enter and wait for the reply |
| `press_keys` | `tab`, `keys` | Send raw keys |
| `choose` | `tab`, `n`, `wait?`, `timeout_ms?` | Pick an option by number |
| `get_answer` | `tab`, `timeout_ms?` | The current reply and options. With `timeout_ms`, waits until a reply or options appear |
| `get_screen` | `tab` | The text on the screen |
| `subscribe` | | Stream events (tabs opened and closed, state changes, options, AutoYes), one JSON object per line |
| `list_webhooks` | `owner?` | List webhooks |
| `add_webhook` | `url`, `owner?`, `headers?` | Add a webhook |
| `remove_webhook` | `url`, `owner?` | Remove a webhook |
| `test_webhooks` | `owner?` | Send a test event to the webhooks |

`yolo: true` starts the agent with no permission prompts and no sandbox (`claude --dangerously-skip-permissions` and the like). The agent can then run anything, so use it only where damage does not matter.

Commands that wait return this shape:

```json
{"ok": true, "tab": {...}, "state": "done", "seq": 3, "answer": "Fixed.", "timed_out": false}
```

| Key | Meaning |
| --- | --- |
| `state` | `done`: `answer` is the reply. `waiting`: read `question` and `options`, then answer with `choose`. `blocked`: a startup prompt stopped the agent. Otherwise `starting`, `idle`, `working`, or `null` when no agent runs in the tab |
| `seq` | A number that goes up with each reply. `send` returns the number its reply will have as `expect_seq` |
| `timed_out` | `true`: call `get_answer` with `timeout_ms` to keep waiting |

On failure a command returns `{"ok": false, "error": {"code": "...", "message": "..."}}`. `code` is one of `invalid` (bad arguments), `no_such_tab`, `busy` (the agent is working), `not_ready` (the agent is starting), `not_found`, `failed`, `unknown_cmd`.

While the version is 0.x, the arguments and results of the API and MCP may change. Changes are listed in the [CHANGELOG](CHANGELOG.md).

## MCP

The tools are the API commands (except `subscribe`).

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

`mdeck uninstall` removes the hooks and stops the server. `--zap` moves `~/.mdeck/` to the Trash; it holds the logs and webhook secrets, so empty the Trash to erase them. The `.mdeck-bak` copies of your settings from before the hooks are kept.

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
