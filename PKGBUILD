# Maintainer: lishengshang <3490017805@qq.com>
pkgname=niri-clip
pkgver=0.7.0
pkgrel=1
pkgdesc="为 niri 打造的全新高性能 Wayland 剪贴板"
arch=('x86_64' 'aarch64')
url="https://github.com/lishengshang/niri-clip"
license=('MIT')
depends=('glibc' 'libgcc' 'wl-clipboard' 'fzf>=0.71' 'libnotify')
makedepends=('cargo')
optdepends=(
  'fuzzel: 后备选择器（无 fzf 环境自动回退）'
  'kitty: kitty icat 图片预览'
  'chafa: 图形终端图片预览'
  'cliphist: 可选, 一次性迁移旧数据'
)
# bundled SQLite 的 GCC LTO 对象与 Rust 链接器不兼容。Rust 自带 LTO 仍然启用。
# Cargo 的 release profile 已剥离符号。
options=('!lto' '!debug')
source=("$pkgname-$pkgver.tar.gz::$url/archive/refs/tags/v$pkgver.tar.gz")
# tag 发布后，在独立的 AUR 配方副本中用 updpkgsums 写入摘要并生成 .SRCINFO。
# 摘要不能写进所校验的同一份源码归档。留空时 makepkg 会拒绝通过校验。
sha256sums=()

prepare() {
  cd "$srcdir/$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  cargo fetch --locked --target host-tuple
}

build() {
  cd "$srcdir/$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable CARGO_TARGET_DIR=target
  cargo build --release --locked --offline -p "$pkgname"
}

package() {
  cd "$srcdir/$pkgname-$pkgver"
  install -Dm755 "target/release/niri-clip" "$pkgdir/usr/bin/niri-clip"
  install -Dm644 "config/config.toml.example" "$pkgdir/usr/share/doc/$pkgname/config.toml.example"
  install -Dm644 "LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 "assets/niri-clip.kdl" "$pkgdir/usr/share/doc/$pkgname/niri-clip.kdl.example"
  install -Dm644 "assets/niri-clip.service" "$pkgdir/usr/lib/systemd/user/niri-clip.service"
  # 服务单元按包安装路径改写（任务 2.6 A1）：源单元随二进制内置、供
  # `cargo install` 用户走 `niri-clip install-service`，写的是 %h/.cargo/bin；
  # 而 AUR 装到 /usr/bin。systemd **不使用 $PATH**（非绝对路径只按编译期
  # 固定目录 /usr/local/bin、/usr/bin 解析），故 %h/.cargo/bin 在 AUR 场景
  # 必然 status=203/EXEC —— 不改这一行，"paru -S 后 enable --now 即用"不成立
  sed -i 's|^ExecStart=%h/\.cargo/bin/niri-clip |ExecStart=/usr/bin/niri-clip |' \
    "$pkgdir/usr/lib/systemd/user/niri-clip.service"
  install -Dm644 "README.md" "$pkgdir/usr/share/doc/$pkgname/README.md"
  # 任务 1.7：man page 与 shell 补全（由二进制自生成，无需仓库内存文件）
  for d in usr/share/man/man1 \
           usr/share/bash-completion/completions \
           usr/share/zsh/site-functions \
           usr/share/fish/vendor_completions.d; do
    mkdir -p "$pkgdir/$d"
  done
  "./target/release/niri-clip" man > "$pkgdir/usr/share/man/man1/niri-clip.1"
  "./target/release/niri-clip" completions bash > "$pkgdir/usr/share/bash-completion/completions/niri-clip"
  "./target/release/niri-clip" completions zsh  > "$pkgdir/usr/share/zsh/site-functions/_niri-clip"
  "./target/release/niri-clip" completions fish > "$pkgdir/usr/share/fish/vendor_completions.d/niri-clip.fish"
}
