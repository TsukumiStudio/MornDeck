# MornDeck

MornDeck は、ターミナル内で動作するタブ型のターミナルです。

## 特徴

- ひとつの端末の中にタブをいくつも作り、サイドバーで切り替えられます。
- 端末を閉じてもプロセスは動き続けます。`mdeck` を開き直せば、ssh からでも同じ画面に戻れます。
- AI 向けの API と MCP があり、タブを開く・文字を送る・画面を読む・返事を待つ、が外からできます。
- claude・codex・copilot の状態 (作業中・入力待ち・完了) をサイドバーに出し、完了したら通知します。

## インストール

```bash
brew install --cask tsukumistudio/tap/morndeck
```

## はじめかた

1. `mdeck` で開きます。
2. `F12` → `c` で設定を開き、「フック」で使うエージェントを選んで `Enter` を押します。
3. タブの中で `claude` などを起動します。

## 操作

`F12` (fn+F12) でサイドバーを操作します。

| キー | 動き |
| --- | --- |
| `↑` `↓` | タブを選ぶ |
| `Enter` | そのタブに戻る |
| `→` | タブのメニュー (名前・通知先・AutoYes・削除) |
| `c` | 設定 |
| `e` | 画面を閉じる (タブは残る) |
| `F12` | やめる |

新しいタブはサイドバーの「+」から開きます。マウスでもクリック・右クリック・ドラッグで操作できます。

設定では、更新・フックの導入・サイドバーの幅・通知音とポップアップ・通知先 (Webhook) を変えられます。

## AutoYes

タブのメニューで AutoYes をオンにすると、Yes / No の確認に自動で Yes と答えます。質問や計画の承認には答えません。

## API

```bash
mdeck api '{"cmd":"open_tab","name":"work","cwd":"/path/to/repo","agent":"claude"}'
mdeck api '{"cmd":"send","tab":"work","text":"テストを直して"}'
```

`tab` はタブの番号か名前です。`?` の付いた引数は省けます。

| コマンド | 引数 | 動き |
| --- | --- | --- |
| `list_tabs` | | タブの一覧 |
| `open_tab` | `cwd`, `name?`, `owner?`, `agent?`, `args?`, `run?`, `yolo?`, `auto_yes?`, `wait?`, `timeout_ms?` | タブを開く。`agent` は `claude` (既定) / `codex` / `copilot`。`run` を渡すとそのコマンドを動かす |
| `close_tab` | `tab` か `owner` | タブを閉じる。`owner` を渡すと、その owner のタブをまとめて閉じる |
| `update_tab` | `tab`, `name?`, `owner?`, `auto_yes?` | タブの設定を変える |
| `send` | `tab`, `text`, `wait?`, `request_id?`, `force?`, `timeout_ms?` | 文字を送って Enter を押し、返事を待つ |
| `press_keys` | `tab`, `keys` | キーをそのまま送る |
| `choose` | `tab`, `n`, `wait?`, `timeout_ms?` | 選択肢を番号で選ぶ |
| `wait` | `tab`, `timeout_ms?` | 返事か選択肢まで待つ |
| `get_answer` | `tab` | 今の返事と選択肢 |
| `get_screen` | `tab` | 画面の文字 |
| `list_webhooks` | `owner?` | 通知先の一覧 |
| `add_webhook` | `url`, `owner?`, `headers?` | 通知先を足す |
| `remove_webhook` | `url`, `owner?` | 通知先を消す |
| `test_webhooks` | `owner?` | 通知先へ試しに送る |

待つコマンドは次の形で返します。`state` が `done` なら `answer` が返事、`waiting` なら `options` を見て `choose` で答えます。`timed_out` が `true` なら `wait` で待ち直します。

```json
{"ok": true, "tab": {...}, "state": "done", "answer": "直しました。", "timed_out": false}
```

失敗すると `{"ok": false, "error": {"code": "...", "message": "..."}}` を返します。

## MCP

ツールは API のコマンドと同じです。

```json
{"mcpServers": {"mdeck": {"command": "/opt/homebrew/bin/mdeck", "args": ["mcp"]}}}
```

別のマシンからは ssh 越しに起動します。

```json
{"mcpServers": {"mdeck": {"command": "ssh", "args": ["<user>@<host>", "/opt/homebrew/bin/mdeck", "mcp"]}}}
```

その鍵で `mdeck mcp` しか動かせないようにするには、Mac 側の `~/.ssh/authorized_keys` で鍵の前に `command=` を付けます。

```
restrict,command="/opt/homebrew/bin/mdeck mcp" ssh-ed25519 <公開鍵>
```

## Webhook

登録した URL へ、次のときに JSON を POST します。

- 返事を書き終えたとき (`state`)
- 選択肢を出したとき (`choices`)
- AutoYes が答えたとき (`auto_yes`)

```bash
mdeck api '{"cmd":"add_webhook","owner":"mytool","url":"https://example.com/hook","headers":{"Authorization":"Bearer ..."}}'
```

`owner` を付けると、同じ `owner` のタブの通知だけが届きます。

## ファイル

設定・タブ・ログは `~/.mdeck/` にあります。

## ライセンス

[Unlicense](LICENSE)
