# MornDeck

サイドバー付きのターミナル多重化ソフト。ssh 先で `mdeck` を起動しても同じ画面に入る。

```bash
brew install --cask tsukumistudio/tap/morndeck
mdeck
```

claude / codex の状態表示には、メニュー (`F12` `m`) からフックを導入する。メニューでは claude / codex それぞれのフックの状態 (最新 / 旧版 / 未導入 / 対象外)、アップデートの確認と更新 (新しいタブで `brew upgrade`)、サイドバーの幅 (`←` / `→`)、キー設定も扱う。

## キー (`F12` の後に押す。Mac の内蔵キーボードでは fn+F12)

既定の割り当て。メニューの「キー設定」で変えられ、`~/.mdeck/keys.json` に保存される。割り当て待ちで押すのはキーだけ (F12 は押しても無視される)。

| キー | 動作 |
| --- | --- |
| `c` | 新規タブ |
| `n` / `p` | 次 / 前のタブ (サイドバーのクリックも可) |
| `x` | タブを閉じる |
| `r` | タブに名前を付ける |
| `m` | メニュー (フックの状態と導入・更新、アップデート、サイドバーの幅、キー設定) |
| `d` | 離脱 (タブは残る) |
| `F12` | 操作一覧を閉じる |

`F12` は MornDeck が受け取り、タブの中へは送らない。メニューや名前入力を開いているときに押すと閉じる。サイドバーの幅は境界のドラッグかメニューで変える。

サーバーが落ちても、次の起動でタブの並び・番号・名前・作業ディレクトリが戻る。claude / codex は保存した会話 ID で再開する (フック導入時)。

マウスホイールでスクロールバック。ドラッグで選択した文字はクリップボードに入る (ssh 越しは端末が OSC 52 に対応している場合)。

## API

`~/.mdeck/mdeck.sock` に JSON を 1 行送ると 1 行返る。`mdeck api '<json>'` でも叩ける。`tab` はタブ番号 (数値) か名前 (文字列)。API からの操作では表示中のタブは切り替わらない (`select` を除く)。

| cmd | 引数 | 内容 |
| --- | --- | --- |
| `list` | | タブ一覧 (`id`, `name`, `title`, `owner`, `cwd`, `agent`, `state`, `active`, `last_used` (unix 秒)) |
| `new` | `cwd`, `name?`, `owner?`, `agent?`, `args?`, `run?`, `wait?`, `timeout_ms?` | `cwd` でタブを作る。`agent` (`claude` / `codex`) を渡すと起動して準備完了まで待ち、確認画面で止まったら `state: "blocked"` と `options` を返す。起動するまでのタブには `choose` と `raw` の送信しか通さない。`owner` は開いた Bot の名前 |
| `rename` | `tab`, `name` | 名前を付ける。空で外す |
| `send` | `tab`, `text`, `request_id?`, `enter?`, `raw?`, `wait?`, `force?`, `timeout_ms?` | 入力を送る。送った時点で `working` になり、`wait` なら `done` / `waiting` まで待って回答か選択肢を返す。作業中のタブは `force` 無しだと `busy` で断る。待たないときは、この依頼の回答に付く `seq` を `expect_seq` で返す。同じ `request_id` の送り直しは打ち込まず、最初の受付結果を `duplicate: true` 付きで返す。`raw` は貼り付け扱いせずキー列として送る |
| `choose` | `tab`, `n`, `wait?`, `timeout_ms?` | 選択肢を番号で選ぶ。番号の無い選択肢は矢印キーで動かして Enter |
| `wait` | `tab`, `timeout_ms?` | 作業が終わるまで待つ。時間切れは `ok: true` / `state: "working"` / `timeout: true` で返る |
| `answer` | `tab` | 最新の回答。作業中は `text` が `null`。`seq` は回答ごとに増える |
| `choices` | `tab` | 提示中の選択肢 (`options`: `n`, `label`, `selected`) と、フックが渡した質問内容 (`question`) |
| `read` | `tab` | 表示中の画面テキスト |
| `select` | `tab` | 表示切替 |
| `close` | `tab` か `owner` | 終了。`owner` ならその Bot のタブをまとめて閉じる |
| `webhooks` | `set?` | `done` と選択肢の提示 (`choices`) を POST する先の一覧を置き換える。要素は URL か `{"url", "headers"}`。`add` で 1 件追加 (同じ URL は置き換え)、`remove` で URL を 1 件削除。何も渡さなければ一覧 (ヘッダーは名前だけ) |
| `subscribe` | | 以降 `state` / `choices` / `tab_new` / `tab_closed` イベントを 1 行ずつ流し続ける |

名前は重複不可で、空白なしの 1〜32 文字、数字だけは不可。画面からは F12 → r で付ける。

`state` は `idle` `working` `waiting` `done`、起動待ちの `starting` / `blocked` (確認画面)、または `null` (エージェント未起動)。

```bash
mdeck api '{"cmd":"new","name":"grok","cwd":"/path/to/repo","agent":"claude"}'
mdeck api '{"cmd":"send","tab":"grok","text":"テストを直して","wait":true}'
mdeck api '{"cmd":"webhooks","set":["https://example.com/hook"]}'
```

## MCP

`mdeck mcp` が stdio の MCP サーバーになる (ツール: `list_tabs` `new_tab` `send` `select_choice` `wait` `answer` `choices` `read` `rename_tab` `select_tab` `webhooks` `close_tab`)。引数は API と同じ (`cmd` 以外)。MCP だけの既定値: `new_tab` は `agent` も `run` も無ければ `claude` を起動 (シェルだけのタブは `"run": ""`)、`send` と `select_choice` は `wait: true`。`subscribe` は無い (webhooks を使う)。待つツールの既定の上限は 2 分で、時間切れなら `wait` を呼び直す。別のマシンからは forced command の SSH 鍵で起動させる。`~/.ssh/authorized_keys` に:

```
restrict,command="/opt/homebrew/bin/mdeck mcp" ssh-ed25519 <PUBLIC_KEY> mdeck-mcp
```

接続側は `ssh -i <鍵> -o BatchMode=yes <user>@<host>` を stdio のコマンドとして登録する (`-t` は付けない)。

## リリース

`Cargo.toml` の version を上げて `v<version>` タグを push すると、Actions が署名・公証して Release と `tsukumistudio/tap` の Cask を更新する。
