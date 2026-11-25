# MornDeck

[English](README.md) | 日本語

MornDeck は、ターミナルの中で動くタブ型のターミナルです。Claude Code・Codex CLI・GitHub Copilot CLI が作業中・入力待ち・完了のどれかをサイドバーに出します。端末を閉じてもタブは動き続けます。

macOS 11 以降 (Apple Silicon / Intel)・Linux (x86_64 / aarch64)・Windows 10 (バージョン 2004 以降) と 11 (x64 / arm64) で動きます。Windows では本体が WSL 2 の中で動き、Windows 側から `mdeck.exe` で起動します。画面とメッセージは英語です。

![MornDeck の画面](docs/screenshot.svg)

## インストール

### macOS

```bash
brew install --cask tsukumistudio/tap/morndeck
```

同じ `mdeck` という名前のツールがほかにもあるので、tap 名を付けて入れてください。

### Windows

1. WSL 2 を入れます。管理者の PowerShell で `wsl --install` を実行して再起動します ([Microsoft の手順](https://learn.microsoft.com/ja-jp/windows/wsl/install))。
2. [最新のリリース](https://github.com/TsukumiStudio/MornDeck/releases/latest)から `MornDeck-<版>-windows-x64.zip` (Arm なら `-arm64.zip`) を落として展開します。
3. PowerShell か Windows Terminal で、展開したフォルダで `.\mdeck.exe` を実行します。初回は同じ版を WSL の中へ入れます。PATH を通す必要はありません。

claude・codex・copilot は WSL の中に入れてください。winget (`winget install TsukumiStudio.MornDeck`) は登録の審査中で、まだ使えません。

### Linux

```bash
v=<版>; a=$(uname -m)
curl -fLO "https://github.com/TsukumiStudio/MornDeck/releases/download/v$v/mdeck-$v-linux-$a.tar.gz"
curl -fLO "https://github.com/TsukumiStudio/MornDeck/releases/download/v$v/mdeck-$v-linux-$a.tar.gz.sha256"
sha256sum -c "mdeck-$v-linux-$a.tar.gz.sha256"
tar -xzf "mdeck-$v-linux-$a.tar.gz"
install -D -m 755 "mdeck-$v-linux-$a/mdeck" ~/.local/bin/mdeck   # ~/.local/bin を PATH に入れておく
```

## はじめかた

1. `mdeck` で開きます。
2. `F12` → `c` で設定 (画面では「Config」) を開き、「Hooks」で使うエージェントを選んで `Enter` を押します。
3. タブの中で `claude` などを起動します。

`F12` でメニュー、`Shift+F12` でサイドバーを隠し、「+」で新しいタブを開きます。更新は設定の「Check for updates」で行います。

更新・設定・API・MCP・Webhook・セキュリティは[マニュアル](docs/manual.ja.md)にあります。MornDeck を操作するエージェント向けは[エージェント向けガイド](docs/agent-guide.md) (英語) です。

## アンインストール

```bash
mdeck uninstall
brew uninstall --cask --zap tsukumistudio/tap/morndeck
```

Linux では、`mdeck uninstall` のあと `rm ~/.local/bin/mdeck` と `rm -r ~/.mdeck` を実行します。

## ライセンス

[Unlicense](LICENSE)。含んでいるライブラリの許諾文は [THIRD_PARTY_LICENSES](THIRD_PARTY_LICENSES) にあります。

MornDeck は Anthropic・OpenAI・GitHub とは関係のない非公式のツールです。Claude・Codex・GitHub Copilot は各社の商標です。
