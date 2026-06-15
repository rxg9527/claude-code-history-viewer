# 本机环境审计

检查时间：2026-06-13  
检查目录：当前仓库根目录

## 目的

这份文档用于记录本机运行和构建 `claude-code-history-viewer` 所需环境的现状、当前缺失项，以及未来如何清理这些依赖。

## 仓库要求

根据仓库自身配置，当前项目至少需要：

- Node.js `18+`
- `pnpm`
- Rust 工具链，且 `src-tauri/Cargo.toml` 中声明的最低版本为 `1.77.2`
- `README` 和 `justfile` 推荐使用 `just`
- 在 macOS 上进行 Tauri 桌面构建时，还需要可用的 Apple 构建工具链

## 当前机器状态

### 平台信息

- 架构：`arm64`
- 操作系统：`macOS 26.4`
- Xcode：`26.2`
- Xcode 开发目录：`/Applications/Xcode.app/Contents/Developer`
- Clang：`Apple clang version 17.0.0`
- Homebrew：`5.1.15`

### 工具检查结果

| 依赖 | 预期 | 当前状态 | 结论 |
|---|---|---|---|
| Node.js | `18+` | `/opt/homebrew/bin/node`，`v25.8.2` | 已安装。版本高于仓库最低要求，但比文档基线更新。 |
| pnpm | 必需 | `/opt/homebrew/bin/pnpm` | 已存在，但当前状态还不能视为稳定可用。 |
| Rust (`rustc`) | 必需 | not found | 缺失 |
| Cargo | 必需 | not found | 缺失 |
| just | 推荐 | not found | 缺失 |
| mise | 可选辅助工具 | not found | 缺失 |
| Xcode / clang | macOS 必需 | 已存在 | 正常 |

### 工作区状态

| 项目 | 当前状态 | 含义 |
|---|---|---|
| `node_modules/` | missing | 当前仓库的前端依赖和 Tauri 侧 JS 依赖尚未安装。 |
| `.pnpm-store/` | present | 仓库工作区内已经存在一个本地 pnpm store。 |
| `src-tauri/target/` | 本次未作为必查项 | 当前判断依赖缺口并不需要依赖 Rust 构建产物。 |

### 运行时观察

1. 执行 `pnpm -v` 时，没有得到稳定的本地版本输出，而是出现了下面两类信息：
   - `The "pnpm" field in package.json is no longer read by pnpm`
   - `GET https://registry.npmjs.org/@pnpm%2Fexe: fetch failed`
2. 当前 `pnpm` 是一个全局安装，位置为：
   - `/opt/homebrew/bin/pnpm`
   - 软链接目标：`../lib/node_modules/pnpm/bin/pnpm.mjs`
3. `npm list -g pnpm --depth=0` 显示：
   - `pnpm@11.3.0`

这说明：本机上虽然存在全局 `pnpm`，但当前状态还不能认定为一个干净、可靠、已验证可用于本仓库的安装。

### 用 CocoaPods 思路理解这一套依赖

如果你更熟悉 iOS / CocoaPods，可以先按下面的方式建立心智模型：

| 当前项目里的概念 | 可类比的 CocoaPods 概念 | 说明 |
|---|---|---|
| `pnpm` | `pod install` + CocoaPods 依赖解析器 | 负责读取依赖声明、解析版本、安装 JS 依赖。 |
| `package.json` | `Podfile` | 声明当前项目依赖哪些包、需要哪些脚本命令。 |
| `pnpm-lock.yaml` | `Podfile.lock` | 锁定实际安装版本，保证团队一致性。 |
| `node_modules/` | `Pods/` 目录 | 安装完成后的本地依赖实体。没有它，项目通常跑不起来。 |
| `.pnpm-store/` | 本机 CocoaPods 缓存 | 类似下载过的 pod 缓存，删掉后可以重新拉取。 |
| `rustc` / `cargo` | `xcodebuild` + 原生编译工具链 | 这里不是纯前端项目，Tauri 壳层和原生命令由 Rust 负责编译。 |
| `rustup` | Ruby / CocoaPods 的版本管理入口 | 用来安装和管理 Rust 工具链，类似你用版本管理器维护 Ruby。 |
| `just` | 项目里的脚本封装命令 | 类似团队常写的 `make`、`rake` 或 shell 脚本入口，把多条命令封装成统一入口。 |
| `mise` | `rbenv` / `asdf` 这类多语言版本管理器 | 帮项目统一管理 `node`、`pnpm`、`rust`、`just` 的版本。 |

你可以先把这个仓库理解成：

- `package.json + pnpm-lock.yaml` 负责前端依赖，类似 `Podfile + Podfile.lock`
- `node_modules/` 是前端世界里的 `Pods/`
- `Cargo.toml` 则更像另一套原生依赖声明，相当于项目里同时有一层“前端依赖系统”和一层“原生依赖系统”

## 当前缺失项

对于“本地桌面开发运行”这个目标，目前的阻塞项是：

1. 没有安装 Rust 工具链。
2. 没有安装 Cargo。
3. 没有安装 `just`，因此仓库默认入口如 `just setup`、`just dev` 目前不可用。
4. 仓库内没有 `node_modules/`，说明 JS 依赖尚未安装。
5. `pnpm` 虽然存在，但由于观察到了 registry fetch 失败，正式使用前还需要额外验证或修复。

从 CocoaPods 角度看，这相当于：

1. 你的 Xcode 工程和 Apple 编译器是好的。
2. 但除了 iOS 编译器这一层之外，这个项目还依赖另一套原生工具链，也就是 Rust；现在它还没装。
3. `node_modules/` 缺失，就像仓库里没有 `Pods/`。
4. `pnpm` 当前状态不稳定，类似你执行 `pod install` 时发现 CocoaPods 自身或 Ruby 环境有异常，这时不应该继续怀疑业务代码，应该先把依赖管理器修好。

## 最小安装路径

针对这台机器，较稳妥的安装路径是：

```bash
brew install just
curl https://sh.rustup.rs -sSf | sh
source ~/.cargo/env
pnpm install
just dev
```

如果你想按仓库的工具声明方式管理环境，而不是手动维护版本，也可以这样：

```bash
brew install mise just
mise install
curl https://sh.rustup.rs -sSf | sh
source ~/.cargo/env
pnpm install
just dev
```

补充说明：

- 仓库里的 `mise.toml` 已声明 `just`、`node`、`pnpm` 和 `rust`，但本机尚未安装 `mise`。
- 在 macOS 上，仓库的 `just setup` 还会补充 Apple 通用构建所需的 Rust target。

如果继续用 CocoaPods 类比，这条安装路径可以这样理解：

1. `brew install just`：先把项目约定的“统一脚本入口”装上。
2. `rustup` 安装 Rust：补上这个项目额外需要的一整套原生编译工具链。
3. `pnpm install`：相当于执行一次 `pod install`，把当前仓库依赖真正落到本地。
4. `just dev`：相当于执行团队封装好的启动脚本，而不是手写一串底层命令。

## 安装后的建议验证步骤

完成安装后，建议按下面顺序验证：

```bash
rustc --version
cargo --version
just --version
pnpm -v
test -d node_modules && echo installed || echo missing
just dev
```

如果 `pnpm -v` 仍然尝试访问 registry，或者继续失败，应该先修复 `pnpm`，再继续处理应用本身。

这和 iOS 项目里“先确保 `pod install` 能稳定执行，再看业务编译错误”是一个思路。

## 如何构建最终生成产物

本项目有三类常见构建产物，目标不同，命令也不同：

| 目标 | 推荐命令 | 主要产物 |
|---|---|---|
| 仅构建前端静态资源 | `just frontend-build` | `dist/` |
| 构建桌面安装包 / `.app` | `just tauri-build` | `src-tauri/target/*/release/bundle/` |
| 构建 WebUI server 单文件二进制 | `just serve-build` | `src-tauri/target/release/claude-code-history-viewer` |

### A. 构建桌面最终产物

如果目标是生成用户可安装或可分发的桌面应用，使用：

```bash
just tauri-build
```

在 macOS 上，`justfile` 会执行：

```bash
tauri build --target universal-apple-darwin
```

也就是说，默认会尝试构建 universal macOS 产物。构建前建议先确保两个 Apple target 已安装：

```bash
rustup target add x86_64-apple-darwin
rustup target add aarch64-apple-darwin
```

如果已经执行过 `just setup`，这两个 target 会在 macOS 的 post setup 阶段自动补齐。

最终产物通常位于：

```text
src-tauri/target/universal-apple-darwin/release/bundle/
```

其中常见文件包括：

- `.app`：macOS 应用包
- `.dmg`：可分发安装镜像
- updater 相关产物：由 `src-tauri/tauri.conf.json` 中的 `bundle.createUpdaterArtifacts=true` 控制

如果不是 universal 构建，产物会落在对应 target 的 release bundle 目录，例如：

```text
src-tauri/target/release/bundle/
```

### B. 构建前端静态资源

如果只想验证前端 production build，使用：

```bash
just frontend-build
```

这个命令会先执行：

```bash
node scripts/sync-version.cjs
```

然后运行 TypeScript 和 Vite 构建，产物在：

```text
dist/
```

注意：`dist/` 不是完整桌面安装包，只是 Tauri 打包前需要嵌入的前端静态资源。

### C. 构建 WebUI server 产物

如果目标是无桌面环境的 server 模式，使用：

```bash
just serve-build
```

这个命令会先构建前端，再用 Rust release 模式构建带 `webui-server` feature 的二进制：

```bash
cd src-tauri && cargo build --release --features webui-server
```

主要产物是：

```text
src-tauri/target/release/claude-code-history-viewer
```

可以用下面命令构建并立即运行：

```bash
just serve-build-run
```

也可以在已经构建过之后直接运行现有二进制：

```bash
just serve-run
```

### D. 发布前建议检查顺序

发布或交付最终产物前，建议至少执行：

```bash
pnpm lint
pnpm build
cargo check --manifest-path src-tauri/Cargo.toml
git diff --check
just tauri-build
```

如果本次只改了版本号，仍然建议让 `just tauri-build` 走完整链路，因为它会同步版本、构建前端、再执行 Tauri 打包。当前版本号的单一来源是 `package.json`，`src-tauri/Cargo.toml` 和 `src-tauri/tauri.conf.json` 由 `scripts/sync-version.cjs` 同步。

## 未来如何清理这些依赖

这里分成两个层级：仓库级清理和系统级清理。

### A. 仓库级清理

这部分只删除当前仓库本地生成的依赖和构建产物：

```bash
rm -rf node_modules
rm -rf dist
rm -rf src-tauri/target
rm -rf .pnpm-store
```

适用场景：

- 想回收仓库占用空间
- 想把当前仓库恢复到“重新安装依赖”的状态
- 怀疑本地缓存或构建产物已经污染

如果类比 CocoaPods，这一步最接近：

- 删除 `Pods/`
- 删除构建产物
- 然后重新执行一次依赖安装

### B. pnpm 缓存清理

优先建议做裁剪，而不是直接硬删：

```bash
pnpm store prune
```

如果你明确使用的是仓库内本地 store，也可以直接删：

```bash
rm -rf .pnpm-store
```

这可以理解成删除本地 pod 缓存。好处是能强制重新下载，坏处是下一次安装会更慢。

### C. 卸载 `just`

如果是通过 Homebrew 安装的：

```bash
brew uninstall just
```

如果是通过 Cargo 安装的：

```bash
cargo uninstall just
```

这和删除团队自定义脚本工具类似，本身不是业务依赖，但会影响你是否还能继续用项目约定的快捷命令。

### D. 卸载 Rust 工具链

如果通过 `rustup` 安装，标准卸载方式是：

```bash
rustup self uninstall
```

如果卸载后仍要手动清理残留：

```bash
rm -rf ~/.cargo
rm -rf ~/.rustup
```

只有在你确认这台机器不再需要 Rust 工具链和 Cargo 安装过的命令时，才建议这么做。

这一步的影响比删除 `Pods/` 大得多，更像把整套原生构建环境一起移除，而不是只清仓库缓存。

### E. 卸载 `mise`

如果是通过 Homebrew 安装的：

```bash
brew uninstall mise
```

如果你希望把 `mise` 的元数据也一起清掉：

```bash
rm -rf ~/.local/share/mise
rm -rf ~/.cache/mise
rm -rf ~/.config/mise
```

把它理解为移除版本管理器本体和它记录的工具版本索引即可。

### F. 卸载 Node.js 和全局 pnpm

本机当前使用的是 Homebrew 安装的 Node，以及位于 `/opt/homebrew/lib/node_modules` 下的全局 `pnpm`。

如果只想删除全局 `pnpm`：

```bash
npm uninstall -g pnpm
```

如果要把 Homebrew 的 Node 一起移除：

```bash
brew uninstall node
```

之后如果还有残留目录，而且你确认不再需要它们，可以再手动删除：

```bash
rm -rf /opt/homebrew/lib/node_modules/pnpm
```

这一步应该放在正常卸载之后，并且只在路径仍然存在时执行。

从使用感受上，它更像：

- `brew uninstall cocoapods`
- 再手动删全局 gem 或缓存残留

### G. Xcode / Apple 工具链

这个仓库在 macOS 上依赖 Apple 开发工具链。除非你明确要影响整台机器上的原生构建环境，否则不建议把 Xcode 或 Command Line Tools 当作仓库级清理对象。

通常这不是本仓库专属的清理项。

## 实际建议

对这台机器来说，最短可行路径是：

1. 安装 Rust。
2. 安装 `just`。
3. 重新验证 `pnpm` 是否稳定可用。
4. 运行 `pnpm install`。
5. 使用 `just dev` 启动。

如果完全按 CocoaPods 的学习路径来记，可以简化成一句话：

1. 先把“依赖管理器本身”修到可用。
2. 再把“原生编译工具链”补齐。
3. 然后安装仓库依赖。
4. 最后再启动项目。

未来如果只是想清理本仓库，最安全的默认做法是先删仓库本地产物：

```bash
rm -rf node_modules dist src-tauri/target .pnpm-store
```

只有在你明确要回滚整机工具链时，再去删除系统级依赖。
