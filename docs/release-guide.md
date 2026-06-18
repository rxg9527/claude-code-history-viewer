# GitHub Release 发布指南

本文档只说明这个仓库当前实际使用的发布方式，不讨论通用 GitHub Release 操作。

## 核心结论

这个仓库的发布机制是：

- `git tag` 是发布触发器
- GitHub Actions 是发布执行器
- GitHub Release 页面主要是结果承载，不是主要操作入口

也就是说，**正常发布不是手动去 Release 页面上传文件**，而是推送版本 tag，让 workflow 自动构建、上传资产、生成更新元数据并最终发布。

## 相关文件

- 主桌面版发布流程：
  - [updater-release.yml](../.github/workflows/updater-release.yml)
- WebUI server 发布流程：
  - [server-release.yml](../.github/workflows/server-release.yml)
- 已有 release 的补救 / 重试流程：
  - [updater-release-retry.yml](../.github/workflows/updater-release-retry.yml)
- Homebrew 分发维护说明：
  - [HOMEBREW.md](./HOMEBREW.md)
- fork 自己的 updater 密钥和 secrets 接入说明：
  - [fork-updater-setup.md](./fork-updater-setup.md)

## 触发方式

主 release workflow 和 server release workflow 的触发条件都是：

```yml
on:
  push:
    tags:
      - "v*"
```

所以你要发布一个版本，例如 `1.13.1`，实际操作是：

```bash
git tag v1.13.1
git push upstream v1.13.1
```

## 发布前置条件

至少确认以下几项：

1. 代码已经提交到正确分支，并且是你要发布的内容
2. 版本号已经准备好
3. 本地或 CI 构建不会失败
4. GitHub 仓库 secrets 已配置

其中这个仓库特别依赖：

- Tauri updater 签名相关 secrets
- `HOMEBREW_TAP_TOKEN`（用于更新 `rxg9527/homebrew-tap` 的桌面 cask 和 server formula）

macOS 桌面构建当前使用 ad-hoc signing，不依赖 Apple Developer ID 证书和 notarization secrets。首次打开仍可能被 macOS 拦截，需要用户手动允许。

## 主桌面版发布流程

[updater-release.yml](../.github/workflows/updater-release.yml) 负责桌面版发布。

它的顺序大致是：

1. 根据 tag 创建或复用一个 GitHub Release
2. 先把这个 release 建成 `draft`
3. 分平台构建桌面应用：
   - macOS universal
   - Linux x64
   - Windows x64
4. 上传各平台产物
5. 生成并上传 `latest.json`
6. 把 release 从 draft 改成 published
7. 更新 `rxg9527/homebrew-tap` 中的桌面 Homebrew cask

这意味着：

- release 一开始不会立刻公开
- 只有整条链路成功后，release 才会正式发布

## Server 发布流程

[server-release.yml](../.github/workflows/server-release.yml) 会和主 release 一起由同一个 tag 触发。

它主要做这些事：

1. 构建 `webui-server` 二进制
2. 打包成各平台 `.tar.gz`
3. 等主 release 不再是 draft
4. 把 server 资产上传到同一个 GitHub Release
5. 上传 `CHECKSUMS.sha256` 供后续校验和分发使用
6. 更新 `rxg9527/homebrew-tap` 中的 server Homebrew formula

## 一次正常发布的最短步骤

### 1. 确认版本号

确保你准备发布的版本号就是目标版本。

### 2. 提交代码

```bash
git status
git add ...
git commit -m "chore: release v1.13.1"
git push
```

### 3. 打 tag 并推送

```bash
git tag v1.13.1
git push upstream v1.13.1
```

### 4. 去 GitHub Actions 观察流程

重点看这两个 workflow：

- `Release with Updater Metadata`
- `Server Release`

## 发布后要检查什么

至少检查以下几项：

1. GitHub Releases 页面上，对应 tag 的 release 是否已经从 `draft` 变成公开状态
2. 是否包含关键资产：
   - macOS `.dmg`
   - Windows installer / portable zip
   - Linux `.AppImage`
   - `latest.json`
   - server 的 `cchv-server-*.tar.gz`
3. `latest.json` 是否存在且内容正确
4. server 的 `CHECKSUMS.sha256` 是否已上传
5. Homebrew tap 是否已更新：
   - `Casks/claude-code-history-viewer.rb`
   - `Formula/cchv-server.rb`
6. 两个 Actions workflow 是否都是绿色

## 发布失败时怎么处理

如果 release 已经创建，但最后阶段失败了，不要急着手工重发全部资产。先看仓库内置的补救流程：

- [updater-release-retry.yml](../.github/workflows/updater-release-retry.yml)

这是一个 `workflow_dispatch` 手动触发流程，需要你提供：

- `release_id`

它会补做这几件事：

1. 重新生成并上传 `latest.json`
2. 最后把 release 发布出去

适用场景：

- release 已经存在
- 资产大部分已经上传
- 但 finalize / `latest.json` 阶段失败

## 实用理解

把这个仓库的 release 流程压缩成一句话：

```text
改版本 -> 提交代码 -> 推送 v* tag -> GitHub Actions 自动构建/上传/生成 latest.json -> release 正式发布
```

如果你只是偶尔发布一次，这一句就够了。
