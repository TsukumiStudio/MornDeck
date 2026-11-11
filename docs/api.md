# Bot 連携と API / MCP

[README](../README.md) の続きです。Bot から MornDeck を使うための説明と、API・MCP のリファレンスです。

## 用語

- **エージェント**: タブの中で動く claude (Claude Code)・codex・copilot (GitHub Copilot CLI)。
- **Bot**: API や MCP で MornDeck を操作する外部のプログラム。
- **owner**: タブの持ち主を表す名前。同じ名前の通知先 (webhook) に、そのタブの完了と選択肢が届く。画面では「通知先」と呼ぶ。

## Bot 連携

Bot はタブを開き、指示を送り、回答を受け取る。

1. `add_webhook` で自分の通知先を登録する (owner に自分の名前)。一度でよい。
2. `open_tab` で `owner` を付けてタブを開く。準備できるまで待ち、起動時の確認画面 (フォルダの信頼など) で止まったら `state: "blocked"` と `options` を返すので、`choose` で答える。
3. `send` で指示を送る。既定は回答か選択肢が出るまで待つ。
   - `state: "done"`: `answer` が回答。
   - `state: "waiting"`: `question` と `options` を見て `choose` で答える。
   - `timed_out: true`: まだ作業中。`wait` を呼び直す。
4. 長い作業は `send` に `wait: false` を付け、完了を webhook で受け取る。届かなかったときに備えて、時間を決めて `get_answer` を見る。回答の `seq` が `send` の返した `expect_seq` 以上なら、その依頼の回答。

`state` の値:

| state | 意味 |
| --- | --- |
| `starting` / `blocked` | 起動待ち / 起動時の確認画面で止まっている |
| `idle` | 起動して入力待ち (まだ指示が無い) |
| `working` | 作業中 |
| `waiting` | 実行の確認や質問で止まっている |
| `done` | 回答した |
| `null` | エージェントが動いていない (シェルだけのタブなど) |

エージェントごとの違い:

| エージェント | 入力待ち (`waiting`) の検知 | 回答本文 (`answer`) | `yolo` で付けるフラグ |
| --- | --- | --- | --- |
| claude | できる | 取れる | `--dangerously-skip-permissions` |
| codex | できる | 取れる | `--dangerously-bypass-approvals-and-sandbox` |
| copilot | できない (確認画面でも `working` のまま) | 取れない (`null`) | `--yolo` |

copilot で確認に止まらないようにするには、`yolo` か AutoYes を使う。

### webhook

通知先へ JSON を POST する。送るのは次の 3 つだけ。

- 完了 (`event: "state"`, `state: "done"`)
- 選択肢の提示 (`event: "choices"`)
- AutoYes が答えたこと (`event: "auto_yes"`)

```json
{"event": "state", "tab": {"id": 3, "name": "mybot-1", "owner": "mybot", "state": "done", ...}, "state": "done", "answer": "...", "seq": 5}
{"event": "choices", "tab": {...}, "question": {"tool": "Bash", "input": {...}}, "options": [{"n": 1, "label": "Yes", "selected": true, "numbered": true}]}
{"event": "auto_yes", "tab": {...}, "label": "Yes", "options": [...]}
```

- 通知先は 2 種類ある。owner を付けて登録したものには、その owner のタブの通知だけが届く。owner 無しで登録したもの (全体) には、全タブの通知が届く。全体の通知先は API だけで扱い、Config には出ない。
- 送るのは 1 回だけで、失敗しても再送しない。結果は `webhook.log` に残る。
- URL は Bot ごとに固定する。依頼ごとに違う URL を足すと溜まり、以後の通知がすべてに届く。
- owner は通知の振り分けの名前で、権限の分離ではない。どの Bot も任意の owner を名乗れる。

### 注意

- `yolo` と AutoYes は、サーバーを起動し直しても続く。
- MCP や API を使える相手は、そのユーザーで任意のコマンドを実行できる (`open_tab` の `run`、`yolo`、他のタブへの `send` など)。SSH の鍵や MCP の設定は、信頼できる Bot にだけ渡す。
- タブの中のエージェントも、環境変数 `MDECK_SOCK` から同じ API を使える。

## API と MCP

`~/.mdeck/mdeck.sock` に JSON を 1 行送ると、1 行返して接続を閉じる (1 接続 1 リクエスト)。`mdeck api '<json>'` でも呼べ、`ok` が `true` でなければ終了コード 1 で終わる。

MCP (`mdeck mcp`) のツール名と引数は、API のコマンド名と引数と同じ (`cmd` 以外)。MCP では待つツールの既定の上限を短くしている (`open_tab` は 1 分、`send` `choose` `wait` は 2 分)。時間切れなら `wait` を呼び直す。API の既定の上限は `open_tab` が 1 分、ほかの待つコマンドが 10 分 (最大 30 分)。

### コマンド

`tab` はタブの番号か名前 (数字だけの文字列は番号として扱う)。

| コマンド | 引数 | 内容 |
| --- | --- | --- |
| `list_tabs` | | タブの一覧 (`tabs`) |
| `open_tab` | `cwd`, `name?`, `owner?`, `agent?`, `args?`, `run?`, `yolo?`, `auto_yes?`, `wait?`, `timeout_ms?` | タブを開く。`agent` は `claude` / `codex` / `copilot`。`agent` も `run` も無ければ claude。シェルだけのタブは `run: ""` か実行するコマンド (`agent` と `run` は片方だけ)。エージェントなら既定は準備できるまで待つ。時間切れでもタブは作られているので、作り直さず `wait` や `choose` を続ける |
| `close_tab` | `tab` | タブを閉じる (中のエージェントも終わる) |
| `close_tabs` | `owner` | その owner のタブをまとめて閉じる |
| `update_tab` | `tab`, `name?`, `owner?`, `auto_yes?` | 渡したものだけ変える。`name` と `owner` は `""` で外す |
| `show_tab` | `tab` | 人が見ている画面に出すタブを切り替える |
| `send` | `tab`, `text`, `wait?`, `request_id?`, `force?`, `timeout_ms?` | 貼り付けて Enter。既定は回答か選択肢が出るまで待つ。作業中のタブは `busy` で断る (`force: true` で送る)。同じ `request_id` の送り直しは打ち込まず、最初の受付結果を `duplicate: true` 付きで返す |
| `press_keys` | `tab`, `keys` | キーをそのまま送る (例: `"\u001b"` で Esc)。作業中かどうかの扱いは変えない |
| `choose` | `tab`, `n`, `wait?`, `timeout_ms?` | 選択肢を番号で選ぶ。既定は次に止まるまで待つ |
| `wait` | `tab`, `timeout_ms?` | 作業が終わる (`done` / `waiting`) まで待つ |
| `get_answer` | `tab` | 待たずに今の回答を返す |
| `get_choices` | `tab` | 提示中の選択肢と、確認を出したツール |
| `get_screen` | `tab` | 画面の各行 (`lines`) |
| `list_webhooks` | `owner?` | 通知先の一覧 (ヘッダーは名前だけ) |
| `add_webhook` | `url`, `owner?`, `headers?` | 通知先を足す。同じ URL はヘッダーごと置き換える |
| `remove_webhook` | `url`, `owner?` | 通知先を消す |
| `test_webhooks` | `owner?` | 通知先にテストの通知を今すぐ送り、1 件ずつ `{url, http, error}` を返す (URL の末尾は伏せる) |

内部用 (MCP には出さない): `attach` (画面)、`subscribe` (イベントを 1 行ずつ流し続ける)、`hook` (エージェントのフック)、`restart` (サーバーを起動し直す。運用は `docs/operations.md`)。

### 戻り値

成功は `{"ok": true, ...}`、失敗は `{"ok": false, "error": {"code": "...", "message": "..."}}`。

**タブ** (`tab` と `tabs` の各要素):

```json
{"id": 3, "name": "mybot-1", "title": "...", "owner": "mybot", "cwd": "/path", "agent": "claude", "state": "done",
 "active": false, "last_used": 1794376000, "auto_yes": false, "yolo": false, "unread": true}
```

**待つ系** (`open_tab` / `send` / `choose` / `wait` / `get_answer`) は同じ形で返す。確認画面で止まっても、時間切れでも `ok: true` になる。

```json
{"ok": true, "tab": {...}, "state": "done", "seq": 5, "answer": "直しました。", "timed_out": false, "expect_seq": 5}
{"ok": true, "tab": {...}, "state": "waiting", "seq": 4, "answer": null, "timed_out": false,
 "question": {"tool": "Bash", "input": {"command": "rm -rf build"}},
 "options": [{"n": 1, "label": "Yes", "selected": true, "numbered": true}, {"n": 2, "label": "No", "selected": false, "numbered": true}]}
{"ok": true, "tab": {...}, "state": "working", "seq": 4, "answer": null, "timed_out": true}
```

- `answer` は、作業中と起動待ちでは `null`。
- `expect_seq` は `send` だけが返す。
- `open_tab` は、時間切れでフックが無いと `hint` を付ける。

**エラーの `code`**:

| code | 意味 |
| --- | --- |
| `invalid` | 引数が足りない・おかしい |
| `no_such_tab` | タブが無い |
| `busy` | 作業中 (`send`) |
| `not_ready` | エージェントが起動していない (`send`。確認画面なら `get_choices` を見て `choose`) |
| `not_found` | 選択肢・通知先が無い |
| `failed` | 書き込み・保存・通知の送信などの失敗 |
| `unknown_cmd` | 知らないコマンド |

名前 (タブ名と owner) は、空白なしの 1〜32 文字で、数字だけは不可。タブ名は重複できない。

### 例

```bash
mdeck api '{"cmd":"add_webhook","owner":"mybot","url":"https://example.com/mybot","headers":{"Authorization":"Bearer ..."}}'
mdeck api '{"cmd":"open_tab","owner":"mybot","name":"mybot-1","cwd":"/path/to/repo","agent":"claude"}'
mdeck api '{"cmd":"send","tab":"mybot-1","text":"テストを直して"}'
```

### MCP の登録

同じ Mac の MCP クライアントには、`mdeck mcp` をそのまま登録する (Intel Mac のパスは `/usr/local/bin/mdeck`)。

```json
{"mcpServers": {"mdeck": {"command": "/opt/homebrew/bin/mdeck", "args": ["mcp"]}}}
```

別のマシンからは、forced command の SSH 鍵で起動させる。`~/.ssh/authorized_keys` に次を書く。この鍵を持つ相手は、そのユーザーで任意のコマンドを実行できる (上の「注意」)。

```
restrict,command="/opt/homebrew/bin/mdeck mcp" ssh-ed25519 <PUBLIC_KEY> mdeck-mcp
```

接続側は `ssh -i <鍵> -o BatchMode=yes <user>@<host>` を stdio のコマンドとして登録する (`-t` は付けない)。

MCP は 1 接続の呼び出しを順に処理する。待っている間は、ほかの呼び出しも返らない。複数のタブを並行して待つなら、`send` に `wait: false` を付けて webhook で受け取る。
