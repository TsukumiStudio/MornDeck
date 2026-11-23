# MornDeck

[English](README.md) | 日本語

MornDeck は、ターミナル内で動作するタブ型のターミナルです。サイドバーに AI エージェントの状態を出します。macOS 11 以降 (Apple Silicon / Intel) と Linux (x86_64 / aarch64) で動きます。画面とメッセージは英語です。

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

新しい版が出るとサイドバーに出ます。更新は設定の「Check for updates」で行います。サーバーも新しい版で起動し直します。`brew upgrade --cask morndeck` だけで更新したときは、`mdeck restart` を実行するまで古い版のサーバーが動き続けます。どちらの場合もタブは戻り、エージェントは会話を再開します。

### Windows

```powershell
winget install TsukumiStudio.MornDeck
```

GitHub の各リリース (0.5.5 から) にも `MornDeck-<版>-windows-x64.zip` と `MornDeck-<版>-windows-arm64.zip` を置いています。展開して `mdeck.exe` を実行します。`mdeck.exe` は署名済みです (Azure Artifact Signing)。

- Windows 10 バージョン 2004 (ビルド 19041) 以降か Windows 11 (x64・arm64) と、WSL 2 と Linux のディストリが要ります (`wsl --install`)。
- MornDeck の本体は WSL の中で動き、エージェントも WSL の中で動かします。claude・codex・copilot は Windows ではなく Linux のディストリに入れてください。
- `mdeck` は WSL のシェルではなく、PowerShell か Windows Terminal で実行してください。初回 (と更新のあと) は、同じ版の Linux 版 `mdeck` を GitHub のリリースから WSL の中 (`~/.local/bin/mdeck`) へ落とし、署名済みの `mdeck.exe` に埋めた SHA-256 で確かめてから入れます。そのあと、WSL が止まらないよう Windows 側からサーバーを起動して、画面を開きます。窓を閉じても、タブは WSL の中で動き続けます。
- 既定の WSL のディストリを使います。別のものを使うときは `MDECK_WSL_DISTRO` を設定します。ディストリには `curl` が要ります (その取得・webhook・更新の確認・使用量のゲージ)。
- ポップアップは「MornDeck」の名前の Windows の通知で出ます。Windows では完了の音は鳴りません。
- winget で入れた場合: 「Check for updates」は新しい窓で `winget upgrade` を実行します。MornDeck の画面を閉じるよう案内が出たら閉じてください。そのあと MornDeck を開き直します (winget に新しい版があれば新しい版で)。
- zip で入れた場合: MornDeck の画面を閉じて、`mdeck.exe` を[最新のリリース](https://github.com/TsukumiStudio/MornDeck/releases/latest)のものに差し替えます。タブは動き続けます。

### Linux

GitHub の各リリース (0.5.0 から) に、`mdeck-<版>-linux-x86_64.tar.gz` と `mdeck-<版>-linux-aarch64.tar.gz` (それぞれ `.sha256` 付き) を置いています。静的リンクなので、ディストリを問わず動きます (x86_64・aarch64)。

```bash
v=<版>; a=$(uname -m)
curl -fLO "https://github.com/TsukumiStudio/MornDeck/releases/download/v$v/mdeck-$v-linux-$a.tar.gz"
curl -fLO "https://github.com/TsukumiStudio/MornDeck/releases/download/v$v/mdeck-$v-linux-$a.tar.gz.sha256"
sha256sum -c "mdeck-$v-linux-$a.tar.gz.sha256"
tar -xzf "mdeck-$v-linux-$a.tar.gz"
install -D -m 755 "mdeck-$v-linux-$a/mdeck" ~/.local/bin/mdeck   # ~/.local/bin を PATH に入れておく
```

- webhook・更新の確認・使用量のゲージに `curl` が要ります。ポップアップは `notify-send` で出します (Debian・Ubuntu では `libnotify-bin`)。無ければポップアップは出ません。完了の音はありません。
- `mdeck` は、最初に起動する前に置き場所を決めてください。フックはそのパスで呼びます。後で移したときは、`mdeck kill`、`mdeck` の順に実行し、設定の「Hooks」の各行で `Enter` を押します。
- 更新は、新しい tar.gz を落とし、導入と同じ `install -D -m 755 …` を実行してから (サーバーが動いている間は `cp` だと「Text file busy」で失敗します)、`mdeck restart` を実行します (タブは戻ります)。新しい版が出るとサイドバーに出ます。
- Windows では Windows 版を使ってください (上を参照)。WSL の中で tar.gz も使えますが、WSL が止まるとタブも止まります。
- Linux 版は CI の Ubuntu (x86_64・aarch64) で、サーバー・タブ・フック・API・起動し直しを確かめています。ポップアップ、いろいろな端末やデスクトップでの見た目、ほかのディストリでのエージェントは、まだ実機で確かめていません。

## はじめかた

1. `mdeck` で開きます。
2. `F12` → `c` で設定 (画面では「Config」) を開き、「Hooks」で使うエージェントを選んで `Enter` を押します。
3. タブの中で `claude` などを起動します。

## 基本操作

`F12` (fn+F12) でサイドバーのメニューを開きます。`↑` `↓` (または `k` `j`) で選び、`Enter` で決定するのが基本です。開いているものは、もう一度 `F12` で閉じます。

| キー | 動き |
| --- | --- |
| `↑` `↓` | タブを選ぶ (選んでいる間、そのタブが表示される) |
| `Enter` | 選んだタブに戻る。「Config」「Exit」「+」を選んでいれば、それを実行 |
| `c` | 設定 (Config) |
| `e` | 画面を閉じる (Exit。タブとサーバーは動いたまま) |
| `F12` | メニューを閉じる |

`Shift+F12` でサイドバーを隠す・戻すができます。

新しいタブは、タブの一覧の最後の「+」から開きます。マウスでも操作できます (タブのクリックで切り替え、サイドバーの境目のドラッグで幅を変える、タブの中でスクロール)。

設定では、更新・フックの導入と解除・サイドバーの幅・使用量のゲージ・通知音とポップアップ・通知先 (Webhook) を変えられます。「Test sound and popup」は実際にテストの通知を出します。ポップアップは MornDeck の名前で出ます。初回に macOS が MornDeck の通知を許可するか尋ねます (System Settings → Notifications → MornDeck)。ポップアップを押しても何も開きません。Homebrew で入れた MornDeck.app が要ります。ソースからビルドしたものや、macOS が許可しない場合は、代わりにスクリプトエディタの名前で出します。通知先を選ぶと「Edit」「Test」「Delete」が出るので、`←` `→` で選んで `Enter` を押します。「Test」はその通知先だけにテストを送り、「Delete」は消す前にもう一度確かめます。

## タブ操作

> **タブの設定を変えるには、`F12` を押してタブを選び、`→` (右矢印) を押します。** タブを選んでいる間は、その行に `⚙`、画面下の帯に `→ Menu` が出ます。`Enter` はそのタブに戻るだけで、メニュー (「Settings for タブ名」) は `→` で開きます。タブの右クリックでも開きます。

メニューでできること:

| 項目 | 変え方 |
| --- | --- |
| 名前 (Name) | `Enter` → 入力 → `Enter` (空にすると外す) |
| Webhook (`owner`) | `←` `→` |
| AutoYes | `←` `→` (AutoYes のタブは、サイドバーで名前の前に `»` が付く) |
| 上へ・下へ (Move up / Move down) | `Enter` か、タブをドラッグ |
| タブを削除 (Delete tab…) | `Enter` → 「Delete」を選ぶ |

`mdeck kill` でサーバーとすべてのタブを止めます。タブの一覧は保存され、次に `mdeck` を開くと戻ります。`mdeck restart` はこの 2 つを続けて行います。

## フック

状態は各エージェントのフックで受け取ります。設定 (`F12` → `c` → Hooks で各エージェントの行の `Enter`) で入れます。claude はコマンドの `mdeck hooks install` でも入れられます。

| エージェント | 入れる場所 |
| --- | --- |
| claude | Claude Code のプラグイン `mdeck@mdeck` (`~/.mdeck/claude-plugin` から入れる) |
| codex | `~/.codex/hooks.json` (`CODEX_HOME` があれば `$CODEX_HOME/hooks.json`) |
| copilot | `~/.copilot/hooks/mdeck.json` (MornDeck 専用) |

- claude は、mdeck が `claude plugin` を実行してプラグインを入れます。Claude Code はそれを `settings.json` (`enabledPlugins` と、`~/.mdeck/claude-plugin` のパスを書いた `extraKnownMarketplaces`) と `~/.claude/plugins/` に記録し、プラグインは `~/.mdeck/claude-plugin` からその場で読みます。`settings.json` をほかのマシンと同期している場合は `mdeck hooks settings` も検討してください。入れられないとき (`claude` コマンドが無い、組織がマーケットプレイスを制限している、など) は、代わりに `~/.claude/settings.json` (`CLAUDE_CONFIG_DIR` があれば `$CLAUDE_CONFIG_DIR/settings.json`) に書き、理由を表示します。理由は `mdeck doctor` でも表示し、サーバーは起動のたびにやり直しはしません。
- `mdeck hooks settings` (設定の Claude Code の行では `s`) で、プラグインの代わりに `settings.json` を使います。この選択は残ります。`mdeck hooks plugin` (`p`) でプラグインに戻ります。
- 前の版で `settings.json` に入れたフックは、サーバーの起動時にプラグインへ移します。プラグインが有効になったのを確かめてから `settings.json` の分を外すので、プラグインを入れられないときは今のまま残ります。両方に入っていても、mdeck は同じイベントを 1 回として数えます。
- Claude Code はプラグインを起動時に読むので、動いている claude は、起動し直すか (`mdeck restart` で全タブを起動し直せます)、その中で `/reload-plugins` を実行するまで状態を送りません。
- mdeck 自身が `settings.json`・`hooks.json`・`mdeck.json` を書くときは、MornDeck のフックだけを足し、ほかの設定は残します。ファイルは整形し直され、最初に書き換える前の中身を `<ファイル名>.mdeck-bak` に残します。
- フックの形式が変わると、サーバーの起動時に導入済みのフックを新しい形式に書き換えます。プラグインのファイルも、変わったか無くなっていれば書き直します。
- claude と copilot のフックは MornDeck のタブの外では何もしません。codex はフックを常駐のプロセスで動かし、どのタブの会話かをフック側で判別できません。そのため MornDeck のサーバーが動いている間、codex のフックはこの Mac のすべての Codex の会話 (デスクトップアプリを含む) のイベントを手元のソケットへ送ります。サーバーはタブと対応が付いた会話だけを使い、それ以外は保存もログへの記録もしません。
- Codex は、フックを入れたときと変わったときに確認を求めます。Codex 側で信頼する (`/hooks`) まで、フックは実行されず、タブに状態は出ません。
- `CLAUDE_CONFIG_DIR` と `CODEX_HOME` (絶対パス) は、サーバーの環境変数 (最初に `mdeck` を実行したシェルのもの) を読みます。`mdeck hooks` と `mdeck uninstall` は、実行したシェルのものを読みます。claude のゲージは、macOS では `CLAUDE_CONFIG_DIR` に関係なく既定のキーチェーンの項目を、Linux では claude の設定フォルダ (`~/.claude` か `$CLAUDE_CONFIG_DIR`) の `.credentials.json` を読みます。
- 外すには、設定のフックの行で `x` を押すか、`mdeck uninstall` を実行します。導入済みの行の `Enter` は入れ直しです。
- サイドバーに状態が出ないときは `mdeck doctor` を実行します。版・フックの入れ方・フックがサーバーに届いているか (届いていなければ何をすればよいか)・エージェントが動いているのにフックが届いていないタブ (サイドバーでは `no hooks`) を表示します。

### 組織の管理設定 (Claude Team・Enterprise)

組織は、メンバーの Claude Code の設定を管理できます。MornDeck に関係するのは次の 2 つです。

- `strictPluginOnlyCustomization` に `"hooks"` を含む: Claude Code は `settings.json` のフックを実行せず、プラグインのフックは実行します。mdeck の既定のプラグインなら動きます。`settings.json` に入れている場合は、設定画面に「blocked by org policy」と出て、`mdeck doctor` が該当の設定を表示します。`mdeck hooks plugin` で直ります。
- `allowManagedHooksOnly` か `disableAllHooks`: Claude Code はユーザーのフックもプラグインのフックも実行しません。mdeck は claude の状態を出せません。

mdeck がこれらを読むのは、Claude Code がサーバーから受け取った設定のキャッシュ (`~/.claude/remote-settings.json`)・`managed-settings.json`・`settings.json` です。MDM のプロファイルで配られた設定は検出しませんが、プラグインは同じように使えます。

## リンク

タブの中の URL (文字の `http://`・`https://`。折り返した行をまたぐものと、アプリが OSC 8 で出したリンクも) を明るい青で描き、マウスの乗ったものに下線を引きます。乗せると近くに「Open link: Cmd+Click」も出ます。**Cmd+クリック** (macOS)、**Ctrl+クリック**、**Option+クリック**のどれかで既定のブラウザで開き (WSL では Windows のブラウザ)、リンクの色が一瞬反転します。端末は Cmd をマウスの報告に入れないので、macOS ではクリックの瞬間に Cmd を押しているかを MornDeck が macOS から読みます (許可は要りません。SSH 越しに使うときは Ctrl+クリックを使ってください)。ただのクリックでは開かないので、文字の選択は今までどおりです。設定の「Clickable links」でオフにできます。

## 使用量のゲージ

サイドバーの下に、claude と codex の週の枠、copilot の月の枠の使用量を出します。このために次を読みます。

| 対象 | 読むもの | 送り先 |
| --- | --- | --- |
| claude | キーチェーンの `Claude Code-credentials` にある OAuth トークン (Linux は `~/.claude/.credentials.json`) | 5 分おきに `api.anthropic.com/api/oauth/usage` |
| copilot | 環境変数 `COPILOT_GITHUB_TOKEN` / `GH_TOKEN` / `GITHUB_TOKEN`、キーチェーンの `copilot-cli`、`~/.copilot/config.json` のいずれかのトークン | 5 分おきに `api.github.com/copilot_internal/user` |
| codex | `~/.codex/sessions` の記録 (`CODEX_HOME` があれば `$CODEX_HOME/sessions`) | 送らない |

- トークンは上の送り先にだけ送り、保存も記録もしません。
- 設定の「Usage gauge」をオフにすると、トークンも記録も読まず、API も呼びません。

MornDeck が自分から通信するのは、この 2 つと、新しい版の確認 (github.com。1 日 1 回と、設定の「Check for updates」) と、登録した通知先だけです。利用状況は送りません。

## AutoYes

タブのメニューで AutoYes をオンにすると、Yes / No の確認に自動で Yes と答えます。

- ファイルの削除や `git push` の確認にも Yes と答えます。任せてよい作業のタブだけでオンにしてください。
- 答えるのは許可の確認だけです。選択肢がすべて英語の `Yes`・`No`・`Allow`・`Always allow`・`Cancel`・`Deny`・`Decline` のどれかで始まり、断る選択肢があるものです。最初の `Yes` を選び、無ければ今回だけ許可する `Allow` を選びます (codex の Computer Use の確認など)。`Allow for this session` と `Always allow` は選びません。
- claude のフックを入れていれば、質問と計画の承認には答えません。フックが無いときと codex・copilot では見分けられないので、そうした確認のメニューには答えます。

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
| `close_tab` | `tab` か `owner`, `force?` | タブを閉じる。`owner` を渡すと、その owner のタブをまとめて閉じる |
| `update_tab` | `tab`, `name?`, `owner?`, `auto_yes?` | タブの設定を変える |
| `send` | `tab`, `text`, `wait?`, `request_id?`, `force?`, `timeout_ms?` | 文字を送って Enter を押し、返事を待つ |
| `press_keys` | `tab`, `keys`, `force?` | キーをそのまま送る |
| `choose` | `tab`, `n`, `wait?`, `force?`, `timeout_ms?` | 選択肢を番号で選ぶ |
| `get_answer` | `tab`, `timeout_ms?` | 今の返事と選択肢。`timeout_ms` を渡すと、返事か選択肢が出るまで待つ |
| `get_screen` | `tab` | 画面の文字 |
| `subscribe` | | 起きたこと (タブの開閉・状態の変化・選択肢・AutoYes) を 1 行 1 JSON で流し続ける |
| `list_webhooks` | `owner?` | 通知先の一覧 |
| `add_webhook` | `url`, `owner?`, `headers?` | 通知先を足す |
| `remove_webhook` | `url`, `owner?` | 通知先を消す |
| `test_webhooks` | `url`, `owner?` | 1 つの通知先へ試しに送る |

タブにはエージェントの `model` と `effort` があります (分からなければ `null`。copilot は常に分かりません)。

`yolo` を `true` にすると、エージェントを確認もサンドボックスも無しで起動します (`claude --dangerously-skip-permissions` など)。エージェントが何でも実行できるので、壊れてもよい環境だけで使ってください。

待つコマンドは次の形で返します。

```json
{"ok": true, "tab": {...}, "state": "done", "seq": 3, "answer": "直しました。", "timed_out": false}
```

| キー | 中身 |
| --- | --- |
| `state` | `done` なら `answer` が返事。`waiting` なら `question` と `options` を見て `choose` で答える (codex の Computer Use の許可などフックの来ない確認では、`question` は画面から読んだ `{"prompt": ..., "details": [...]}`)。起動時の確認で止まると `blocked`。ログインし直す必要があると `auth_required` (`/login` か `codex login`。`question` にエラー)。ほかは `starting`・`idle`・`working` と、エージェントのいないタブの `null` |
| `seq` | 返事ごとに増える番号。`send` は、その依頼の返事の番号を `expect_seq` で返す |
| `timed_out` | `true` なら、`get_answer` に `timeout_ms` を付けて待ち直す |

失敗すると `{"ok": false, "error": {"code": "...", "message": "..."}}` を返します。`code` は `invalid` (引数が違う)・`no_such_tab`・`busy` (作業中)・`not_ready` (起動中)・`tab_waiting` (質問で止まっている。`choose` で答える)・`not_found`・`self_tab` (呼んだ側のタブ)・`failed`・`unknown_cmd` のどれかです。

0.x の間は、API と MCP の引数や戻り値を変えることがあります。変えたときは [CHANGELOG](CHANGELOG.md) に書きます。

## AI エージェントから使う

Claude Code・Codex CLI・GitHub Copilot CLI などの MCP クライアントから、タブを開く・中のエージェントに指示を送る・返事を待つ、ができます。

```bash
mdeck mcp install   # 入っているエージェントの CLI に MCP サーバーを登録する
mdeck guide         # エージェント向けのガイドを表示する
```

[docs/agent-guide.md](docs/agent-guide.md) (英語) は、エージェントに読ませるためのガイドです。つなぎ方・基本の流れ・状態の意味・待ち方・エラーの意味をまとめています。シェルを使えるエージェントなら、設定なしで `mdeck tabs`・`mdeck send <tab> <text>`・`mdeck wait <tab>` も使えます。

呼んだ側が動いているタブへの send・choose・close は、`force` を付けないかぎり断ります (エラー `self_tab`)。

## MCP

ツールは API のコマンドと同じです (`subscribe` を除く)。下の設定は `mdeck mcp install` が書き込みます。

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

登録した URL へ、次のときに JSON を POST します。つながらないときや 5xx が返ったときは、5 秒・15 秒・45 秒後に最大 3 回送り直します。通知には送り直しても変わらない `event_id` と、何回目かを表す `attempt` が付きます。

| とき | JSON |
| --- | --- |
| 返事を書き終えた | `{"event": "state", "state": "done", "tab": {...}, "answer": "...", "seq": 3}` |
| 選択肢を出した | `{"event": "choices", "tab": {...}, "question": {...}, "options": [{"n": 1, "label": "Yes", "selected": true}]}` |
| AutoYes が答えた | `{"event": "auto_yes", "tab": {...}, "label": "Yes", "options": [...]}` |
| サーバーが起動した (初めての起動、`mdeck restart`、更新後の初めての起動) | `{"event": "server_started", "reason": "restart", "version": "0.5.3", "previous_version": "0.5.2", "owner": null, "resumed": 20, "tabs": [{...}]}` |
| ログインし直す必要がある | `{"event": "state", "state": "auth_required", "tab": {...}, "error": "authentication_failed", "message": "Login expired · Please run /login"}` |

`auth_required` は、そのタブがまた返事を書き終えるまで 1 回だけ送り、ポップアップの設定にかかわらず必ずポップアップを出します。claude のフック (ログインのエラーでの `StopFailure`) と、フックが無くても画面から見つけます。画面で見るのは、claude の `Login expired ·`・`Not logged in ·`・`Invalid API key ·` の行と、codex の `Your access token could not be refreshed` です。数えるのは画面の下のほうの行だけで、その後の返事より上に残った古いエラーは数えません。そこからエラーの行が消えると idle に戻します。claude のほかの API エラー (使用量の上限など) は、エラーの文を返事として `done` にします。

`server_started` の `reason` は、`first_start` (この Mac で初めて動かした)、`updated` (新しい版の初めての起動。`previous_version` は前の版で、0.5.3 より前からの更新では `null`)、`restart` のどれかです。`resumed` はエージェントを再開したタブの数です。オーナー別の webhook には、そのオーナーのタブだけを入れて (`owner` 付きで) 送るので、自分のタブが戻ったかを確かめられます。

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

## アンインストール

```bash
mdeck uninstall
brew uninstall --cask --zap tsukumistudio/tap/morndeck
```

Linux では、`mdeck uninstall` のあと `rm ~/.local/bin/mdeck` と `rm -r ~/.mdeck` (ログと通知先の鍵) を実行します。

`mdeck uninstall` はフック (claude のプラグインとマーケットプレイスの登録も) と MCP の登録を外してサーバーを止めます。`--zap` の前に実行してください。`--zap` は `~/.mdeck/` をゴミ箱へ移します。ログと通知先の鍵が入っているので、消し切るにはゴミ箱を空にしてください。フックを入れる前の設定を残した `.mdeck-bak` は消しません。

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
