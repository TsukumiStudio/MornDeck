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

claude / codex の状態表示には、サイドバーの ⚙ Config でフックを導入する。導入先は `~/.claude/settings.json` と `~/.codex/hooks.json` で、既存の設定は残す。MornDeck の外で動く claude / codex には影響しない。導入後に起動した claude / codex から反映される。古い版のフックは、サーバーの起動時に入れ替える。

### 更新

Config の「更新を確認」で新しい版が見つかると「更新を反映」に変わり、もう一度 Enter で新しい版を入れてサーバーを入れ替え、画面をつなぎ直す。タブは戻り、claude / codex は会話を再開する (作業中だったものはそこで止まる)。手で更新するなら次を実行する。

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

サイドバーの Exit で離脱しても、タブはサーバーに残る。もう一度 `mdeck` でつなぐ。

## サイドバー操作 (`F12`。Mac の内蔵キーボードでは fn+F12)

サイドバーは上から「MornDeck vX」(狭いときは 2 行)「⚙ Config」「✕ Exit」、その下にタブ (セッション)、タブの最後尾に「+」が並ぶ。下部には claude / codex の週次の使用量を出す。「Claude +2d12h」は週の経過に対して 2 日 12 時間分使いすぎ (− は余裕)、下のゲージは塗りが使用量、`│` が今週の今の位置。claude はキーチェーンの Claude Code のトークンで使用量 API を 5 分おきに、codex は `~/.codex/sessions` の最新の記録を毎分読む。`F12` でカーソルが出る (最初は表示中のタブ)。操作中は画面全体が黄色い枠で囲まれる。

| キー | 動作 |
| --- | --- |
| `↑` `↓` / `k` `j` | カーソル移動。タブに乗せるとそのタブを表示 |
| `Enter` / `→` | 開く (下の表) |
| `Esc` / `F12` | やめる |

| 項目 | `Enter` / `→` / クリック |
| --- | --- |
| ⚙ Config | Config を開く |
| ✕ Exit | 離脱 (タブは残る) |
| タブ | `Enter` とクリックはそのタブで作業に戻る。`→` と右クリックはそのタブのメニュー |
| + | 新規タブ |

**Config**: claude 用・codex 用のフック (入っている版を表示し、Enter でその分だけ導入・更新)、更新の確認と反映、サイドバーの幅 (`←` / `→`)、完了したら音を鳴らす (システムの Glass)・通知を出す (macOS の通知)・webhook のあるタブは音と通知なし (Enter で切り替え。`~/.mdeck/config.json` に残る)。音と通知は、表示中のタブ以外が完了したときだけ。

完了してまだ画面でも API (`answer` `wait` `read` `choices` `send` `choose`) でも見ていないタブは、サイドバーで「● 未読」と出る。`list` の `unread` でも分かる。

**タブのメニュー**: 名前 / owner / owner の webhook (一覧・`←` / `→` で編集か削除を選んで Enter・追加) / 実行の確認に自動で Yes (Enter で切り替え) / タブを削除する (確認で「削除する」を選ぶ)。owner が無いタブには webhook の項目は出ない。入力中は `←` / `→` でカーソルを動かし、`Enter` で決定、`Esc` / `F12` でやめる。

ポップアップの項目はクリックでも選べる。`Esc` (タブのメニューでは `←` も) はサイドバーに戻り、`F12` は全部閉じる。`F12` は MornDeck が受け取り、タブの中へは送らない。

サイドバーの幅は境界のドラッグか Config で変える。

サーバーが落ちても、次の起動でタブの並び・番号・名前・作業ディレクトリが戻る。claude / codex は保存した会話 ID で再開する (フック導入時)。

マウスホイールでスクロールバック。中のアプリがマウス報告を求めるとき (claude など) はアプリへ渡す。ドラッグで選択した文字はクリップボードに入る。ssh 越しは端末が OSC 52 に対応している場合だけで、Terminal.app は非対応。

## 設定とファイル

すべて `~/.mdeck/` に置く。環境変数 `MDECK_SOCK` でソケットの場所を変えると、下のファイルもその隣に置く。

| ファイル | 内容 |
| --- | --- |
| `mdeck.sock` | API のソケット |
| `tabs.json` | タブの並び・名前・作業ディレクトリ・会話 ID・サイドバーの幅 |
| `webhooks.json` / `owner_webhooks.json` | 全体 / owner ごとの webhook |
| `mdeck.log` | サーバーのエラー出力 |
| `hook.log` | サーバーに届かなかったフック |
| `mcp.log` | MCP の呼び出し記録 |
| `webhook.log` | webhook の送信結果 (HTTP ステータス、curl のエラー) |

タブの中では `MDECK_TAB` (タブ番号) と `MDECK_SOCK` が設定される。

## API

`~/.mdeck/mdeck.sock` に JSON を 1 行送ると 1 行返る。`mdeck api '<json>'` でも叩ける。`tab` はタブ番号 (数値) か名前 (文字列)。API からの操作では表示中のタブは切り替わらない (`select` を除く)。

| cmd | 引数 | 内容 |
| --- | --- | --- |
| `list` | | タブ一覧 (`id`, `name`, `title`, `owner`, `cwd`, `agent`, `state`, `active`, `last_used` (unix 秒)) |
| `new` | `cwd`, `name?`, `owner?`, `agent?`, `args?`, `yolo?`, `auto_yes?`, `run?`, `wait?`, `timeout_ms?`, `webhook?` | `cwd` でタブを作る。`agent` (`claude` / `codex`) を渡すと起動して準備完了まで待ち、確認画面で止まったら `state: "blocked"` と `options` を返す。起動するまでのタブには `choose` と `raw` の送信しか通さない。`owner` は開いた Bot の名前。`yolo: true` は確認もサンドボックスも無しで起動する (claude `--dangerously-skip-permissions`、codex `--dangerously-bypass-approvals-and-sandbox`)。`auto_yes` は下の `auto_yes` と同じ |
| `rename` | `tab`, `name` | 名前を付ける。空で外す |
| `owner` | `tab`, `owner`, `webhook`? | owner を付け替える。空で外す。`webhook` は `send` と同じ |
| `webhook_test` | `owner?` | 登録済みの webhook に `{event: "test", owner, message}` を今すぐ POST し、1 件ずつ `{url (末尾は伏せる), http, error}` を返す。全部 2xx なら `ok: true`。`owner` 無しは全体の一覧 |
| `auto_yes` | `tab`, `on` | 実行の確認 (選択肢が全部 Yes / No で始まる) に最初の Yes を自動で選ぶ。質問 (AskUserQuestion)・計画の承認・他の選択肢は答えない。選んだときは `subscribe` に `auto_yes` が流れる |
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

名前は重複不可で、空白なしの 1〜32 文字、数字だけは不可。画面からはタブのメニューで付ける。

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

`mdeck mcp` が stdio の MCP サーバーになる (ツール: `list_tabs` `new_tab` `send` `select_choice` `wait` `answer` `choices` `read` `test_webhooks` `rename_tab` `set_owner` `set_auto_yes` `select_tab` `webhooks` `close_tab`)。引数は API と同じ (`cmd` 以外)。MCP だけの既定値: `new_tab` は `agent` も `run` も無ければ `claude` を起動 (シェルだけのタブは `"run": ""`)、`send` と `select_choice` は `wait: true`。`subscribe` は無い (webhooks を使う)。待つツールの既定の上限は 2 分 (`new_tab` は 1 分) で、時間切れなら `wait` を呼び直す。

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
| `F12` が効かない | 端末アプリやシステムが F12 を横取りしていないか確認する |
| サイドバーに claude / codex の状態が出ない | Config でフックが「最新」か確認し、claude / codex を起動し直す。届かなかった呼び出しは `~/.mdeck/hook.log` に残る |
| 更新したのに版が変わらない | サーバーが古いまま。`pkill -f 'mdeck server'` してから `mdeck` |
| `mdeck` がつながらない | `~/.mdeck/mdeck.log` を見る |
| MCP が応答しない | `~/.mdeck/mcp.log` で呼び出しが届いているか見る。ssh 越しは `-t` を付けない |
| `new` が `blocked` で返る | claude / codex が確認画面 (フォルダの信頼など) で止まっている。`choices` を見て `choose` で答える |

## ライセンス

[Unlicense](LICENSE)
