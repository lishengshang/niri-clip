# Maintainer: lishengshang <3490017805@qq.com>
pkgname=niri-clip
pkgver=0.2.0
pkgrel=1
pkgdesc="为 niri + Wayland 设计的高性能剪贴板历史 - 单进程 fzf 不跳顶"
arch=('x86_64' 'aarch64')
url="https://github.com/lishengshang/niri-clip"
license=('MIT')
depends=('wl-clipboard' 'fzf' 'fuzzel' 'sqlite')
makedepends=('cargo' 'git')
optdepends=(
  'kitty: fzf TUI 终端'
  'nirius: niri focus-or-spawn'
  'chafa: 图片预览'
  'cliphist: 旧数据迁移'
)
source=("$pkgname-$pkgver.tar.gz::https://github.com/lishengshang/niri-clip/archive/v$pkgver.tar.gz")
sha256sums=('SKIP')

build() {
  cd "$pkgname-$pkgver"
  cargo build --release --locked
}

package() {
  cd "$pkgname-$pkgver"
  install -Dm755 "target/release/niri-clip" "$pkgdir/usr/bin/niri-clip"
  install -Dm644 "config/config.toml.example" "$pkgdir/usr/share/doc/$pkgname/config.toml.example"
  install -Dm644 "LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 "assets/niri-clip.kdl" "$pkgdir/usr/share/doc/$pkgname/niri-clip.kdl.example"
  install -Dm644 "assets/niri-clip.service" "$pkgdir/usr/lib/systemd/user/niri-clip.service"
  install -Dm644 "README.md" "$pkgdir/usr/share/doc/$pkgname/README.md"
}
