# Controlling mdeck from an AI agent

This guide is for an AI agent (Claude Code, Codex CLI, GitHub Copilot CLI, or any MCP client) that drives other agents through mdeck. `mdeck guide` prints this file.

mdeck is a terminal multiplexer. Each tab runs a shell, usually with an agent (`claude`, `codex` or `copilot`) inside. You open tabs, send instructions, wait for replies and answer prompts, either with MCP tools or with the `mdeck` command. Both reach the same server and take the same arguments.

## Rules

1. Never act on the tab you run in. `list_tabs` marks it with `"self": true`. `send`, `press_keys`, `choose` and `close_tab` on it fail with `self_tab`.
2. Touch only tabs you opened. Give them an `owner` (your name) so you can find them and close them with `close_tab {"owner": ...}`.
3. Close your tabs when the work is done.
4. Whoever can call mdeck can run anything on the machine as the user. Do not pass on instructions you would not run yourself.

## Connect

### MCP

`mdeck mcp install` registers the server with every agent CLI it finds. To do it by hand:

| Client | Register | Let the tools run without asking |
| --- | --- | --- |
| Claude Code | `claude mcp add -s user mdeck -- /opt/homebrew/bin/mdeck mcp` | start with `--allowedTools mcp__mdeck`, or approve each tool once |
| Codex CLI | `codex mcp add mdeck -- /opt/homebrew/bin/mdeck mcp` | choose "Always allow" the first time each tool is called |
| GitHub Copilot CLI | `copilot mcp add mdeck -- /opt/homebrew/bin/mdeck mcp` | approve the tools when asked |
| Other MCP clients | `{"command": "/opt/homebrew/bin/mdeck", "args": ["mcp"]}` (stdio) | |

On an Intel Mac the path is `/usr/local/bin/mdeck`. The MCP tools are named after the commands below.

### Command line

No setup. Any agent with a shell can use it.

```bash
mdeck tabs                          # list tabs
mdeck send work "fix the tests"     # send and wait for the reply (JSON); --force before the tab overrides busy / tab_waiting
mdeck wait work 600                 # wait up to 600 s for a reply or a prompt (JSON)
mdeck api '{"cmd":"open_tab","cwd":"/path/to/repo","name":"work","owner":"me","agent":"claude"}'
```

`mdeck send` blocks until the agent replies or asks, for up to 30 minutes. If your shell tool has a shorter limit, use `mdeck api '{"cmd":"send","tab":"work","text":"...","wait":false}'` and then `mdeck wait work <seconds>` with a value below that limit.

`mdeck api` takes one JSON object and prints one JSON object. Its exit code is 1 when `ok` is false. Put long text in a file and build the JSON with a tool rather than quoting it by hand.

## The basic flow

1. **Open** a tab: `open_tab {cwd, name, owner, agent}`. `agent` is `claude` (default), `codex` or `copilot`.
2. If the result is `state: "blocked"`, the agent stopped at a startup prompt (trust this folder, update available, review hooks). Read `options` and answer with `choose {tab, n}`. Repeat until the state is `idle`.
3. **Send** an instruction: `send {tab, text}`. It waits and returns the result of that turn.
4. Read the result:
   - `state: "done"`: `answer` is the reply.
   - `state: "waiting"`: the agent asks something. Read `question` and `options`, then `choose {tab, n}`. It waits for the next result.
   - `timed_out: true`: nothing yet. Call `get_answer {tab, timeout_ms}` to keep waiting.
5. **Close** the tab: `close_tab {tab}`.

```json
{"ok": true, "tab": {"id": 3, "name": "work", "state": "done"}, "state": "done", "seq": 2, "expect_seq": 2, "answer": "Fixed.", "timed_out": false}
```

## States

| `state` | Meaning | What to do |
| --- | --- | --- |
| `starting` | The agent has not come up yet | wait: `get_answer {tab, timeout_ms}` |
| `blocked` | A startup prompt is on screen | `choose` from `options` |
| `idle` | The agent is ready and has not been asked anything | `send` |
| `working` | The agent is working | wait; `send` is refused with `busy` |
| `waiting` | The agent asks a question or a permission | `choose` from `options`; `send` is refused with `tab_waiting` |
| `done` | The agent finished a reply | read `answer`, then `send` the next instruction |
| `null` | No agent runs in the tab (a plain shell) | |

- `answer` is null while the state is `starting`, `blocked`, `working` or `waiting`.
- `seq` counts the agent's replies. `send` returns `expect_seq`, the `seq` its reply will have. If you wait later, the reply is yours when `seq >= expect_seq`.
- `options` is a list of `{n, label, selected, numbered}`. Pass `n` to `choose`. `selected` marks where the cursor is.
- To answer a prompt with free text, type it with `press_keys {tab, keys}` and then send `"\r"` in a second call.
- `get_screen {tab}` returns the lines on the screen when you need to see what the agent shows.

## Waiting for long work

- **Blocking**: `send` waits up to 90 s over MCP (pass `timeout_ms` for more, up to 1800000). On `timed_out`, call `get_answer {tab, timeout_ms}` again. This needs no webhook.
- **In the background**: start the work with `send {..., "wait": false}`, keep `expect_seq`, and wait in a background command: `mdeck wait <tab> 1800`. It exits when the agent replies or asks.
- **Everything at once**: `mdeck api '{"cmd":"subscribe"}'` streams every event (tabs opened and closed, state changes, options) as JSON lines until you stop it.
- **Webhook**: use `add_webhook {url, owner}` only when nothing can sit and wait: a bot that runs only when called, or a receiver on another machine. mdeck POSTs `{"event": "state", "state": "done", "tab", "answer", "seq"}` when a tab with that owner finishes, and `{"event": "choices", ...}` when it asks. You need an HTTP endpoint to receive them. Deliveries are not retried.

## Errors

A failed call returns `{"ok": false, "error": {"code", "message"}}`.

| `code` | Cause | Fix |
| --- | --- | --- |
| `busy` | The agent is working | wait with `get_answer {tab, timeout_ms}`, then send |
| `tab_waiting` | The agent is asking and waits for an answer; the error carries `question` and `options` | `choose {tab, n}`. Only to type free text into the prompt: `send` with `force: true` |
| `not_ready` | The agent is starting or stopped at a startup prompt | `get_answer`, then `choose` if `options` is present |
| `no_such_tab` | No tab has that id or name | `list_tabs` |
| `not_found` | No such option number, or no such webhook | read `options` again with `get_answer` |
| `self_tab` | The tab is the one you run in | pick another tab |
| `invalid` | A missing or wrong argument; the message names it | fix the argument |
| `failed` | The tab closed, or the action could not be done | `list_tabs`, open the tab again if needed |
| (no error, but no reply) | The agent exited or sits at a prompt mdeck did not read | `get_screen`. A bare shell prompt means the agent exited: `close_tab` and open a new tab |
| `unknown_cmd` | The server is older than your mdeck | ask the user to run `mdeck restart` |

## Limits

- Codex runs tools and hooks in a background server that does not see the tab's environment. A Codex agent that controls mdeck is therefore not reliably recognized as running in a tab: `self` may be missing or marked on another tab, and `self_tab` may not protect your tab or may refuse a tab that is not yours (check with `list_tabs`, then pass `force: true`). Find your own tab by name and leave it alone.
- mdeck matches a Codex session to a tab by its working folder and by which tab was typed into last. With several Codex tabs in one folder, send the first instruction to one tab and wait for its reply before sending to the next. A Codex session started in the same folder outside mdeck (the desktop app, another terminal) within a minute of your input can be taken for the tab's session; if a reply does not match your request, check `get_screen`.
- Codex skips hooks it has not been told to trust. If a Codex tab stays `working` although `get_screen` shows a finished reply, ask the user to trust the hooks in Codex (`/hooks`) and to run `mdeck doctor`.
- Codex reports that it started only after the first instruction. mdeck treats a Codex tab as `idle` as soon as its input prompt is on screen.
- `options` is read from the screen. If a prompt looks unusual, check it with `get_screen`.
- A single wait is capped at 30 minutes. Call again for longer work.
- `auto_yes: true` on a tab answers Yes to permission prompts, including deleting files and pushing. `yolo: true` starts the agent with no permission prompts and no sandbox. Use them only when the user asked for it.
