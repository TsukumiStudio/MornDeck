# MornDeck

MornDeck は、ターミナル内で動作するタブ型のターミナルです。ひとつの端末の中にタブをいくつも作り、サイドバーで切り替えて使います。

## 特徴

- ひとつの端末の中にタブをいくつも作れます。タブはサイドバーに並び、キーやマウスで切り替えられます。
- 端末を閉じても、タブの中のプロセスは動いたまま残ります。もう一度 `mdeck` を開けば続きから使えて、ssh で外からつないでも同じ画面に戻れます。
- AI 向けの API と MCP がそろっています。AI や自分で書いたプログラムから、タブを開く・文字を送る・画面を読む・返事を待つ、といった操作ができます。
- claude・codex・copilot をタブで動かすと、考え中か・入力待ちか・終わったかをサイドバーに表示し、終わったら通知音やポップアップで知らせます。

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

## API

`mdeck api '<json>'` で、タブを外から操作できます。

```bash
mdeck api '{"cmd":"open_tab","name":"work","cwd":"/path/to/repo","agent":"claude"}'
mdeck api '{"cmd":"send","tab":"work","text":"テストを直して"}'
mdeck api '{"cmd":"get_screen","tab":"work"}'
```

主なコマンドは次のとおりです。

| コマンド | できること |
| --- | --- |
| `list_tabs` | タブの一覧を返します |
| `open_tab` / `close_tab` | タブを開きます / 閉じます |
| `send` | 文字を送って Enter を押し、エージェントの返事を待ちます |
| `press_keys` | キーをそのまま送ります |
| `choose` | エージェントが出した選択肢を番号で選びます |
| `wait` / `get_answer` | 返事を待ちます / 今の返事を返します |
| `get_screen` | タブの画面の文字を返します |

すべてのコマンドと引数は [docs/api.md](docs/api.md) にあります。

## MCP

`mdeck mcp` を MCP サーバーとして登録すると、AI から同じ操作ができます。ツールの名前と引数は、API のコマンドと同じです。

```json
{"mcpServers": {"mdeck": {"command": "/opt/homebrew/bin/mdeck", "args": ["mcp"]}}}
```

## Webhook

エージェントが終わったときなどに、登録した URL へ JSON を POST します。登録は API の `add_webhook` か、設定 (Config) の「通知先」から行います。

## ライセンス

[Unlicense](LICENSE)
