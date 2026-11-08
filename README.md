# MornDeck

サイドバー付きのターミナル多重化ソフト。端末はサーバープロセスが持ち、全クライアントへ同じ画面を配る。ssh 先で `mdeck` を起動しても同じ画面に入る。

```bash
cargo install --path .
mdeck install-hooks   # ~/.claude/settings.json と ~/.codex/hooks.json に状態通知フックを追記
mdeck                 # サーバーが無ければ起動して接続
```

## キー (`Ctrl-]` の後に押す)

| キー | 動作 |
| --- | --- |
| `c` | 新規タブ |
| `n` / `p` / `1`-`9` | タブ切替 (サイドバーのクリックも可) |
| `x` | タブを閉じる |
| `d` | 離脱 (タブは残る) |
| `Ctrl-]` | `Ctrl-]` を送る |

マウスホイールでスクロールバック。文字選択は端末側のマウス報告回避操作 (iTerm2/Terminal は Option+ドラッグ、Ghostty は Shift+ドラッグ) で行う。

## API

`~/.mdeck/mdeck.sock` に JSON を 1 行送ると 1 行返る。`mdeck api '<json>'` でも叩ける。

| cmd | 引数 | 内容 |
| --- | --- | --- |
| `list` | | タブ一覧 (`id`, `title`, `agent`, `state`, `active`) |
| `new` | `cwd?`, `run?` | タブを作り、`run` をシェルに入力 |
| `send` | `tab`, `text`, `enter?`(既定 true), `raw?` | 入力を送る。`raw` は貼り付け扱いせずキー列として送り、Enter も付けない |
| `read` | `tab` | 表示中の画面テキスト |
| `select` / `close` | `tab` | 表示切替 / 終了 |
| `subscribe` | | 以降 `state` / `tab_new` / `tab_closed` イベントを 1 行ずつ流し続ける |

`state` は `idle` `working` `waiting` `done` か `null` (エージェント未起動)。

```bash
mdeck api '{"cmd":"new","cwd":"/path/to/repo","run":"claude"}'
mdeck api '{"cmd":"send","tab":2,"text":"テストを直して"}'
mdeck api '{"cmd":"subscribe"}'
```
