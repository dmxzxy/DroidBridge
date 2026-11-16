#!/usr/bin/env bash
# 插件宿主集成自测（bash / git-bash）：
#   先 cargo build，然后 ./tools/plugins/host-selftest.sh
# 启动 target/debug/droidbridge --plugin-host，喂 init + req menu，校验 NDJSON 输出。
set -u

cd "$(dirname "$0")/../.." || exit 1

EXE=target/debug/droidbridge.exe
[ -x "$EXE" ] || EXE=target/debug/droidbridge
if [ ! -x "$EXE" ]; then
  echo "缺少 $EXE，先执行 cargo build" >&2
  exit 1
fi

# 转成 JSON 里能用的绝对路径
abspath() {
  local p
  p=$(cd "$1" && { pwd -W 2>/dev/null || pwd; })
  printf '%s' "$p" | sed 's#\\#/#g'
}

ROOT=$(pwd -W 2>/dev/null || pwd)
WORK="$ROOT/target/plugin-host-selftest"
rm -rf "$WORK"
mkdir -p "$WORK/plugin" "$WORK/data"

cat > "$WORK/plugin/main.lua" <<'LUA'
db.menu("tools", "Selftest", function()
  print("selftest menu ran")
  return "selftest-ok"
end)
print("selftest boot")
LUA

DIR=$(abspath "$WORK/plugin")
DATA=$(abspath "$WORK/data")

OUT="$WORK/out.ndjson"

send() { printf '%s\n' "$1" >&"$HIN"; }

# 读一行，记录；遇到 call 自动回 ret
read_line() {
  local line
  IFS= read -r -t 10 line <&"$HOUT" || return 1
  printf '%s\n' "$line" >> "$OUT"
  case "$line" in
    *'"t":"call"'*)
      local id
      id=$(printf '%s' "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
      [ -n "$id" ] && send "{\"t\":\"ret\",\"id\":$id,\"ok\":true}"
      ;;
  esac
  printf '%s' "$line"
}

# 跑一次完整交互；成功返回 0
run_once() {
  : > "$OUT"
  coproc HOST { "$EXE" --plugin-host 2>"$WORK/err.txt"; }
  HIN=${HOST[1]}
  HOUT=${HOST[0]}

  send "{\"t\":\"init\",\"plugin\":{\"id\":\"selftest\",\"name\":\"Selftest\",\"version\":\"1.0.0\",\"dir\":\"$DIR\",\"dataDir\":\"$DATA\",\"main\":\"main.lua\"},\"granted\":[],\"settings\":{},\"lang\":\"zh\",\"node\":\"pc\"}"

  local line ready=0
  for _ in $(seq 1 40); do
    line=$(read_line) || break
    case "$line" in
      *'"t":"ready"'*) ready=1; break ;;
    esac
  done
  [ "$ready" = 1 ] || { kill "$HOST_PID" 2>/dev/null; return 1; }

  send '{"t":"req","id":1,"m":"menu","p":{"id":"tools:Selftest","ctx":{}}}'
  local res=""
  for _ in $(seq 1 40); do
    line=$(read_line) || break
    case "$line" in
      *'"t":"res"'*)
        case "$line" in
          *'"id":1,'*) res="$line"; break ;;
        esac
        ;;
    esac
  done
  [ -n "$res" ] || { kill "$HOST_PID" 2>/dev/null; return 1; }

  exec {HIN}>&-
  wait "$HOST_PID" 2>/dev/null
  [ "$res" != "" ]
}

ok=0
for attempt in 1 2; do
  # 刚 build 完时 Windows 可能短暂锁住 exe，稍等一下再起
  sleep 0.4
  if run_once; then ok=1; break; fi
  [ "$attempt" = 1 ] && echo "第一次尝试失败，重试…" >&2
done

if [ "$ok" != 1 ]; then
  echo "SELFTEST FAILED" >&2
  echo "---- host output ----" >&2
  cat "$OUT" >&2
  echo "---- host stderr ----" >&2
  cat "$WORK/err.txt" 2>/dev/null >&2
  exit 1
fi

# ---- 校验
fail() {
  echo "SELFTEST FAILED: $1" >&2
  echo "---- host output ----" >&2
  cat "$OUT" >&2
  exit 1
}
grep -q '"t":"ready"' "$OUT" || fail "缺少 ready"
grep -q '"m":"register"' "$OUT" || fail "缺少 register"
grep -q '"kind":"menu"' "$OUT" || fail "缺少 register menu"
grep -q '"msg":"selftest boot"' "$OUT" || fail "缺少 boot 日志"
grep -q '"msg":"selftest menu ran"' "$OUT" || fail "缺少菜单日志"
grep -q '"ok":"selftest-ok"' "$OUT" || fail "res 内容不对"

echo "SELFTEST OK"
echo "---- host output ----"
cat "$OUT"
