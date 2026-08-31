# Maintainer: lishengshang <3490017805@qq.com>
pkgname=niri-clip
pkgver=0.5.1
pkgrel=1
pkgdesc="为 niri 打造的全新高性能 Wayland 剪贴板"
arch=('x86_64' 'aarch64')
url="https://github.com/lishengshang/niri-clip"
license=('MIT')
depends=('wl-clipboard' 'fzf>=0.71')
makedepends=('cargo' 'git')
optdepends=(
  'fuzzel: 后备选择器（无 fzf 环境自动回退）'
  'kitty: kitty icat 图片预览'
  'chafa: 图形终端图片预览'
  'cliphist: 可选, 一次性迁移旧数据'
)
# 主包只构建 CLI（-p niri-clip）：workspace 中 niri-clip-gui 的依赖链
# （iced/winit）需要 libxkbcommon，由独立的 niri-clip-gui 包承载。
# NOTE: 提交 AUR 正式包前需用 updpkgsums / makepkg -g 替换为真实校验值；
# SKIP 仅适用于 *-git 包。入库的 Cargo.lock 保证 --locked 可复现构建。
source=("$pkgname-$pkgver.tar.gz::https://github.com/lishengshang/niri-clip/archive/v$pkgver.tar.gz")
sha256sums=('e0f66d293150d63d12cc7259a801a3dedd28799a5bf9b8b9af48cb769339f147')

build() {
  cd "$pkgname-$pkgver"
  # makepkg 注入的 -flto=auto 会把 bundled sqlite3.c 编成 GCC LTO 字节码，
  # cargo 默认的 lld 链接器无法解析，导致大量 undefined symbol。
  # 剥离 -flto（Rust 自带 LTO，见 profile.release）
  export CFLAGS="${CFLAGS/-flto=auto/}" CXXFLAGS="${CXXFLAGS/-flto=auto/}" LDFLAGS="${LDFLAGS/-flto=auto/}"
  cargo build --release --locked -p niri-clip
}

package() {
  cd "$pkgname-$pkgver"
  install -Dm755 "target/release/niri-clip" "$pkgdir/usr/bin/niri-clip"
  install -Dm644 "config/config.toml.example" "$pkgdir/usr/share/doc/$pkgname/config.toml.example"
  install -Dm644 "LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 "assets/niri-clip.kdl" "$pkgdir/usr/share/doc/$pkgname/niri-clip.kdl.example"
  install -Dm644 "assets/niri-clip.service" "$pkgdir/usr/lib/systemd/user/niri-clip.service"
  install -Dm644 "README.md" "$pkgdir/usr/share/doc/$pkgname/README.md"
  # 任务 1.7：man page 与 shell 补全（由二进制自生成，无需仓库内存文件）
  for d in usr/share/man/man1 \
           usr/share/bash-completion/completions \
           usr/share/zsh/site-functions \
           usr/share/fish/vendor_completions.d; do
    mkdir -p "$pkgdir/$d"
  done
  "./target/release/niri-clip" man | gzip -c > "$pkgdir/usr/share/man/man1/niri-clip.1.gz"
  "./target/release/niri-clip" completions bash > "$pkgdir/usr/share/bash-completion/completions/niri-clip"
  "./target/release/niri-clip" completions zsh  > "$pkgdir/usr/share/zsh/site-functions/_niri-clip"
  "./target/release/niri-clip" completions fish > "$pkgdir/usr/share/fish/vendor_completions.d/niri-clip.fish"
}
