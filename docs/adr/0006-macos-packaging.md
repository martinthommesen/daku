# Packaging: DMG+Sparkle primary, Homebrew alternate

v1 ships as a **notarised macOS `.app` / DMG** with **Sparkle** auto-updates (inherit waku’s release path; needs Apple Developer ID). **Homebrew cask** is an alternate install that upgrades via `brew` — Sparkle is off or no-op in cask builds so the two channels do not fight.

The repo is **public GPL-3.0-only**; handing a colleague a build is intentional distribution with source on GitHub.
