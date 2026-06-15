# Fork Updater 签名接入指南

本文档只针对当前 fork：

- 发布源仓库：`rxg9527/claude-code-history-viewer`
- 第一阶段不保留 Homebrew
- 保留 Tauri updater

目标是让 fork 自己的 GitHub Release + `latest.json` + updater 签名链成立。

## 需要准备什么

你需要准备自己的 Tauri updater 密钥对：

- 公钥：写进 `src-tauri/tauri.conf.json`
- 私钥：不要进仓库，放到 GitHub Actions secrets

当前仓库里 `src-tauri/tauri.conf.json` 仍保留的是上游公钥。  
在你生成并替换自己的公钥之前，fork 的 updater 仍然不能作为独立产品成立。

## 生成 updater 密钥对

在仓库根目录执行：

```bash
./node_modules/.bin/tauri signer generate -w ~/.tauri/rxg9527-cchv.key
```

命令会：

- 生成一个私钥文件到 `~/.tauri/rxg9527-cchv.key`
- 输出对应的公钥文本

如果你希望私钥带密码，可以加：

```bash
./node_modules/.bin/tauri signer generate \
  -w ~/.tauri/rxg9527-cchv.key \
  -p 'your-password'
```

## 你要保存哪些内容

生成后请分别保存：

1. **私钥文件内容**
   - 来源：`~/.tauri/rxg9527-cchv.key`
   - 用途：GitHub Actions secret `TAURI_SIGNING_PRIVATE_KEY`

2. **私钥密码**
   - 如果你生成时设置了密码
   - 用途：GitHub Actions secret `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

3. **公钥文本**
   - 来源：命令输出
   - 用途：替换 `src-tauri/tauri.conf.json` 里的 `plugins.updater.pubkey`

## 在 fork 仓库里配置的 GitHub Secrets

到 fork 仓库 `rxg9527/claude-code-history-viewer` 的 GitHub Settings -> Secrets and variables -> Actions，至少配置：

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`（如果你的私钥带密码）

如果保留 macOS 正式签名 / notarization，还需要你自己的 Apple 相关 secrets。  
但这和 updater 私钥是两回事，不能混用。

## 替换公钥

生成完密钥对后，把新的公钥替换到：

- [tauri.conf.json](../src-tauri/tauri.conf.json)

目标字段：

```json
"plugins": {
  "updater": {
    "pubkey": "..."
  }
}
```

注意：

- 这里必须是**你的新公钥**
- 不能继续保留上游公钥
- 否则 fork 的 release 即使签名了，客户端也验不过

## 什么时候算接入完成

至少满足以下条件：

1. fork 仓库已经配置好 `TAURI_SIGNING_PRIVATE_KEY`
2. `tauri.conf.json` 已替换成 fork 自己的 updater 公钥
3. 推送 `v*` tag 后，release workflow 能成功生成：
   - 各平台资产
   - 对应 `.sig`
   - `latest.json`
4. 本地运行 fork 构建出来的 app，能够正确检查 fork 自己 release 的更新

## 当前阶段边界

这份文档只解决：

- fork 自己的 updater 密钥
- fork 自己的 release 签名链

它不覆盖：

- 品牌重命名
- bundle identifier 更换
- URL scheme 更换
- Homebrew 分发恢复
