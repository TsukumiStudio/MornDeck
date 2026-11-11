# MornDeck

MornDeck は、ターミナル内で動作するタブ型のターミナルです。ひとつの端末の中にタブをいくつも作り、サイドバーで切り替えて使います。

## 特徴

- ひとつの端末の中にタブをいくつも作れます。タブはサイドバーに並び、キーやマウスで切り替えられます。
- 端末を閉じても、タブの中のプロセスは動いたまま残ります。もう一度 `mdeck` を開けば続きから使えて、ssh で外からつないでも同じ画面に戻れます。
- AI 向けの API と MCP がそろっています。AI や自分で書いたプログラムから、タブを開く・文字を送る・画面を読む・返事を待つ、といった操作ができます。
- claude・codex・copilot をタブで動かすと、考え中か・入力待ちか・返事を書き終えたかをサイドバーに表示し、書き終えたら通知音やポップアップで知らせます。

## インストール

Mac (Apple Silicon / Intel) と Homebrew が必要です。

```bash
brew install --cask tsukumistudio/tap/morndeck
```

## はじめかた

1. 端末で `mdeck` を実行して、MornDeck を開きます。
2. `F12` を押してから `c` を押すと、設定 (Config) が開きます。「フック」の中から使うエージェント (Claude Code・Codex・GitHub Copilot) を選んで `Enter` を押してください。これで、サイドバーにエージェントの状態が出るようになります。
3. タブの中で `claude` などを起動すれば、準備は完了です。

新しいタブを開くときは、`F12` を押してからサイドバーの一番下にある「+」を選びます。

## 操作

`F12` (Mac の内蔵キーボードでは fn+F12) を押すと、サイドバーを操作できるようになります。操作中は画面が黄色い枠で囲まれます。

| キー | できること |
| --- | --- |
| `↑` `↓` / `k` `j` | タブを選びます。選んだタブが画面に出ます |
| `Enter` | 選んだタブに戻って作業を続けます |
| `→` | タブのメニュー (タブ名・通知先・AutoYes・削除) を開きます |
| `c` / `e` | 設定を開きます / 画面を閉じます (タブは動いたまま残ります) |
| `F12` | サイドバーの操作をやめます |

設定とタブのメニューでは、値を `←` / `→` で変え、`Enter` で入力や操作をします。閉じるときは `F12` を押します。

マウスでは、タブをクリックするとそのタブに移り、右クリックするとタブのメニューが開きます。サイドバーの境目をドラッグすると幅が変わり、端末の中をドラッグすると文字をコピーできます。

## 設定

| 項目 | できること |
| --- | --- |
| 更新を確認 | 新しい版があれば、そのまま入れ替えます |
| フック | エージェントのフックを入れます。入れると、サイドバーにエージェントの状態が出ます |
| 見た目 | サイドバーの幅を変えます |
| 通知 | 返事を書き終えたときの通知音とポップアップを選びます。通知先のあるタブでは鳴らさない、という設定もあります |
| 通知先 (webhook) | 通知先を追加・編集・削除します |

通知音とポップアップが出るのは、画面に出ていないタブのときだけです。

## AutoYes

タブのメニューで AutoYes をオンにすると、「実行してよいか」の Yes / No の確認に、自動で Yes と答えます。

- ファイルの削除や push の確認にも Yes と答えます。任せてよい作業のタブでだけオンにしてください。
- 質問 (AskUserQuestion)、計画の承認、Yes / No 以外の選択肢には答えません。
- 答えたことは `~/.mdeck/events.log` と Webhook に残ります。

## API

`mdeck api '<json>'` で、タブを外から操作できます。返事の `ok` が `true` でなければ、終了コード 1 で終わります。ソケット `~/.mdeck/mdeck.sock` に JSON を 1 行送っても同じです。

```bash
mdeck api '{"cmd":"open_tab","name":"work","cwd":"/path/to/repo","agent":"claude"}'
mdeck api '{"cmd":"send","tab":"work","text":"テストを直して"}'
mdeck api '{"cmd":"get_screen","tab":"work"}'
```

`tab` には、タブの番号か名前を渡します。`?` の付いた引数は省けます。

| コマンド | 引数 | できること |
| --- | --- | --- |
| `list_tabs` | | タブの一覧を返します |
| `open_tab` | `cwd`, `name?`, `owner?`, `agent?`, `args?`, `run?`, `yolo?`, `auto_yes?`, `wait?`, `timeout_ms?` | タブを開きます。`agent` は `claude` / `codex` / `copilot` で、既定は claude です。シェルだけのタブは `run` に `""` か実行するコマンドを渡します。エージェントの準備ができるまで待ちます |
| `close_tab` | `tab` | タブを閉じます |
| `close_tabs` | `owner` | その owner のタブをまとめて閉じます |
| `update_tab` | `tab`, `name?`, `owner?`, `auto_yes?` | タブ名・owner・AutoYes を変えます。`""` を渡すと外れます |
| `show_tab` | `tab` | 画面に出すタブを切り替えます |
| `send` | `tab`, `text`, `wait?`, `request_id?`, `force?`, `timeout_ms?` | 文字を送って Enter を押し、返事か選択肢が出るまで待ちます。作業中のタブには送らず `busy` を返します (`force: true` で送ります) |
| `press_keys` | `tab`, `keys` | キーをそのまま送ります (例: `"\u001b"` で Esc) |
| `choose` | `tab`, `n`, `wait?`, `timeout_ms?` | 選択肢を番号で選び、次に止まるまで待ちます |
| `wait` | `tab`, `timeout_ms?` | 返事か選択肢が出るまで待ちます |
| `get_answer` | `tab` | 待たずに、今の返事を返します |
| `get_choices` | `tab` | 出ている選択肢を返します |
| `get_screen` | `tab` | 画面の文字を 1 行ずつ返します |
| `list_webhooks` | `owner?` | 通知先の一覧を返します |
| `add_webhook` | `url`, `owner?`, `headers?` | 通知先を登録します |
| `remove_webhook` | `url`, `owner?` | 通知先を消します |
| `test_webhooks` | `owner?` | 通知先へ試しの通知を送り、結果を返します |

待つコマンド (`open_tab` / `send` / `choose` / `wait` / `get_answer`) は、次の形で返します。

```json
{"ok": true, "tab": {...}, "state": "done", "seq": 5, "answer": "直しました。", "timed_out": false}
```

| state | 意味 |
| --- | --- |
| `starting` / `blocked` | 起動を待っています / 起動時の確認画面で止まっています (`choose` で答えます) |
| `idle` | 最初の指示を待っています |
| `working` | 作業中です。`timed_out: true` なら、`wait` を呼び直してください |
| `waiting` | 選択肢を出して止まっています。`question` と `options` を見て、`choose` で答えてください |
| `done` | 返事を書き終えました。`answer` が返事です |

失敗すると `{"ok": false, "error": {"code": "...", "message": "..."}}` を返します。`code` は `invalid` / `no_such_tab` / `busy` / `not_ready` / `not_found` / `failed` / `unknown_cmd` のどれかです。

copilot は、選択肢を出しても `working` のままで、`answer` も取れません。止めたくないときは `yolo` か AutoYes を使ってください。

## MCP

`mdeck mcp` を MCP サーバーとして登録すると、AI から API と同じ操作ができます。ツールの名前と引数は、API のコマンドと同じです。

```json
{"mcpServers": {"mdeck": {"command": "/opt/homebrew/bin/mdeck", "args": ["mcp"]}}}
```

- 待つツールは、`open_tab` が 1 分、`send` / `choose` / `wait` が 2 分で時間切れになります。続きは `wait` で待ちます。
- 1 つの接続の呼び出しは順番に処理します。複数のタブを同時に待つときは、`send` に `wait: false` を付けて、返事を Webhook で受け取ってください。
- 別のマシンから使うときは、`~/.ssh/authorized_keys` に `restrict,command="/opt/homebrew/bin/mdeck mcp" ssh-ed25519 <公開鍵>` と書き、つなぐ側で `ssh -i <鍵> -o BatchMode=yes <user>@<host>` を登録します。

API と MCP を使える相手は、その Mac で好きなコマンドを実行できます。信頼できる相手にだけ使わせてください。

## Webhook

登録した URL へ、次のときに JSON を POST します。

| event | 送るとき |
| --- | --- |
| `state` | エージェントが返事を書き終えたとき (`answer` に返事が入ります) |
| `choices` | エージェントが「実行してよいか」などの選択肢を出したとき |
| `auto_yes` | AutoYes が選択肢に自動で答えたとき |

```bash
mdeck api '{"cmd":"add_webhook","owner":"mytool","url":"https://example.com/hook","headers":{"Authorization":"Bearer ..."}}'
```

- `owner` を付けて登録すると、同じ `owner` のタブの通知だけが届きます。付けないと、すべてのタブの通知が届きます。
- 通知は 1 回だけ送り、失敗しても送り直しません。結果は `~/.mdeck/webhook.log` に残ります。
- 設定の「通知先」からも登録できます。ヘッダーを付けたいときは API を使ってください。

## ファイル

ファイルはすべて `~/.mdeck/` に置きます。

| ファイル | 中身 |
| --- | --- |
| `tabs.json` | タブの並び・名前・作業ディレクトリ・会話 ID などです |
| `config.json` | 設定です |
| `webhooks.json` / `owner_webhooks.json` | 通知先です |
| `events.log` | API の呼び出し・状態の変化・AutoYes などの記録です |
| `webhook.log` | Webhook を送った結果です |
| `mdeck.log` | サーバーのエラー出力です |

## ライセンス

[Unlicense](LICENSE)
