# 設定

**[English](configuration.md)** | Japanese

## 設定ファイルの場所

ncon は設定を複数のレイヤーでマージして読み込みます (XDG 流):

1. **組み込みデフォルト**: バイナリに埋め込まれた既定値 (常に存在)。
2. `/etc/ncon/config.toml`: サイト共通設定。通常はパッケージマネージャ
   がインストールします。組み込みデフォルトに上書き適用されます。
3. `~/.config/ncon/config.toml`: ユーザー個別の上書き。サイト共通設定に
   上書き適用されます。多くのユーザーが編集するのはこのレイヤーです。

テーブルは再帰的にマージされ、スカラ・配列・`Option` フィールドは上位
レイヤーで丸ごと置き換えられます (helix / mpv と同じセマンティクス)。
ユーザーが書かなかったフィールドはサイト共通 → 組み込みデフォルトの順で
透けて見えるので、部分的なユーザー設定でも問題ありません (推奨)。

## 設定ファイルの生成: `--init-config`

`--init-config` はオプションのカンマ区切り引数を取ります。最初のトークン
が `system`、`user`、`/` または `~/` で始まるパスのいずれかであれば、
それが書き込み先になります。残りのトークンはプリセット名として扱われます。

### 書き込み先

| 形式 | 書き込み先 |
|---|---|
| `ncon --init-config=system` | `/etc/ncon/config.toml` |
| `ncon --init-config=user` | `~/.config/ncon/config.toml` |
| `ncon --init-config=/tmp/x.toml` | `/tmp/x.toml` (読み込みは `NCON_CONFIG` 参照、後述) |
| `ncon --init-config=~/foo.toml` | `$HOME/foo.toml` (tilde 展開、読み込みは `NCON_CONFIG` 参照、後述) |

`system` トークン使用時は `/etc/ncon/` への書き込み権限が必要なので
`sudo` で実行してください。

### コマンド実行例

```bash
sudo ncon --init-config=system,default
ncon --init-config=user,vim,jp
```

## 単一ファイルバイパス: `NCON_CONFIG`

環境変数 `NCON_CONFIG` にパスを設定し、そのファイルが存在する場合、
**そのファイルのみが読み込まれます** ─ `/etc/` と `~/.config/` の
レイヤーはスキップされ、マージは行われません。

主な用途は 2 つ:

1. **任意パスで生成したアドホック設定の読み込み**。
   `--init-config=/tmp/x.toml` や `--init-config=~/foo.toml` で生成
   したファイルは XDG 標準パス外なので、通常起動時には読み込まれません。
   `NCON_CONFIG` がそれら任意パス書き込みと実行時読み込みを橋渡しします。
2. **デバッグ**。`NCON_CONFIG=/dev/null ncon` は何もファイルを読まず
   組み込みデフォルトのみで起動するので、`/etc/ncon/` や `~/.config/ncon/`
   を触らずに baseline 比較ができます。

例:

```bash
ncon --init-config=~/foo.toml,vim       # ファイル生成
NCON_CONFIG=~/foo.toml ncon              # そのファイルだけを読み込み起動
```

## 利用可能なプリセット

| プリセット | 説明 |
|-----------|------|
| `default` | 標準キーバインド (Ctrl+Shift+C/V など) |
| `vim` | Vim ライクスクロール (Ctrl+Shift+U/D) |
| `emacs` | Emacs ライクスクロール (Alt+Shift+V/N) |
| `japanese` / `jp` | CJK フォント + IME 自動無効化 |

## 設定ファイル例

```toml
[font]
main = "JetBrains Mono"             # リガチャフォント推奨 (推奨フォント参照)
cjk = "Noto Sans CJK JP"           # またはフルパス: "/usr/share/fonts/.../X.ttf"
emoji = "Noto Color Emoji"
symbols = "Hack Nerd Font Mono"
size = 16.0                          # フォントサイズ (px, デフォルト: 16.0)
render_mode = "lcd"
lcd_filter = "light"

[keybinds]
copy = ["ctrl+shift+c", "ctrl+insert"]
paste = ["ctrl+shift+v", "shift+insert"]
screenshot = ["printscreen", "ctrl+shift+s"]

[terminal]
scrollback_lines = 10000
ime_disabled_apps = ["vim", "nvim", "emacs", "less", "man"]

[keyboard]
repeat_delay = 400           # キーリピート遅延 (ms)
repeat_rate = 30             # キーリピートレート (ms)
xkb_layout = "jp"            # XKB キーボードレイアウト
xkb_options = "ctrl:nocaps"  # XKB オプション (Caps Lock を Ctrl に)

[mouse]
speed = 1.0                  # カーソル速度倍率 (デフォルト: 1.0、4K では 1.5〜2.0 推奨)

[display]
prefer_external = true       # 外部モニター優先 (HDMI/DP > 内蔵)
auto_switch = true           # ホットプラグ時に自動切り替え

[drm]
device = "auto"              # "auto" は各 GPU を probe して接続中のディスプレイを自動選択
                             # または明示的パス: "/dev/dri/card1"

[notifications]
enabled = true               # OSC 9/99 通知を有効化 (デフォルト: true)

[paths]
screenshot_dir = "~/Pictures"
```

### DRM デバイス選択

`device = "auto"` (デフォルト) は各 `/dev/dri/card*` を probe し、ディスプレイが接続されている GPU を自動選択します。明示的にデバイスを指定することも可能です:

```toml
[drm]
device = "/dev/dri/card1"    # 特定の GPU を使用
```

ncon がどのデバイスを使用しているか確認するには、起動ログを参照してください:

```bash
journalctl -u ncon@tty2 -e | grep "DRM"
```

```
DRM auto-detect: /dev/dri/card0 has no connected connectors, skipping
DRM auto-detect: /dev/dri/card1 has 1 connected connector(s), selected
DRM device: /dev/dri/card1 (auto-detected)
```

### Optimus ラップトップ (Intel + NVIDIA)

Optimus 環境では、NVIDIA GPU が存在してもディスプレイは通常 Intel iGPU 経由で出力されます。`device = "auto"` はこれを自動的に処理します — 接続中のディスプレイがない GPU はスキップされます。

`No connected connector found` エラーで ncon が起動しない場合、どの GPU にディスプレイが接続されているか確認してください:

```bash
# 利用可能な DRM デバイスを確認
ls -l /dev/dri/card*

# 各デバイスの GPU を確認
udevadm info -a /dev/dri/card0 | grep -i vendor
udevadm info -a /dev/dri/card1 | grep -i vendor
```

**NVIDIA GPU を直接使用したい場合**、カーネルモードセッティングを有効にしてください:

```bash
echo 'options nvidia-drm modeset=1' | sudo tee /etc/modprobe.d/nvidia-drm.conf
sudo update-initramfs -u
sudo reboot
```

それでも動作しない場合 (Optimus では一般的)、Intel iGPU を明示的に指定してください:

```toml
[drm]
device = "/dev/dri/card1"    # NVIDIA ではなく Intel iGPU を使用
```

## Nerd Fonts (アイコン表示)

**yazi**, **ranger**, **lsd**, **eza**, **fish** などでアイコンを表示するには Nerd Font が必要:

```bash
# Hack Nerd Font をダウンロード・インストール
sudo mkdir -p /usr/local/share/fonts
cd /usr/local/share/fonts
sudo curl -OL https://github.com/ryanoasis/nerd-fonts/releases/latest/download/Hack.tar.xz
sudo tar xf Hack.tar.xz && sudo rm Hack.tar.xz
sudo fc-cache -fv
```

`config.toml` で設定 — フォント名でもファイルパスでも指定可能:

```toml
[font]
symbols = "Hack Nerd Font Mono"    # フォント名で指定 (推奨)
# symbols = "/usr/local/share/fonts/HackNerdFontMono-Regular.ttf"  # パスでも可
```

`symbols` フォントは Powerline グリフ (U+E000-U+F8FF) や Nerd Font アイコンのフォールバックとして使用されます。指定しない場合、ncon は fontconfig 経由でインストール済みの Nerd Font を自動検出します。

注: Powerline 矢印グリフ (E0B0-E0B7) はフォントに関係なくプログラムでピクセルパーフェクトに描画されます。

## 推奨フォント

デフォルトの等幅フォント (DejaVu Sans Mono) は**リガチャ非対応**です。`=>` `->` `!=` `===` などのリガチャを有効にするには、リガチャ対応フォントをインストールしてください:

| フォント | リガチャ | インストール (Debian/Ubuntu) | 備考 |
|---------|---------|---------------------------|------|
| **JetBrains Mono** | `=>` `->` `!=` `<=` | `sudo apt install fonts-jetbrains-mono` | 普段使いにおすすめ — 可読性のバランスが良い |
| **FiraCode** | `=>` `->` `!=` `===` `>=` `\|>` ... | `sudo apt install fonts-firacode` | リガチャの種類が最も多い — デモ映えする |
| **Cascadia Code** | `=>` `->` `!=` `<=` | [GitHub releases](https://github.com/microsoft/cascadia-code/releases) | Microsoft 製コーディングフォント |

```toml
[font]
main = "JetBrains Mono"    # または "Fira Code"
```

## フォントサイズ

デフォルトのフォントサイズは `16.0` (px) です。高解像度ディスプレイでは、見やすいサイズに変更してください:

| 解像度 | 推奨サイズ |
|--------|-----------|
| 1080p (FHD) | 16.0 (デフォルト) |
| 1440p (QHD) | 18.0 – 20.0 |
| 2160p (4K) | 22.0 – 28.0 |

```toml
[font]
size = 24.0
```

ランタイムでも `Ctrl+Plus` / `Ctrl+Minus` で変更可能です (`Ctrl+0` でリセット)。
