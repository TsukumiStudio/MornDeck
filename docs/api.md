# API と MCP

[README](../README.md) の続きです。外部のプログラムから MornDeck を操作するための説明です。

## 言葉

- **エージェント**: タブの中で動く claude (Claude Code)・codex・copilot (GitHub Copilot CLI) のことです。
- **owner**: タブの持ち主を表す名前です。同じ名前の通知先 (webhook) に、そのタブの通知が届きます。画面では「通知先」と呼んでいます。

## 使い方の流れ

タブを開いて指示を送り、エージェントの返事を受け取るまでの流れです。

1. `add_webhook` で、自分の通知先を登録します。`owner` には自分の名前を入れます。登録は一度だけで済みます。
2. `open_tab` に `owner` を付けて、タブを開きます。エージェントの準備ができるまで待ちます。起動時の確認画面 (フォルダを信頼するかどうかなど) で止まったときは、`state: "blocked"` と `options` が返るので、`choose` で答えてください。
3. `send` で指示を送ります。既定では、返事か選択肢が出るまで待ちます。
   - `state: "done"` のときは、`answer` が返事です。
   - `state: "waiting"` のときは、`question` と `options` を見て、`choose` で答えてください。
   - `timed_out: true` のときは、まだ作業中です。`wait` を呼び直してください。
4. 時間のかかる作業では、`send` に `wait: false` を付けて、返事を webhook で受け取ります。webhook が届かなかったときに備えて、時間を決めて `get_answer` も見てください。返ってきた `seq` が、`send` の返した `expect_seq` 以上なら、その指示への返事です。

`state` の値:

| state | 意味 |
| --- | --- |
| `starting` / `blocked` | 起動を待っています / 起動時の確認画面で止まっています |
| `idle` | 起動して、最初の指示を待っています |
| `working` | 作業中です |
| `waiting` | 実行の確認や質問で止まっています |
| `done` | 返事を書き終えました |
| `null` | エージェントが動いていません (シェルだけのタブなど) |

エージェントごとの違い:

| エージェント | `waiting` を見分けられるか | 返事 (`answer`) を取れるか | `yolo` で付けるフラグ |
| --- | --- | --- | --- |
| claude | 見分けられます | 取れます | `--dangerously-skip-permissions` |
| codex | 見分けられます | 取れます | `--dangerously-bypass-approvals-and-sandbox` |
| copilot | 見分けられません (確認画面でも `working` のままです) | 取れません (`null` になります) | `--yolo` |

copilot を確認画面で止めたくないときは、`yolo` か AutoYes を使ってください。

## Webhook

登録した通知先へ、次の 3 つのときに JSON を POST します。

- エージェントが返事を書き終えたとき (`event: "state"`, `state: "done"`)
- エージェントが選択肢を出したとき (`event: "choices"`)
- AutoYes が選択肢に答えたとき (`event: "auto_yes"`)

```json
{"event": "state", "tab": {"id": 3, "name": "mytool-1", "owner": "mytool", "state": "done", ...}, "state": "done", "answer": "...", "seq": 5}
{"event": "choices", "tab": {...}, "question": {"tool": "Bash", "input": {...}}, "options": [{"n": 1, "label": "Yes", "selected": true, "numbered": true}]}
{"event": "auto_yes", "tab": {...}, "label": "Yes", "options": [...]}
```

- 通知先には 2 種類あります。owner を付けて登録した通知先には、その owner のタブの通知だけが届きます。owner を付けずに登録した通知先 (全体) には、すべてのタブの通知が届きます。全体の通知先は API でだけ扱え、Config には出ません。
- 通知は 1 回だけ送ります。失敗しても送り直しません。結果は `webhook.log` に残ります。
- 通知先の URL は、プログラムごとに 1 つに決めてください。指示のたびに違う URL を登録すると通知先が溜まっていき、それ以降の通知がそのすべてに届きます。
- owner は通知を振り分けるための名前で、権限を分けるものではありません。どのプログラムも、好きな owner を名乗れます。

## 気をつけること

- `yolo` と AutoYes は、サーバーを起動し直しても続きます。
- API や MCP を使える相手は、そのユーザーの権限で好きなコマンドを実行できます (`open_tab` の `run`、`yolo`、ほかのタブへの `send` など)。SSH の鍵や MCP の設定は、信頼できる相手にだけ渡してください。
- タブの中のエージェントも、環境変数 `MDECK_SOCK` を使って同じ API を呼べます。

## API

`~/.mdeck/mdeck.sock` に JSON を 1 行送ると、1 行の JSON が返って接続が閉じます (1 回の接続で 1 リクエストです)。`mdeck api '<json>'` でも呼べます。この場合、返事の `ok` が `true` でなければ、終了コード 1 で終わります。

待つコマンドの既定の上限は、`open_tab` が 1 分、ほかが 10 分 (最大 30 分) です。

### コマンド

`tab` には、タブの番号か名前を渡します。数字だけの文字列は番号として扱います。

| コマンド | 引数 | できること |
| --- | --- | --- |
| `list_tabs` | | タブの一覧 (`tabs`) を返します |
| `open_tab` | `cwd`, `name?`, `owner?`, `agent?`, `args?`, `run?`, `yolo?`, `auto_yes?`, `wait?`, `timeout_ms?` | タブを開きます。`agent` には `claude` / `codex` / `copilot` を渡します。`agent` も `run` も無ければ claude を起動します。シェルだけのタブを開くときは、`run` に `""` か実行するコマンドを渡します (`agent` と `run` は片方だけ渡せます)。エージェントを起動したときは、既定で準備ができるまで待ちます。時間切れになってもタブはできているので、開き直さずに `wait` や `choose` を続けてください |
| `close_tab` | `tab` | タブを閉じます。中のエージェントも終わります |
| `close_tabs` | `owner` | その owner のタブを、まとめて閉じます |
| `update_tab` | `tab`, `name?`, `owner?`, `auto_yes?` | 渡したものだけを変えます。`name` と `owner` は `""` を渡すと外れます |
| `show_tab` | `tab` | 人が見ている画面に出すタブを切り替えます |
| `send` | `tab`, `text`, `wait?`, `request_id?`, `force?`, `timeout_ms?` | 文字を貼り付けて Enter を押します。既定では、返事か選択肢が出るまで待ちます。作業中のタブには送らず、`busy` を返します (`force: true` を付けると送ります)。同じ `request_id` で送り直したときは打ち込まず、最初の結果に `duplicate: true` を付けて返します |
| `press_keys` | `tab`, `keys` | キーをそのまま送ります (例: `"\u001b"` で Esc)。作業中かどうかの扱いは変えません |
| `choose` | `tab`, `n`, `wait?`, `timeout_ms?` | 選択肢を番号で選びます。既定では、次に止まるまで待ちます |
| `wait` | `tab`, `timeout_ms?` | 作業が終わる (`done` か `waiting` になる) まで待ちます |
| `get_answer` | `tab` | 待たずに、今の返事を返します |
| `get_choices` | `tab` | 出ている選択肢と、確認を出したツールを返します |
| `get_screen` | `tab` | 画面の各行 (`lines`) を返します |
| `list_webhooks` | `owner?` | 通知先の一覧を返します。ヘッダーは名前だけを返します |
| `add_webhook` | `url`, `owner?`, `headers?` | 通知先を登録します。同じ URL がすでにあれば、ヘッダーごと置き換えます |
| `remove_webhook` | `url`, `owner?` | 通知先を消します |
| `test_webhooks` | `owner?` | 通知先へ試しの通知をすぐに送り、1 件ずつ `{url, http, error}` を返します (URL の末尾は伏せます) |

このほかに内部で使うコマンドがあります。MCP には出していません: `attach` (画面)、`subscribe` (イベントを 1 行ずつ流し続けます)、`hook` (エージェントのフック)、`restart` (サーバーを起動し直します。使い方は `docs/operations.md` にあります)。

### 返事の形

成功すると `{"ok": true, ...}` を、失敗すると `{"ok": false, "error": {"code": "...", "message": "..."}}` を返します。

**タブ** (`tab` と、`tabs` の各要素):

```json
{"id": 3, "name": "mytool-1", "title": "...", "owner": "mytool", "cwd": "/path", "agent": "claude", "state": "done",
 "active": false, "last_used": 1794376000, "auto_yes": false, "yolo": false, "unread": true}
```

**待つコマンド** (`open_tab` / `send` / `choose` / `wait` / `get_answer`) は、どれも同じ形で返します。確認画面で止まったときも、時間切れのときも `ok: true` です。

```json
{"ok": true, "tab": {...}, "state": "done", "seq": 5, "answer": "直しました。", "timed_out": false, "expect_seq": 5}
{"ok": true, "tab": {...}, "state": "waiting", "seq": 4, "answer": null, "timed_out": false,
 "question": {"tool": "Bash", "input": {"command": "rm -rf build"}},
 "options": [{"n": 1, "label": "Yes", "selected": true, "numbered": true}, {"n": 2, "label": "No", "selected": false, "numbered": true}]}
{"ok": true, "tab": {...}, "state": "working", "seq": 4, "answer": null, "timed_out": true}
```

- 作業中と起動待ちのときは、`answer` が `null` になります。
- `expect_seq` を返すのは `send` だけです。
- `open_tab` が時間切れになり、フックも入っていないときは、`hint` を付けて返します。

**エラーの `code`**:

| code | 意味 |
| --- | --- |
| `invalid` | 引数が足りないか、正しくありません |
| `no_such_tab` | そのタブはありません |
| `busy` | タブが作業中です (`send`) |
| `not_ready` | エージェントがまだ起動していません (`send`)。確認画面で止まっているときは、`get_choices` を見て `choose` で答えてください |
| `not_found` | 選択肢や通知先が見つかりません |
| `failed` | 書き込み・保存・通知の送信などに失敗しました |
| `unknown_cmd` | 知らないコマンドです |

タブ名と owner は、空白を含まない 1〜32 文字にしてください。数字だけの名前は使えません。同じタブ名は 2 つ付けられません。

### 例

```bash
mdeck api '{"cmd":"add_webhook","owner":"mytool","url":"https://example.com/mytool","headers":{"Authorization":"Bearer ..."}}'
mdeck api '{"cmd":"open_tab","owner":"mytool","name":"mytool-1","cwd":"/path/to/repo","agent":"claude"}'
mdeck api '{"cmd":"send","tab":"mytool-1","text":"テストを直して"}'
```

## MCP

MCP (`mdeck mcp`) のツールの名前と引数は、API のコマンドの名前と引数と同じです (`cmd` は要りません)。

MCP では、待つツールの既定の上限を短くしています。`open_tab` は 1 分、`send` `choose` `wait` は 2 分です。時間切れになったら、`wait` を呼び直してください。

MCP は、1 つの接続の呼び出しを順番に処理します。待っている間は、ほかの呼び出しも返りません。複数のタブを同時に待ちたいときは、`send` に `wait: false` を付けて、返事を webhook で受け取ってください。

### 登録のしかた

同じ Mac の MCP クライアントには、`mdeck mcp` をそのまま登録します (Intel Mac では、パスが `/usr/local/bin/mdeck` になります)。

```json
{"mcpServers": {"mdeck": {"command": "/opt/homebrew/bin/mdeck", "args": ["mcp"]}}}
```

別のマシンから使うときは、forced command を付けた SSH 鍵で起動させます。`~/.ssh/authorized_keys` に次の行を書いてください。この鍵を持つ相手は、そのユーザーの権限で好きなコマンドを実行できます (上の「気をつけること」を見てください)。

```
restrict,command="/opt/homebrew/bin/mdeck mcp" ssh-ed25519 <PUBLIC_KEY> mdeck-mcp
```

つなぐ側では、`ssh -i <鍵> -o BatchMode=yes <user>@<host>` を stdio のコマンドとして登録します。`-t` は付けないでください。
