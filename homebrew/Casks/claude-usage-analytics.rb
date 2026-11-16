# Homebrew cask for Claude Usage Analytics.
# Lives in the tap repository jravas/homebrew-tap as Casks/claude-usage-analytics.rb.
# Update `version` and `sha256` for each release; `brew fetch --cask` prints the hash.
cask "claude-usage-analytics" do
  arch arm: "aarch64", intel: "x64"

  version "0.1.0"
  sha256 arm:   "REPLACE_ARM_SHA256",
         intel: "REPLACE_INTEL_SHA256"

  url "https://github.com/jravas/claude-usage-analytics/releases/download/v#{version}/Claude.Usage.Analytics_#{version}_#{arch}.dmg"
  name "Claude Usage Analytics"
  desc "See where your Claude Code tokens go"
  homepage "https://github.com/jravas/claude-usage-analytics"

  depends_on macos: ">= :ventura"

  app "Claude Usage Analytics.app"

  zap trash: [
    "~/Library/Application Support/digital.prototyp.claude-usage-analytics",
    "~/Library/LaunchAgents/Claude Usage Analytics.plist",
  ]
end
