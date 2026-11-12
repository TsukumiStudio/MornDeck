# MornDeck

[English](README.md) | 日本語

MornDeck は、ターミナル内で動作するタブ型のターミナルです。サイドバーに AI エージェントの状態を出します。macOS 11 以降 (Apple Silicon / Intel) で動きます。

![MornDeck の画面](docs/screenshot.svg)

## 特徴

- ひとつの端末の中にタブをいくつも作り、サイドバーで切り替えられます。
- 端末を閉じてもプロセスは動き続けます。`mdeck` を開き直せば、ssh からでも同じ画面に戻れます。
- AI 向けの API と MCP があり、タブを開く・文字を送る・画面を読む・返事を待つ、が外からできます。
- Claude Code・Codex CLI・GitHub Copilot CLI (以下 claude・codex・copilot) の状態 (作業中・入力待ち・完了) をサイドバーに出します。完了は通知音・ポップアップ・Webhook で知らせられます (どれも既定はオフ)。

## インストール

```bash
brew install --cask tsukumistudio/tap/morndeck
```

`mdeck` という名前のツールはほかにもあります (crates.io・npm・PyPI の Markdown スライドツールなど)。MornDeck とは別物なので、tap 名 (`tsukumistudio/tap`) を付けて入れてください。同じ名前のコマンドを両方入れることはできません。

更新は設定の「更新を確認」で行います。サーバーも新しい版で起動し直します。`brew upgrade --cask morndeck` だけで更新したときは、`mdeck kill` をしてから `mdeck` を開き直すまで、古い版のサーバーが動き続けます。

cask は入れたあと `mdeck` の quarantine 属性を外します。公証済みのバイナリですが、属性が残ると更新のたびに初回確認のダイアログで止まり、ssh 越しでは気づけないためです。

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

設定では、更新・フックの導入と解除・サイドバーの幅・使用量のゲージ・通知音とポップアップ・通知先 (Webhook) を変えられます。

`mdeck kill` でサーバーとすべてのタブを止めます。タブの一覧は保存され、次に `mdeck` を開くと戻ります。

## フック

状態は各エージェントのフックで受け取ります。設定で導入すると、次のファイルを書き換えます。

| エージェント | ファイル |
| --- | --- |
| claude | `~/.claude/settings.json` |
| codex | `~/.codex/hooks.json` |
| copilot | `~/.copilot/hooks/mdeck.json` (MornDeck 専用) |

- 足すのは MornDeck のフックだけで、ほかの設定は残します。ファイルは整形し直されます。
- 最初に書き換える前の中身を `<ファイル名>.mdeck-bak` に残します。
- フックの形式が変わると、サーバーの起動時に導入済みのフックを新しい形式に書き換えます。
- フックは MornDeck のタブの外では何もしません。
- 外すには、設定のフックの行で `Enter` を押すか、`mdeck uninstall` を実行します。

## 使用量のゲージ

サイドバーの下に、claude と codex の週の枠、copilot の月の枠の使用量を出します。このために次を読みます。

| 対象 | 読むもの | 送り先 |
| --- | --- | --- |
| claude | キーチェーンの `Claude Code-credentials` にある OAuth トークン | 5 分おきに `api.anthropic.com/api/oauth/usage` |
| copilot | 環境変数 `COPILOT_GITHUB_TOKEN` / `GH_TOKEN` / `GITHUB_TOKEN`、キーチェーンの `copilot-cli`、`~/.copilot/config.json` のいずれかのトークン | 5 分おきに `api.github.com/copilot_internal/user` |
| codex | `~/.codex/sessions` の記録 | 送らない |

- トークンは上の送り先にだけ送り、保存も記録もしません。
- どちらの API も各社が公開している仕様ではないので、予告なく使えなくなることがあります。
- 設定の「使用量のゲージ」をオフにすると、トークンも記録も読まず、API も呼びません。

MornDeck が自分から通信するのは、この 2 つと、設定の「更新を確認」(github.com) と、登録した通知先だけです。利用状況は送りません。

## AutoYes

タブのメニューで AutoYes をオンにすると、Yes / No の確認に自動で Yes と答えます。

- ファイルの削除や `git push` の確認にも Yes と答えます。任せてよい作業のタブだけでオンにしてください。
- 選択肢がすべて英語の `Yes` / `No` で始まる確認だけに答えます。
- claude のフックを入れていれば、質問と計画の承認には答えません。フックが無いときと codex・copilot では見分けられないので、選択肢がすべて `Yes` / `No` で始まるメニューには答えます。

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
| `get_answer` | `tab`, `timeout_ms?` | 今の返事と選択肢。`timeout_ms` を渡すと、返事か選択肢が出るまで待つ |
| `get_screen` | `tab` | 画面の文字 |
| `subscribe` | | 起きたこと (タブの開閉・状態の変化・選択肢・AutoYes) を 1 行 1 JSON で流し続ける |
| `list_webhooks` | `owner?` | 通知先の一覧 |
| `add_webhook` | `url`, `owner?`, `headers?` | 通知先を足す |
| `remove_webhook` | `url`, `owner?` | 通知先を消す |
| `test_webhooks` | `owner?` | 通知先へ試しに送る |

`yolo` を `true` にすると、エージェントを確認もサンドボックスも無しで起動します (`claude --dangerously-skip-permissions` など)。エージェントが何でも実行できるので、壊れてもよい環境だけで使ってください。

待つコマンドは次の形で返します。

```json
{"ok": true, "tab": {...}, "state": "done", "seq": 3, "answer": "直しました。", "timed_out": false}
```

| キー | 中身 |
| --- | --- |
| `state` | `done` なら `answer` が返事。`waiting` なら `question` と `options` を見て `choose` で答える。起動時の確認で止まると `blocked`。ほかは `starting`・`idle`・`working` と、エージェントのいないタブの `null` |
| `seq` | 返事ごとに増える番号。`send` は、その依頼の返事の番号を `expect_seq` で返す |
| `timed_out` | `true` なら、`get_answer` に `timeout_ms` を付けて待ち直す |

失敗すると `{"ok": false, "error": {"code": "...", "message": "..."}}` を返します。`code` は `invalid` (引数が違う)・`no_such_tab`・`busy` (作業中)・`not_ready` (起動中)・`not_found`・`failed`・`unknown_cmd` のどれかです。

0.x の間は、API と MCP の引数や戻り値を変えることがあります。変えたときは [CHANGELOG](CHANGELOG.md) に書きます。

## MCP

ツールは API のコマンドと同じです (`subscribe` を除く)。

```json
{"mcpServers": {"mdeck": {"command": "/opt/homebrew/bin/mdeck", "args": ["mcp"]}}}
```

Intel Mac では `/usr/local/bin/mdeck` です。

別のマシンからは ssh 越しに起動します。

```json
{"mcpServers": {"mdeck": {"command": "ssh", "args": ["<user>@<host>", "/opt/homebrew/bin/mdeck", "mcp"]}}}
```

MCP を使える鍵は、その Mac で何でも実行できます ([セキュリティ](#セキュリティ))。その鍵で `mdeck mcp` しか動かせないようにするには、Mac 側の `~/.ssh/authorized_keys` で鍵の前に `command=` を付けます。

```
restrict,command="/opt/homebrew/bin/mdeck mcp" ssh-ed25519 <公開鍵>
```

## Webhook

登録した URL へ、次のときに JSON を POST します。届かなくても送り直しません。

| とき | JSON |
| --- | --- |
| 返事を書き終えた | `{"event": "state", "state": "done", "tab": {...}, "answer": "...", "seq": 3}` |
| 選択肢を出した | `{"event": "choices", "tab": {...}, "question": {...}, "options": [{"n": 1, "label": "Yes", "selected": true}]}` |
| AutoYes が答えた | `{"event": "auto_yes", "tab": {...}, "label": "Yes", "options": [...]}` |

```bash
mdeck api '{"cmd":"add_webhook","owner":"mytool","url":"https://example.com/hook","headers":{"Authorization":"Bearer ..."}}'
```

`owner` を付けると、同じ `owner` のタブの通知だけが届きます。`test_webhooks` は `{"event": "test", "owner": ..., "message": ...}` を送ります。

## セキュリティ

- 待ち受けるのは `~/.mdeck/mdeck.sock` (本人だけが読み書きできる Unix ソケット) だけで、ネットワークのポートは開きません。
- このソケット・API・MCP を使える人は、その Mac であなたとして何でも実行できます (`open_tab` の `run`、`send`、`press_keys`)。ssh 越しに MCP を使わせる鍵は、シェルを渡すのと同じです。
- Webhook は返事の本文を送り先へ送ります。`http://` の送り先には、本文もヘッダーの鍵も暗号化されずに流れます。
- `~/.mdeck/events.log` に、API の呼び出し・返事・フックの内容が残ります (文字列の値はそれぞれ 300 文字で切ります)。`mcp.log` には MCP の接続ごとの `SSH_CONNECTION` (接続元のアドレス) が残ります。通知先のヘッダーの値はログに残さず、URL はホストまでしか残しません。
- 通知先の URL とヘッダーの値は、`~/.mdeck/webhooks.json` と `owner_webhooks.json` に、本人だけが読める平文で保存します。
- サーバーが起動すると (`mdeck api` や `mdeck mcp` が起動したときも)、保存したタブはエージェントの会話を再開します。`yolo` と AutoYes の設定も元のままです。
- 脆弱性の報告は [SECURITY.md](SECURITY.md) を見てください。

## アンインストール

```bash
mdeck uninstall
brew uninstall --cask --zap tsukumistudio/tap/morndeck
```

`mdeck uninstall` はフックを外してサーバーを止めます。`--zap` は `~/.mdeck/` をゴミ箱へ移します。ログと通知先の鍵が入っているので、消し切るにはゴミ箱を空にしてください。フックを入れる前の設定を残した `.mdeck-bak` は消しません。

## ファイル

設定・タブ・ログは `~/.mdeck/` にあります。

## ソースからビルド

Rust が要ります。rustup が `rust-toolchain.toml` の版を入れます。

```bash
cargo build --release   # target/release/mdeck
zsh build.sh            # dist/MornDeck.app (その場かぎりの署名)
```

配布している版は Developer ID で署名し、Apple の公証を受けています。

## ライセンス

[Unlicense](LICENSE)

含んでいるライブラリの許諾文は [THIRD_PARTY_LICENSES](THIRD_PARTY_LICENSES) にあり、`mdeck --licenses` でも読めます。

MornDeck は Anthropic・OpenAI・GitHub とは関係のない非公式のツールです。Claude・Codex・GitHub Copilot は各社の商標です。
