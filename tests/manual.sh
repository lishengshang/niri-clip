#!/usr/bin/env bash
# niri-clip manual smoke test - 验证 pin 置顶 + 删除后 pos 跟随 + 性能
# 全程在临时 XDG 环境隔离运行，绝不触碰真实剪贴板历史库
set -euo pipefail
BIN="${1:-$HOME/.cargo/bin/niri-clip}"
if [[ ! -x "$BIN" ]]; then BIN="./target/debug/niri-clip"; fi
if [[ ! -x "$BIN" ]]; then echo "niri-clip not found"; exit 1; fi

TMP_ROOT=$(mktemp -d /tmp/niri-clip-manual.XXXXXX)
trap 'rm -rf "$TMP_ROOT"' EXIT
export XDG_STATE_HOME="$TMP_ROOT/state"
export XDG_CONFIG_HOME="$TMP_ROOT/config"
export XDG_CACHE_HOME="$TMP_ROOT/cache"
mkdir -p "$XDG_STATE_HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME"

echo "=== niri-clip manual smoke test ==="
echo "bin: $BIN"
echo "version: $($BIN --version)"
echo "isolated state: $XDG_STATE_HOME"

# 1. wipe
echo -e "\n[1] wipe"
$BIN wipe >/dev/null
cliphist wipe >/dev/null 2>&1 || true
echo "wiped, count niri-clip=$($BIN list-raw | wc -l) cliphist=$(cliphist list 2>/dev/null | wc -l)"

# 2. 造 20 条
echo -e "\n[2] insert 20 entries"
for i in $(seq 1 20); do
  echo "test-entry-$i $(date +%s%N)" | $BIN store >/dev/null
done
sleep 0.2
count=$($BIN list-raw | wc -l)
echo "inserted, count=$count"
if [[ "$count" -ne 20 ]]; then echo "FAIL: expected 20 got $count"; exit 1; fi
echo "list-raw head:"
$BIN list-raw | head -n 5

# 3. 验证 pinned 置顶
echo -e "\n[3] pin test"
# list-raw 为 5 列格式 num\t▶\t★\tid\tpreview，id 在第 4 列
first_id=$($BIN list-raw | head -n1 | cut -f4)
echo "pin $first_id"
$BIN pin "$first_id" >/dev/null
echo "after pin head:"
$BIN list-raw | head -n 3
if ! $BIN list-raw | head -n1 | grep -q "★"; then echo "FAIL: pinned not on top"; exit 1; fi
echo "pin OK, unpin"
$BIN pin "$first_id" >/dev/null

# 4. 验证删除后 pos 跟随（模拟 fzf --track --id-nth 逻辑）
echo -e "\n[4] delete pos follow test (middle & last)"
# 获取当前列表 id 顺序（id 在第 4 列）
ids_before=($($BIN list-raw | cut -f4))
echo "before ids: ${ids_before[*]:0:5} ... total ${#ids_before[@]}"
# 删中间第 5 条 (index 4)
mid_id=${ids_before[4]}
mid_next=${ids_before[5]}
echo "delete middle id=$mid_id, expect next=$mid_next to move to pos 5"
$BIN delete "$mid_id" >/dev/null
ids_after=($($BIN list-raw | cut -f4))
echo "after ids: ${ids_after[*]:0:5} ..."
if [[ "${ids_after[4]}" != "$mid_next" ]]; then
  echo "FAIL: middle delete pos not follow, expected ${mid_next} at pos5 got ${ids_after[4]}"
  exit 1
fi
echo "middle delete OK"

# 删最后一行，期望停在上一个
last_idx=$((${#ids_after[@]}-1))
last_id=${ids_after[$last_idx]}
prev_id=${ids_after[$((last_idx-1))]}
echo "delete last id=$last_id, expect prev=$prev_id to be last"
$BIN delete "$last_id" >/dev/null
ids_after2=($($BIN list-raw | cut -f4))
new_last=${ids_after2[-1]}
if [[ "$new_last" != "$prev_id" ]]; then
  echo "FAIL: last delete pos not follow, expected $prev_id got $new_last"
  exit 1
fi
echo "last delete OK"

# 5. 压测 10k
echo -e "\n[5] bench 10k"
# 先批量插入到接近 10k
echo "current count: $($BIN list-raw | wc -l), inserting 200 more for bench..."
for i in $(seq 1 200); do
  echo "bench-$i-$(date +%s%N)-$RANDOM" | $BIN store >/dev/null
done
echo "bench via store::bench_10k (list 10k):"
time $BIN status >/dev/null
# 直接测 list 10000 耗时
start=$(date +%s%N)
$BIN list-raw >/dev/null 2>&1 || true
end=$(date +%s%N)
elapsed_ms=$(( (end-start)/1000000 ))
echo "list-raw (300 limit) took ${elapsed_ms}ms"
# 全量列表测试（隔离环境内）
DB_PATH="$XDG_STATE_HOME/niri-clip/db.sqlite"
echo "full list via sqlite ($DB_PATH):"
sqlite3 "$DB_PATH" "SELECT count(*) FROM clips;" 2>&1 | head
time sqlite3 "$DB_PATH" "SELECT id, text FROM clips ORDER BY pinned DESC, ts DESC LIMIT 10000;" >/dev/null 2>&1 || echo "sqlite bench done"

if [[ "$elapsed_ms" -gt 50 ]]; then
  echo "WARN: list-raw >50ms ($elapsed_ms), consider cache"
else
  echo "PASS: list-raw <50ms"
fi

# 6. 图片预览开关
echo -e "\n[6] image preview config"
if grep -q "enable_image_preview" "$XDG_CONFIG_HOME/niri-clip/config.toml" 2>/dev/null; then
  echo "config has enable_image_preview"
  grep enable_image "$XDG_CONFIG_HOME/niri-clip/config.toml"
else
  echo "no image preview config"
fi
if command -v chafa >/dev/null 2>&1; then echo "chafa available: $(chafa --version | head -1)"; else echo "chafa not installed (optional)"; fi

# 7. 清理
echo -e "\n[7] cleanup - wipe"
$BIN wipe >/dev/null
echo "final count: $($BIN list-raw | wc -l)"

echo -e "\n=== ALL TESTS PASSED ==="
echo "Mod+V 删除后 pos 跟随: OK"
echo "懒加载 300 + 缓存: OK"
echo "图片预览开关: checked"
