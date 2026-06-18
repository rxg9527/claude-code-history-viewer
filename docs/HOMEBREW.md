# Homebrew Maintainer Guide

This fork publishes two Homebrew entries through `rxg9527/homebrew-tap`:

- Desktop app cask: `brew install --cask rxg9527/tap/claude-code-history-viewer`
- Headless WebUI server formula: `brew install rxg9527/tap/cchv-server`

## Architecture

```text
Tag push
  -> updater-release.yml
     -> build desktop app assets
     -> generate latest.json for the Tauri updater
     -> publish GitHub Release
     -> update Casks/claude-code-history-viewer.rb in rxg9527/homebrew-tap

  -> server-release.yml
     -> build server tarballs
     -> upload server assets and CHECKSUMS.sha256
     -> update Formula/cchv-server.rb in rxg9527/homebrew-tap
```

The desktop cask update runs after the GitHub Release is published. If the cask sync fails, the release assets remain available and the workflow failure should be fixed by updating the tap token or cask file.

## Required Secret

`HOMEBREW_TAP_TOKEN` must be configured in this repository's **Repository secrets**.

Create a fine-grained GitHub token with:

- Repository access: `rxg9527/homebrew-tap`
- Permissions: Contents read and write

Then add it as:

```text
Settings -> Secrets and variables -> Actions -> Repository secrets
HOMEBREW_TAP_TOKEN
```

The same token is used by both desktop cask and server formula automation.

## Desktop Cask

Location in the tap repository:

```text
Casks/claude-code-history-viewer.rb
```

The release workflow:

1. Finds the macOS `.dmg` asset from the current GitHub Release, preferring the universal build.
2. Downloads the DMG and computes SHA256.
3. Updates `version`, `sha256`, and `url` in the tap cask.
4. Commits the change directly to `rxg9527/homebrew-tap`.

The cask includes `auto_updates true` because the app also has the built-in Tauri updater. Homebrew may skip normal upgrades unless the user runs `brew upgrade --greedy` or the installed app has not already updated itself.

macOS desktop builds in this fork are ad-hoc signed and not notarized. First launch may require right-click > Open or approval in System Settings > Privacy & Security.

## Server Formula

Location in the tap repository:

```text
Formula/cchv-server.rb
```

The server workflow:

1. Builds server tarballs for macOS arm64/x64 and Linux arm64/x64.
2. Uploads the tarballs to the same GitHub Release.
3. Generates and uploads `CHECKSUMS.sha256`.
4. Updates `version` and per-platform SHA256 values in the tap formula.

## Manual Desktop Cask Update

Use this only if workflow automation fails after a release was already published.

```bash
VERSION="1.13.2"
ASSET="Claude.Code.History.Viewer_1.13.1_universal.dmg"
URL="https://github.com/rxg9527/claude-code-history-viewer/releases/download/v${VERSION}/${ASSET}"

curl -fsSL "$URL" -o /tmp/cchv.dmg
shasum -a 256 /tmp/cchv.dmg
```

Then update `Casks/claude-code-history-viewer.rb` in the tap repository:

```ruby
version "1.13.2"
sha256 "<computed-sha256>"
url "https://github.com/rxg9527/claude-code-history-viewer/releases/download/v1.13.2/Claude.Code.History.Viewer_1.13.1_universal.dmg"
```

Commit and push the tap change:

```bash
git add Casks/claude-code-history-viewer.rb
git commit -m "chore: update claude-code-history-viewer cask to v${VERSION}"
git push
```

## Verification Commands

```bash
brew tap rxg9527/tap

brew info --cask rxg9527/tap/claude-code-history-viewer
brew audit --cask rxg9527/tap/claude-code-history-viewer
brew install --cask rxg9527/tap/claude-code-history-viewer --dry-run

brew info rxg9527/tap/cchv-server
brew audit --formula rxg9527/tap/cchv-server
brew install rxg9527/tap/cchv-server --dry-run
```

## Troubleshooting

### `HOMEBREW_TAP_TOKEN` is missing

The Homebrew update job fails with an error saying the secret is required. Add the token under repository-level Actions secrets.

### Cask SHA256 mismatch

Re-download the exact DMG URL from the cask and recompute:

```bash
curl -fsSL "<dmg-url>" -o /tmp/cchv.dmg
shasum -a 256 /tmp/cchv.dmg
```

Update the cask with the new checksum and push the tap commit.

### Cask not found

Refresh the tap:

```bash
brew untap rxg9527/tap
brew tap rxg9527/tap
brew info --cask rxg9527/tap/claude-code-history-viewer
```

### macOS blocks first launch

This is expected for ad-hoc signed, non-notarized desktop builds. Use right-click > Open, or allow the app in System Settings > Privacy & Security.
