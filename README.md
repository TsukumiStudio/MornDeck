# MornDeck

サイドバー付きのターミナル多重化ソフト。コマンドは `mdeck`。

<!-- TODO: サイドバーに claude / codex のタブが並んだスクリーンショット -->

- タブ (セッション) をサイドバーに並べ、claude / codex の状態 (考え中・入力待ち・完了) を表示する
- 端末を閉じてもタブは残り、ssh 先で `mdeck` を起動すると同じ画面に入る
- サーバーが落ちても、タブと claude / codex の会話を次の起動で戻す
- ソケット API と MCP サーバーで、Bot からタブを開いて指示を送り、回答や選択肢を受け取れる。完了は webhook で通知する

## 動作要件

- macOS (Apple Silicon / Intel)
- Homebrew
- 状態表示を使うなら claude (Claude Code) か codex

## インストール

```bash
brew install --cask tsukumistudio/tap/morndeck
mdeck
```

初回の `mdeck` がバックグラウンドでサーバーを起動し、画面につなぐ。

claude / codex の状態表示には、`F12` → `m` (コンフィグ) でフックを導入する。導入先は `~/.claude/settings.json` と `~/.codex/hooks.json` で、既存の設定は残す。MornDeck の外で動く claude / codex には影響しない。導入後に起動した claude / codex から反映される。

### 更新

コンフィグの「アップデートを確認」から更新するか、次を実行する。

```bash
HOMEBREW_NO_AUTOREMOVE=1 brew upgrade --cask morndeck
pkill -f 'mdeck server'   # サーバーを再起動して反映する
mdeck
```

サーバーの再起動で、動いている claude / codex は終了する。次の起動で保存した会話 ID から再開する。

### アンインストール

```bash
pkill -f 'mdeck server'
HOMEBREW_NO_AUTOREMOVE=1 brew uninstall --cask morndeck
rm -rf ~/.mdeck
```

フックは残るので、`~/.claude/settings.json` と `~/.codex/hooks.json` から `mdeck hook` を含む項目を消す。

## 使い方

| コマンド | 動作 |
| --- | --- |
| `mdeck` | 画面につなぐ。サーバーが無ければ起動する |
| `mdeck api '<json>'` | API を 1 回叩く (下の「API」) |
| `mdeck mcp` | stdio の MCP サーバー (下の「MCP」) |
| `mdeck server` | サーバーを前面で起動する (通常は `mdeck` が起動する) |
| `mdeck --version` | 版を表示 |

`F12` → `d` で離脱しても、タブはサーバーに残る。もう一度 `mdeck` でつなぐ。

## サイドバー操作 (`F12`。Mac の内蔵キーボードでは fn+F12)

サイドバーは上から「MornDeck」「バージョン」「⚙ コンフィグ」、その下にタブ (セッション) が並ぶ。`F12` でカーソルが出る (最初は表示中のタブ)。

| キー | 動作 |
| --- | --- |
| `↑` `↓` / `k` `j` | カーソル移動 |
| `Enter` | コンフィグかセッションの設定を開く |
| `Esc` / `F12` | やめる |

**コンフィグ**: claude 用・codex 用のフック (入っている版を表示し、Enter でその分だけ導入・更新)、アップデートの確認と更新 (新しいタブで `brew upgrade`)、サイドバーの幅 (`←` / `→`)、キー設定。

**セッションの設定**: このタブを表示 / 名前 / owner / owner の webhook (一覧・Enter で削除・追加) / タブを閉じる (Enter 2 回)。owner が無いタブには webhook の項目は出ない。

ポップアップの `Esc` はサイドバーに戻り、`F12` は全部閉じる。`F12` は MornDeck が受け取り、タブの中へは送らない。

サイドバー操作中は次のキーもそのまま使える。既定の割り当てで、コンフィグの「キー設定」で変えられ、`~/.mdeck/keys.json` に保存される (`j` `k` は移動に使うので割り当てられない。割り当て待ちで押すのはキーだけで、F12 は無視される)。

| キー | 動作 |
| --- | --- |
| `c` | 新規タブ |
| `n` / `p` | 次 / 前のタブ (サイドバーのクリックも可) |
| `x` | タブを閉じる |
| `r` | 表示中のタブの名前を変える |
| `m` | コンフィグ |
| `d` | 離脱 (タブは残る) |

サイドバーの幅は境界のドラッグかコンフィグで変える。

サーバーが落ちても、次の起動でタブの並び・番号・名前・作業ディレクトリが戻る。claude / codex は保存した会話 ID で再開する (フック導入時)。

マウスホイールでスクロールバック。ドラッグで選択した文字はクリップボードに入る (ssh 越しは端末が OSC 52 に対応している場合)。

## 設定とファイル

すべて `~/.mdeck/` に置く。環境変数 `MDECK_SOCK` でソケットの場所を変えると、下のファイルもその隣に置く。

| ファイル | 内容 |
| --- | --- |
| `mdeck.sock` | API のソケット |
| `tabs.json` | タブの並び・名前・作業ディレクトリ・会話 ID・サイドバーの幅 |
| `keys.json` | キー設定 |
| `webhooks.json` / `owner_webhooks.json` | 全体 / owner ごとの webhook |
| `mdeck.log` | サーバーのエラー出力 |
| `hook.log` | サーバーに届かなかったフック |
| `mcp.log` | MCP の呼び出し記録 |

タブの中では `MDECK_TAB` (タブ番号) と `MDECK_SOCK` が設定される。

## API

`~/.mdeck/mdeck.sock` に JSON を 1 行送ると 1 行返る。`mdeck api '<json>'` でも叩ける。`tab` はタブ番号 (数値) か名前 (文字列)。API からの操作では表示中のタブは切り替わらない (`select` を除く)。

| cmd | 引数 | 内容 |
| --- | --- | --- |
| `list` | | タブ一覧 (`id`, `name`, `title`, `owner`, `cwd`, `agent`, `state`, `active`, `last_used` (unix 秒)) |
| `new` | `cwd`, `name?`, `owner?`, `agent?`, `args?`, `run?`, `wait?`, `timeout_ms?`, `webhook?` | `cwd` でタブを作る。`agent` (`claude` / `codex`) を渡すと起動して準備完了まで待ち、確認画面で止まったら `state: "blocked"` と `options` を返す。起動するまでのタブには `choose` と `raw` の送信しか通さない。`owner` は開いた Bot の名前 |
| `rename` | `tab`, `name` | 名前を付ける。空で外す |
| `send` | `tab`, `text`, `request_id?`, `enter?`, `raw?`, `wait?`, `force?`, `timeout_ms?`, `webhook?` | 入力を送る。送った時点で `working` になり、`wait` なら `done` / `waiting` まで待って回答か選択肢を返す。作業中のタブは `force` 無しだと `busy` で断る。待たないときは、この依頼の回答に付く `seq` を `expect_seq` で返す。同じ `request_id` の送り直しは打ち込まず、最初の受付結果を `duplicate: true` 付きで返す。`raw` は貼り付け扱いせずキー列として送る |
| `choose` | `tab`, `n`, `wait?`, `timeout_ms?` | 選択肢を番号で選ぶ。番号の無い選択肢は矢印キーで動かして Enter |
| `wait` | `tab`, `timeout_ms?` | 作業が終わるまで待つ。時間切れは `ok: true` / `state: "working"` / `timeout: true` で返る |
| `answer` | `tab` | 最新の回答。作業中は `text` が `null`。`seq` は回答ごとに増える |
| `choices` | `tab` | 提示中の選択肢 (`options`: `n`, `label`, `selected`) と、フックが渡した質問内容 (`question`) |
| `read` | `tab` | 表示中の画面テキスト |
| `select` | `tab` | 表示切替 |
| `close` | `tab` か `owner` | 終了。`owner` ならその Bot のタブをまとめて閉じる |
| `webhooks` | `owner?`, `set?`, `add?`, `remove?` | `done` と選択肢の提示 (`choices`) を POST する先。下の「webhook」参照 |
| `subscribe` | | 以降 `state` / `choices` / `tab_new` / `tab_closed` イベントを 1 行ずつ流し続ける |

`timeout_ms` の既定は `new` が 60 秒、待つそれ以外が 10 分 (上限 30 分)。

名前は重複不可で、空白なしの 1〜32 文字、数字だけは不可。画面からは F12 → セッションの設定 (か F12 → r) で付ける。

`state` は `idle` `working` `waiting` `done`、起動待ちの `starting` / `blocked` (確認画面)、または `null` (エージェント未起動)。

```bash
mdeck api '{"cmd":"new","name":"grok","owner":"grok-bot","cwd":"/path/to/repo","agent":"claude"}'
mdeck api '{"cmd":"send","tab":"grok","text":"テストを直して","wait":true}'
```

### webhook

`done` と選択肢の提示 (`choices`) を JSON で POST する。送り先は 2 種類あり、両方に届く。

- **全体** (`owner` 無し): 全タブの通知が届く。`~/.mdeck/webhooks.json`
- **owner ごと** (`owner` 付き): `new` で同じ `owner` を付けて開いたタブの通知だけが届く。`~/.mdeck/owner_webhooks.json`

`owner` を付けた操作はその owner の一覧だけを読み書きし、全体や他の owner の登録には触れない。Bot は自分の `owner` を付けて登録・削除する。

| 引数 | 内容 |
| --- | --- |
| `add` | 1 件追加 (同じ URL は置き換え)。URL か `{"url", "headers"}` |
| `remove` | URL を 1 件削除 |
| `set` | 一覧を丸ごと置き換える |
| (なし) | 一覧 (ヘッダーは名前だけ) |

通知の本文は `subscribe` のイベントと同じ形に `owner` (タブを開いた Bot、無ければ `null`) を足したもの。

```json
{"event": "state", "tab": 3, "name": "grok", "agent": "claude", "state": "done", "answer": "...", "seq": 5, "owner": "grok-bot"}
{"event": "choices", "tab": 3, "name": "grok", "question": "...", "options": [{"n": 1, "label": "Yes", "selected": true, "numbered": true}], "owner": "grok-bot"}
```

Bot は `new` / `send` (MCP の `new_tab` / `send`) に `webhook` を付けるだけでもよい。そのタブの owner の一覧に登録し、同じ URL・同じヘッダーなら何もしない (毎回渡してよい)。ヘッダーだけ違えば上書きする。owner の無いタブではエラーになり、何もしない。

```json
{"name": "new_tab", "arguments": {"cwd": "/path", "name": "kimekyawa", "owner": "kimekyawa",
  "webhook": {"url": "https://example.com/routine/kimekyawa", "headers": {"Authorization": "Bearer ..."}}}}
```

```bash
mdeck api '{"cmd":"webhooks","add":"https://example.com/all"}'                       # 全体
mdeck api '{"cmd":"webhooks","owner":"grok-bot","add":{"url":"https://example.com/grok","headers":{"X-Key":"..."}}}'
mdeck api '{"cmd":"webhooks","owner":"grok-bot"}'                                    # grok-bot の一覧
mdeck api '{"cmd":"webhooks","owner":"grok-bot","remove":"https://example.com/grok"}'
```

## MCP

`mdeck mcp` が stdio の MCP サーバーになる (ツール: `list_tabs` `new_tab` `send` `select_choice` `wait` `answer` `choices` `read` `rename_tab` `select_tab` `webhooks` `close_tab`)。引数は API と同じ (`cmd` 以外)。MCP だけの既定値: `new_tab` は `agent` も `run` も無ければ `claude` を起動 (シェルだけのタブは `"run": ""`)、`send` と `select_choice` は `wait: true`。`subscribe` は無い (webhooks を使う)。待つツールの既定の上限は 2 分 (`new_tab` は 1 分) で、時間切れなら `wait` を呼び直す。

同じ Mac の MCP クライアントには `mdeck mcp` をそのまま登録する (Intel Mac のパスは `/usr/local/bin/mdeck`)。

```json
{"mcpServers": {"mdeck": {"command": "/opt/homebrew/bin/mdeck", "args": ["mcp"]}}}
```

別のマシンからは forced command の SSH 鍵で起動させる。`~/.ssh/authorized_keys` に:

```
restrict,command="/opt/homebrew/bin/mdeck mcp" ssh-ed25519 <PUBLIC_KEY> mdeck-mcp
```

接続側は `ssh -i <鍵> -o BatchMode=yes <user>@<host>` を stdio のコマンドとして登録する (`-t` は付けない)。

## トラブルシュート

| 症状 | 対処 |
| --- | --- |
| `F12` が効かない | Mac の内蔵キーボードは fn+F12。端末アプリやシステムが F12 を横取りしていないか確認する |
| サイドバーに claude / codex の状態が出ない | コンフィグでフックが「最新」か確認し、claude / codex を起動し直す。届かなかった呼び出しは `~/.mdeck/hook.log` に残る |
| 更新したのに版が変わらない | サーバーが古いまま。`pkill -f 'mdeck server'` してから `mdeck` |
| `mdeck` がつながらない | `~/.mdeck/mdeck.log` を見る |
| MCP が応答しない | `~/.mdeck/mcp.log` で呼び出しが届いているか見る。ssh 越しは `-t` を付けない |
| `new` が `blocked` で返る | claude / codex が確認画面 (フォルダの信頼など) で止まっている。`choices` を見て `choose` で答える |

## ライセンス

<!-- TODO: ライセンスを決めて LICENSE を置く -->
未定。
