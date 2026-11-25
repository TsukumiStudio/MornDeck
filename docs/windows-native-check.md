# ネイティブ Windows 版の実機確認

WSL を使わずに Windows で直接動く `mdeck.exe` を、配る前に実機で確かめる手順です。いま配っている WSL 版とは別のフォルダに置いて試します。

## 用意

- Windows 10 1803 以降か Windows 11 (x64 か ARM)
- 試したいエージェント (Claude Code・Codex・Copilot) を Windows に直接入れておく (WSL の中ではなく)
- ユーザーのフォルダ (`%USERPROFILE%`) の名前に空白が無いこと (フックのコマンドに exe のパスをそのまま書くため)

## 入手

1. GitHub の [Actions → Test](https://github.com/TsukumiStudio/MornDeck/actions/workflows/test.yml) で、main の一番新しい成功した実行を開く
2. Artifacts から `mdeck-native-X64` (ARM の PC は `mdeck-native-ARM64`) を落とし、`mdeck.exe` を `C:\mdeck-native\` に置く

以下のコマンドは PowerShell で、このフォルダの exe をフルパスで呼びます (WSL 版の `mdeck` と取り違えないため)。

```powershell
C:\mdeck-native\mdeck.exe --version
```

`mdeck 0.5.22` のように版が出れば入手できています。

## 確かめること

| # | やること | 期待 |
| --- | --- | --- |
| 1 | `C:\mdeck-native\mdeck.exe` を起動する | 画面が出て、左にサイドバー、右に PowerShell のタブ |
| 2 | タブで文字を打つ・矢印キー・Ctrl+C | そのまま PowerShell に届く |
| 3 | 窓の大きさを変える | 1 秒以内に中身が新しい大きさで描き直される |
| 4 | F12 → `+` で新しいタブ、タブで `exit` | 新しいタブが開き、`exit` したタブは消える |
| 5 | 下の「フック」の手順のあと、タブで `claude` を起動して何か頼む | サイドバーの状態が working → done と変わる |
| 6 | 同じく `codex` と `copilot` | 5 と同じ |
| 7 | F12 → Config → Test sound and popup | 右下に MornDeck の名前のトーストが出る |
| 8 | サイドバーの下の使用量 | Claude と Copilot (入れていれば Codex も) に数字が出る |
| 9 | 別の PowerShell で下の「起動し直し」を実行 | 数秒で画面がつながり直し、claude・codex のタブは同じ会話に戻る |
| 10 | 画面の窓を閉じて、もう一度 `C:\mdeck-native\mdeck.exe` | タブが残っている (サーバーは裏で動き続ける) |

### フック (5・6 の前に)

```powershell
C:\mdeck-native\mdeck.exe hooks install
```

```powershell
C:\mdeck-native\mdeck.exe doctor
```

`doctor` の `hooks` の行が 3 つとも `installed` になれば入っています。5・6 のあとにもう一度実行し、`received` の行に数が出ればフックが届いています。

### 起動し直し (9)

```powershell
C:\mdeck-native\mdeck.exe restart
```

## 片付け

フックを外してサーバーを止めます。WSL 版のフック (WSL の中の設定) には触りません。

```powershell
C:\mdeck-native\mdeck.exe uninstall
```

そのあと `C:\mdeck-native` と `%USERPROFILE%\.mdeck` を消します。

## 報告してほしいもの

- 期待どおりでなかった番号と、そのときの画面 (スクリーンショット)
- `mdeck.exe doctor` の出力
- `%USERPROFILE%\.mdeck\events.log` の最後の 50 行程度。webhook の URL が入ることがあるので、送る前に見て消してください

```powershell
Get-Content $env:USERPROFILE\.mdeck\events.log -Tail 50
```

## 分かっている制限

- Copilot を npm で入れたもの (中身が node.exe) は、フックが届くまで Copilot のタブだと分かりません
- 「Install update」は、入れ替え方を出すだけです (winget に載ってから自動にします)
- Copilot の使用量のトークンは、資格情報マネージャーの `copilot-cli` で始まる名前のものを読みます。名前の付け方は推測なので、8 で Copilot に数字が出なければ教えてください
