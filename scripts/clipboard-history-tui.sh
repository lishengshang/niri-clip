#!/usr/bin/env bash
# niri-clip TUI v0.1 - Plan B: 单进程 fzf + reload-sync + --track --id-nth
# 解决删除后跳回顶部的问题：删除/固定后光标停在 下一个/上一个，而不是第一行
# 兼容 cliphist + pinned.ids (后续 Rust 版本将迁移到 SQLite)
set -Eeuo pipefail

ENABLE_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/niri/clipboard-history.enabled"
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/cliphist"
PIN_FILE="$STATE_DIR/pinned.ids"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/niri-clip"
mkdir -p "$CACHE_DIR" "$STATE_DIR"
chmod 700 "$STATE_DIR" 2>/dev/null || true
touch "$PIN_FILE" 2>/dev/null || true
chmod 600 "$PIN_FILE" 2>/dev/null || true

if [[ ! -e "$ENABLE_FILE" ]]; then
    notify-send "剪贴板历史已关闭" "创建 ~/.config/niri/clipboard-history.enabled 后再使用" 2>/dev/null || true
    exit 0
fi

for cmd in cliphist fzf wl-copy fuzzel; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        notify-send -u critical "剪贴板历史" "缺少命令: $cmd" 2>/dev/null || true
        exit 1
    fi
done

# ===== 生成 fzf reload 用的 build-menu 脚本 =====
BUILD_SCRIPT="$CACHE_DIR/build-menu.sh"
cat >"$BUILD_SCRIPT" <<'EOS_BUILD'
#!/usr/bin/env bash
set -euo pipefail
PIN_FILE="${XDG_STATE_HOME:-$HOME/.local/state}/cliphist/pinned.ids"
touch "$PIN_FILE" 2>/dev/null || true
# pinned 置顶
cliphist list | while IFS=$'\t' read -r id content; do
    [[ "$id" =~ ^[0-9]+$ ]] || continue
    if grep -Fqx -- "$id" "$PIN_FILE" 2>/dev/null; then
        printf '★\t%s\t%s\n' "$id" "$content"
    fi
done
cliphist list | while IFS=$'\t' read -r id content; do
    [[ "$id" =~ ^[0-9]+$ ]] || continue
    if ! grep -Fqx -- "$id" "$PIN_FILE" 2>/dev/null; then
        printf ' \t%s\t%s\n' "$id" "$content"
    fi
done
EOS_BUILD
chmod +x "$BUILD_SCRIPT"

# ===== 固定/取消固定 脚本 =====
PIN_SCRIPT="$CACHE_DIR/toggle-pin.sh"
cat >"$PIN_SCRIPT" <<'EOS_PIN'
#!/usr/bin/env bash
set -euo pipefail
id="${1:-}"
[[ "$id" =~ ^[0-9]+$ ]] || exit 0
PIN_FILE="${XDG_STATE_HOME:-$HOME/.local/state}/cliphist/pinned.ids"
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/cliphist"
mkdir -p "$STATE_DIR"
touch "$PIN_FILE"
tmp=$(mktemp "$STATE_DIR/pinned.ids.XXXXXX")
if grep -Fqx -- "$id" "$PIN_FILE" 2>/dev/null; then
    grep -Fxv -- "$id" "$PIN_FILE" >"$tmp" || true
    notify-send "剪贴板" "已取消固定第 $id 条" 2>/dev/null || true
else
    cat "$PIN_FILE" >"$tmp" 2>/dev/null || true
    printf '%s\n' "$id" >>"$tmp"
    notify-send "剪贴板" "已固定第 $id 条" 2>/dev/null || true
fi
sort -n -u "$tmp" >"$PIN_FILE"
rm -f -- "$tmp"
EOS_PIN
chmod +x "$PIN_SCRIPT"

# ===== 删除 脚本 (含星标二次确认) =====
DELETE_SCRIPT="$CACHE_DIR/delete.sh"
cat >"$DELETE_SCRIPT" <<'EOS_DELETE'
#!/usr/bin/env bash
set -euo pipefail
id="${1:-}"
[[ "$id" =~ ^[0-9]+$ ]] || exit 0
PIN_FILE="${XDG_STATE_HOME:-$HOME/.local/state}/cliphist/pinned.ids"
# 查出原始行用于 cliphist delete (需要 id+TAB+preview 精确匹配)
orig=$(cliphist list | grep -F -m1 -- "${id}	" || true)
if [[ -z "$orig" ]]; then
    # 兜底：id 可能不存在了
    exit 0
fi
if grep -Fqx -- "$id" "$PIN_FILE" 2>/dev/null; then
    choice=$(printf '%s\n' '取消' '确认' | fuzzel --dmenu --lines 2 --width 18 --prompt "删除星标 $id? " 2>/dev/null || true)
    if [[ "$choice" != "确认" ]]; then
        exit 0
    fi
    # 删除后同步清理 pinned
    printf '%s\n' "$orig" | cliphist delete >/dev/null 2>&1 || true
    sed -i -E "/^${id}$/d" "$PIN_FILE" 2>/dev/null || true
    notify-send "剪贴板" "已删除星标第 $id 条" 2>/dev/null || true
else
    printf '%s\n' "$orig" | cliphist delete >/dev/null 2>&1 || true
    notify-send "剪贴板" "已删除第 $id 条" 2>/dev/null || true
fi
EOS_DELETE
chmod +x "$DELETE_SCRIPT"

# ===== 清空 脚本 =====
WIPE_SCRIPT="$CACHE_DIR/wipe.sh"
cat >"$WIPE_SCRIPT" <<'EOS_WIPE'
#!/usr/bin/env bash
set -euo pipefail
PIN_FILE="${XDG_STATE_HOME:-$HOME/.local/state}/cliphist/pinned.ids"
choice=$(printf '%s\n' '取消' '确认清空' | fuzzel --dmenu --lines 2 --width 20 --prompt '清空全部历史？ ' 2>/dev/null || true)
if [[ "$choice" == "确认清空" ]]; then
    cliphist wipe >/dev/null 2>&1 || true
    : >"$PIN_FILE" 2>/dev/null || true
    notify-send "剪贴板" "历史已清空" 2>/dev/null || true
fi
EOS_WIPE
chmod +x "$WIPE_SCRIPT"

# ===== 预览脚本 (可选，全量 decode) =====
PREVIEW_SCRIPT="$CACHE_DIR/preview.sh"
cat >"$PREVIEW_SCRIPT" <<'EOS_PREVIEW'
#!/usr/bin/env bash
id="${1:-}"
[[ "$id" =~ ^[0-9]+$ ]] || exit 0
orig=$(cliphist list | grep -F -m1 -- "${id}	" || true)
if [[ -n "$orig" ]]; then
    printf '%s\n' "$orig" | cliphist decode 2>/dev/null | head -n 100 | head -c 2000
else
    echo "(无预览)"
fi
EOS_PREVIEW
chmod +x "$PREVIEW_SCRIPT"

BUILD_CMD="bash $BUILD_SCRIPT"
PIN_CMD="bash $PIN_SCRIPT"
DELETE_CMD="bash $DELETE_SCRIPT"
WIPE_CMD="bash $WIPE_SCRIPT"

# 预览：优先用传递的 id 做全量 decode，失败则显示截断的 preview
# fzf preview 里的 {2} 会被替换为 id 列
PREVIEW_PLACEHOLDER="bash $PREVIEW_SCRIPT {2}"

# ===== 单进程 fzf 核心 =====
# --track + --id-nth 2 : 以 id 为主键跨 reload 跟踪光标，删除后自动停在下一个
# --no-sort : 保持 pinned 置顶顺序
# reload-sync : 同步重载，避免闪空
selected=$(
    bash "$BUILD_SCRIPT" | fzf \
        --no-sort \
        --delimiter=$'\t' \
        --with-nth='1,3..' \
        --tabstop=1 \
        --height=100% \
        --layout=reverse \
        --border \
        --info=inline \
        --prompt='剪贴板> ' \
        --header=$'Enter粘贴 · ^P固定/取消 · ^X删除 · Alt-X清空 · ^R刷新 · 预览在下方' \
        --track \
        --id-nth=2 \
        --preview="$PREVIEW_PLACEHOLDER" \
        --preview-window='down:5:wrap:border-rounded' \
        --bind "ctrl-p:execute-silent($PIN_CMD {2})+reload-sync($BUILD_CMD)" \
        --bind "ctrl-x:execute-silent($DELETE_CMD {2})+reload-sync($BUILD_CMD)" \
        --bind "ctrl-r:reload-sync($BUILD_CMD)" \
        --bind "alt-x:execute($WIPE_CMD)+reload-sync($BUILD_CMD)" \
        --bind "ctrl-f:accept" \
        --expect=ctrl-f \
        2>/dev/null || true
)

# fzf 退出状态处理
[[ -n "$selected" ]] || exit 0

# 当用了 --expect，首行是按键，剩余是选中行；没有 expect 时直接是选中行
# 我们绑定了 ctrl-f 为 expect，所以需要兼容
first_line=$(printf '%s\n' "$selected" | head -n1)
rest=$(printf '%s\n' "$selected" | tail -n +2)

if [[ "$first_line" == "ctrl-f" ]]; then
    row="$rest"
else
    # 无 expect 时，first_line 就是 row 本身
    if [[ -n "$rest" ]]; then
        row="$rest"
    else
        row="$first_line"
    fi
fi

[[ -n "$row" ]] || exit 0
# row 格式: ★/ \t id \t content_preview
id=$(printf '%s\n' "$row" | cut -f2)
preview=$(printf '%s\n' "$row" | cut -f3-)
[[ "$id" =~ ^[0-9]+$ ]] || exit 0
original=$(printf '%s\t%s\n' "$id" "$preview")
# 查找真实原始内容做粘贴（避免 preview 被截断）
orig_full=$(cliphist list | grep -F -m1 -- "${id}	" || true)
if [[ -n "$orig_full" ]]; then
    original="$orig_full"
fi

printf '%s\n' "$original" | cliphist decode 2>/dev/null | wl-copy 2>/dev/null || {
    # 兜底：直接复制 preview
    printf '%s' "$preview" | wl-copy 2>/dev/null || true
}
# 轻量通知可选
# notify-send "剪贴板" "已粘贴第 $id 条" 2>/dev/null || true
exit 0
