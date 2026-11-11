# MornDeck

サイドバー付きのターミナル多重化ソフト。コマンドは `mdeck`。

- タブをサイドバーに並べ、claude / codex / copilot の状態 (考え中・入力待ち・完了) を表示する。
- 端末を閉じてもタブはサーバーに残り、もう一度 `mdeck` を起動すると同じ画面に戻る。ssh 先からもつなげる。
- サーバーが落ちても、次の起動でタブとエージェントの会話を戻す。
- ソケット API と MCP サーバーで、Bot からタブを開いて指示を送り、回答を受け取れる。完了は webhook で知らせる。

## 用語

- **エージェント**: タブの中で動く claude (Claude Code)・codex・copilot (GitHub Copilot CLI)。
- **Bot**: API や MCP で MornDeck を操作する外部のプログラム。
- **owner**: タブの持ち主を表す名前。同じ名前の通知先 (webhook) に、そのタブの完了と選択肢が届く。画面では「通知先」と呼ぶ。

## はじめに

1. 入れて起動する。

   ```bash
   brew install --cask tsukumistudio/tap/morndeck
   mdeck
   ```

2. `F12` → `c` で Config を開き、「フック」の Claude Code / Codex / GitHub Copilot のうち使うものを Enter で導入する。
3. タブの中で `claude` などを起動する。新しいタブは `F12` → 一番下の「+」で開く (ホームのシェルが開く)。

フックは、エージェントの状態表示にも、Bot 連携 (起動待ち・`wait`・回答・webhook) にも必須。Bot だけで使う場合も、一度 `mdeck` を開いて導入する。

## インストールと更新

動作要件は macOS (Apple Silicon / Intel) と Homebrew。

**フックの導入先**:

- `~/.claude/settings.json`
- `~/.codex/hooks.json`
- `~/.copilot/hooks/mdeck.json` (MornDeck 専用のファイル)

既存の設定は残し、MornDeck の外で動くエージェントには影響しない。導入後に起動したエージェントから効く。古い版のフックは、サーバーの起動時に入れ替える。

**更新**: Config の「更新を確認」で新しい版が見つかると「更新を反映」に変わる。もう一度 Enter で新しい版を入れ、サーバーを起動し直して画面をつなぎ直す。手で更新するなら次を実行する。

```bash
HOMEBREW_NO_AUTOREMOVE=1 HOMEBREW_NO_ASK=1 brew upgrade --cask morndeck
pkill -f 'mdeck server'   # 新しい版はサーバーを起動し直してから効く
mdeck
```

サーバーを起動し直すと、動いているエージェントはいったん終わり、保存した会話 ID で再開する。作業中だったものはそこで止まる。

**アンインストール**:

```bash
pkill -f 'mdeck server'
HOMEBREW_NO_AUTOREMOVE=1 brew uninstall --cask morndeck
rm -rf ~/.mdeck
```

フックは残る。`~/.claude/settings.json` と `~/.codex/hooks.json` から `mdeck hook` を含む項目を消し、`~/.copilot/hooks/mdeck.json` を消す。

## 画面と操作

| コマンド | 動作 |
| --- | --- |
| `mdeck` | 画面につなぐ。サーバーが無ければ起動する |
| `mdeck api '<json>'` | API を 1 回呼ぶ (下の「API と MCP」) |
| `mdeck mcp` | stdio の MCP サーバー |
| `mdeck server` | サーバーを前面で起動する (ふつうは `mdeck` が起動する) |
| `mdeck --version` | 版を表示する |

サイドバーは上から「MornDeck vX」(サーバーの版)、「⚙ Config (c)」「✕ Exit (e)」、タブ、「+」、下部に使用量が並ぶ。

### キーボード

`F12` (Mac の内蔵キーボードでは fn+F12) でサイドバーの操作に入る。操作中は画面全体が黄色い枠で囲まれる。`F12` はタブの中へは送らない。

| キー | 動作 |
| --- | --- |
| `↑` `↓` / `k` `j` | 選ぶ。タブを選ぶとそのタブを表示する |
| `Enter` | 決定。タブならそのタブで作業に戻る |
| `→` | タブのメニューを開く (Config・Exit・+ では `Enter` と同じ) |
| `c` / `e` | Config を開く / 画面を閉じる (Exit) |
| `F12` | 閉じる |

ポップアップ (Config・タブのメニュー) では、値は `←` / `→` で変え、`Enter` は入力と操作 (更新・導入・削除・閉じる) だけに使う。閉じるのは一番下の「閉じる (F12)」か `F12`。`←` と `Esc` では閉じない。入力欄では `←` / `→` でカーソルを動かし、`Enter` で決定、`F12` でやめる。

### Config

| 項目 | 内容 |
| --- | --- |
| 更新を確認 | 新しい版を探す。見つかると「更新を反映」に変わる |
| フック | Claude Code / Codex / GitHub Copilot のフックの版。`Enter` で導入・更新 |
| 見た目 › サイドバーの幅 | `←` / `→` |
| 通知 › 完了した時の通知音 | `←` / `→` で「なし」か mac のシステムの音を選ぶ。選ぶと鳴る |
| 通知 › 完了した時のポップアップ | macOS の通知。`←` / `→` でオン・オフ |
| 通知 › 通知先のあるタブは非通知 | 通知先 (webhook) のあるタブでは、音もポップアップも出さない |
| 通知先 (webhook) | 行で `←` / `→` を押して「編集」か「削除」を選び、`Enter`。「+ 通知先を追加」は名前 → URL の順に入力する。ヘッダー (`Authorization` など) は API の `add_webhook` で付ける |

通知音とポップアップは、表示中ではないタブが完了したときだけ出る。設定は `~/.mdeck/config.json` に残る。

### タブのメニュー

| 項目 | 内容 |
| --- | --- |
| タブ名 | `Enter` で入力。空にすると外す |
| 通知先 | `←` / `→` で、登録済みの通知先から選ぶ |
| AutoYes | 実行の確認に自動で Yes と答える (下の「AutoYes」)。`←` / `→` でオン・オフ |
| タブを削除 | 確認で「削除する」を選ぶと、中のエージェントごと終わる |

完了してから誰も見ていないタブは、サイドバーに「● 未読」と出る。画面に表示するか、API で回答・画面・選択肢を読むか、送ると既読になる。

### マウス

- サイドバーとポップアップの項目は、マウスが乗ると反応する (ポップアップでは乗った項目が選ばれる)。
- 左クリック: Config・Exit・+ を実行する。タブはそのタブで作業に戻る。ポップアップの項目は実行し、値の行は次の値にする。
- 右クリック: タブのメニューを開く。ポップアップの値の行は前の値に戻す。
- ポップアップの外を押すと閉じる。ポップアップの上のホイールで項目を上下に動かす。
- サイドバーの境界をドラッグすると幅が変わる。
- 端末の中はドラッグで文字を選び、離すとクリップボードへ入る (サイドバーの文字は入らない)。ホイールでスクロールし、中のアプリがマウスを使うとき (claude など) はアプリへ渡す。ssh 越しのコピーは、端末が OSC 52 に対応している場合だけ使える (Terminal.app は非対応)。

### AutoYes

タブの画面を 0.3 秒おきに見て、カーソル付きの Yes / No だけのメニュー (claude の「Do you want to proceed?」、codex の「Would you like to run the following command?」など) が出たら、最初の Yes を選ぶ。

- 削除や push の確認にも Yes と答える。オンにするのは任せてよい作業だけにする。
- 質問 (AskUserQuestion)・計画の承認・Yes / No 以外の選択肢には答えない。
- 押したことは `events.log` と webhook (`auto_yes`) に残る。答えなかったメニューも `events.log` に残る。
- オンのままサーバーを起動し直しても続く。

### 使用量

サイドバーの下部に、claude と codex の週次、copilot の月次 (プレミアムリクエスト) の使用量を出す。

- 「Claude +2d12h」は、枠の経過に比べて 2 日 12 時間分使いすぎ。`-` は余裕。
- ゲージの塗りが使用量、`│` が枠の中の今の位置。

| エージェント | 読み方 | 間隔 |
| --- | --- | --- |
| claude | キーチェーンの Claude Code のトークンで使用量 API | 5 分 |
| codex | `~/.codex/sessions` の最新の記録 | 1 分 |
| copilot | `copilot login` の認証情報 (キーチェーンの `copilot-cli` か `~/.copilot/config.json`) で GitHub の内部 API | 5 分 |

キーチェーンは初回に macOS の許可ダイアログが出ることがある。copilot のトークンはサーバーの起動中に 1 回だけ読むので、後から `copilot login` したらサーバーを起動し直す。copilot の使用量は内部 API を使うので、プランや API の変更で出なくなることがある。

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

## ファイル

すべて `~/.mdeck/` に置く。環境変数 `MDECK_SOCK` でソケットの場所を変えると、ほかのファイルもその隣に置く。

| ファイル | 内容 |
| --- | --- |
| `mdeck.sock` | API のソケット |
| `tabs.json` | タブの並び・名前・owner・作業ディレクトリ・会話 ID・AutoYes・サイドバーの幅 |
| `config.json` | 通知音・ポップアップ・非通知の設定 |
| `webhooks.json` / `owner_webhooks.json` | 全体 / owner ごとの通知先 |
| `events.log` | 起きたことの記録 (API の呼び出しと結果、フック、状態の変化、AutoYes、webhook、設定の変更、起動し直し)。ヘッダーの値は伏せ、長い文字列は切る。10MB を超えると `events.log.1` へ回す |
| `webhook.log` | webhook の送信結果 (HTTP ステータス、curl のエラー) |
| `hook.log` | サーバーに届かなかったフック |
| `mcp.log` | MCP の呼び出し |
| `mdeck.log` | サーバーのエラー出力 |

タブの中では、環境変数 `MDECK_TAB` (タブ番号) と `MDECK_SOCK` が設定される。

## トラブルシュート

| 症状 | 対処 |
| --- | --- |
| `F12` が効かない | 端末アプリやシステムが F12 を横取りしていないか確かめる |
| サイドバーにエージェントの状態が出ない | Config でフックが「最新」か確かめ、エージェントを起動し直す。届かなかったフックは `hook.log` に残る |
| 更新したのに版が変わらない | サーバーが古いまま。`pkill -f 'mdeck server'` してから `mdeck` |
| `mdeck` がつながらない | `mdeck.log` を見る |
| `open_tab` が `timed_out` で返る | フックが入っていない (`hint` が付く)。Config で導入する |
| `open_tab` が `blocked` で返る | 起動時の確認画面 (フォルダの信頼など)。`options` を見て `choose` で答える |
| `send` が `busy` / `not_ready` | 作業中なら `wait`、起動待ちなら `get_choices` → `choose` |
| webhook が届かない | `test_webhooks` で HTTP ステータスを見る。過去の送信は `webhook.log` |
| AutoYes で止まった | `events.log` の `auto_yes` の行に、押したか・答えなかった理由が残る |
| MCP が応答しない | `mcp.log` で呼び出しが届いているか見る。ssh 越しは `-t` を付けない |

## ライセンス

[Unlicense](LICENSE)
