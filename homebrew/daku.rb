# frozen_string_literal: true

# Draft Homebrew cask. Installs the channel-homebrew DMG so Sparkle is a
# compile-time no-op and `brew upgrade` owns updates.
# Build that artifact with: DAKU_CHANNEL=homebrew ./scripts/bundle.sh --unsigned
cask "daku" do
  version "0.1.0"
  sha256 :no_check

  url "https://github.com/martinthommesen/daku/releases/download/v#{version}/Daku-#{version}-homebrew.dmg"
  name "Daku"
  desc "Operator console for ServiceNow Environments"
  homepage "https://github.com/martinthommesen/daku"

  auto_updates false
  depends_on macos: ">= :ventura"

  app "Daku.app"

  # ~/.daku is Operator config/db — leave it on uninstall.
  zap trash: [
    "~/Library/Caches/app.daku",
    "~/Library/Preferences/app.daku.plist",
  ]
end
