#!/bin/bash
# 本物の mdeck で、サーバーの起動 → タブで偽の claude を動かす → フックが届く → send と get_answer →
# restart で会話 ID が保たれて再開する、までを通しで確かめる。HOME とソケットは一時フォルダに分ける (普段のサーバーには触れない)。
# 使い方: ci/e2e.sh <mdeck のパス>
set -euo pipefail
M=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
T=$(mktemp -d)
export HOME=$T MDECK_SOCK=$T/.mdeck/mdeck.sock SHELL=/bin/bash MDECK_BIN=$M PATH="$T/bin:$PATH"
unset MDECK_TAB CLAUDE_CONFIG_DIR CODEX_HOME TMUX
mkdir -p "$T/.mdeck" "$T/bin"
echo '{"usage": false}' > "$T/.mdeck/config.json"
# タブのシェルが (macOS のログインシェルの path_helper などで) PATH を並べ替えても、偽の claude が先に見つかるようにする
printf 'PS1="$ "\nexport PATH="$HOME/bin:$PATH"\n' | tee "$T/.bashrc" > "$T/.bash_profile"
trap '"$M" kill >/dev/null 2>&1 || true; [ -n "${KEEP:-}" ] || rm -rf "$T"' EXIT

# 偽の claude: 起動で SessionStart、1 行受けるたびに UserPromptSubmit と Stop (回答は「echo: 行」) をフックで送る
cat > "$T/bin/claude" <<'S'
#!/bin/bash
sid="sess-$$"
[ "${1:-}" = "--resume" ] && sid=$2
hook() { printf '{"hook_event_name":"%s","session_id":"%s"%s}' "$1" "$sid" "$2" | "$MDECK_BIN" hook-send claude; }
hook SessionStart ""
echo "fake claude ($sid)"
while IFS= read -r line; do
  hook UserPromptSubmit ",\"prompt\":\"$line\""
  echo "answer: $line"
  hook Stop ",\"last_assistant_message\":\"echo: $line\""
done
S
chmod +x "$T/bin/claude"

api() { "$M" api "$1"; }
fail() {
  echo "FAIL: $*" >&2
  api '{"cmd":"get_screen","tab":"agent"}' 2>/dev/null | python3 -c 'import json,sys; print("\n".join(l for l in json.load(sys.stdin).get("lines", []) if l.strip()))' >&2 || true
  [ -f "$T/.mdeck/events.log" ] && grep -v list_tabs "$T/.mdeck/events.log" | tail -10 >&2
  [ -f "$T/.mdeck/hook.log" ] && tail -5 "$T/.mdeck/hook.log" >&2
  exit 1
}
state() { api '{"cmd":"list_tabs"}' | python3 -c 'import json,sys; t=[t for t in json.load(sys.stdin)["tabs"] if t["name"]=="agent"]; print(t[0]["state"] if t else "gone")'; }
# タブの一覧は 2 秒ごとに tabs.json に保存される。会話 ID が入るまで待って読む
session() {
  for _ in $(seq 1 50); do
    s=$(python3 -c 'import json,sys; print(next((t.get("session") or "") for t in json.load(open(sys.argv[1]))["tabs"] if t.get("name")=="agent"))' "$T/.mdeck/tabs.json" 2>/dev/null) && [ -n "$s" ] && { echo "$s"; return; }
    sleep 0.2
  done
}
wait_state() { for _ in $(seq 1 100); do [ "$(state)" = "$1" ] && return 0; sleep 0.1; done; fail "state is $(state), not $1"; }
answer() { api "{\"cmd\":\"get_answer\",\"tab\":\"agent\",\"timeout_ms\":10000}" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("answer"))'; }

# 1. サーバー
(cd "$T" && nohup "$M" server >/dev/null 2>"$T/server.log" &)
for _ in $(seq 1 50); do api '{"cmd":"version"}' >/dev/null 2>&1 && break; sleep 0.1; done
api '{"cmd":"version"}' >/dev/null || fail "the server did not start"
echo "ok: server"

# 2. タブで偽の claude を起動し、SessionStart のフックで idle になる
api "{\"cmd\":\"open_tab\",\"name\":\"agent\",\"cwd\":\"$T\",\"run\":\"claude\"}" >/dev/null
wait_state idle
echo "ok: hook reached the server (idle)"

# 3. send して回答を待つ
api '{"cmd":"send","tab":"agent","text":"hello","wait":false}' >/dev/null
[ "$(answer)" = "echo: hello" ] || fail "answer: $(answer)"
wait_state done
echo "ok: send and get_answer"
before=$(session)
[ -n "$before" ] || fail "no session saved"

# 4. restart: 同じ会話 ID で再開し、また答える
"$M" restart >/dev/null
for _ in $(seq 1 100); do api '{"cmd":"version"}' >/dev/null 2>&1 && break; sleep 0.1; done
wait_state idle
after=$(session)
[ "$after" = "$before" ] || fail "session changed: $before -> $after"
# 再開したタブでは、偽の claude が同じ会話 ID の --resume で起動している
api '{"cmd":"get_screen","tab":"agent"}' | grep -q "fake claude ($before)" || fail "the tab did not resume $before"
api '{"cmd":"send","tab":"agent","text":"again","wait":false}' >/dev/null
[ "$(answer)" = "echo: again" ] || fail "answer after restart: $(answer)"
echo "ok: restart kept the session ($after)"
