use std::borrow::Cow;

use anyhow::{Result, anyhow};
use rust_embed::RustEmbed;
use warpui_core::AssetProvider;

#[derive(Clone, Copy, RustEmbed)]
#[folder = "../../app/assets"]
#[include = "bundled/**"] // Should be kept in sync with BUNDLED_ASSETS_DIR.
#[include = "async/**"] // Should be kept in sync with ASYNC_ASSETS_DIR.
#[cfg_attr(target_family = "wasm", exclude = "async/**")]
// Excludes take precedence.
// Standalone CLI builds (the `oz` tarball) are headless and never render the
// onboarding/theme imagery in `async/`, so we exclude those bytes from the
// embedded asset set to keep the CLI binary small — mirroring the carve-out
// already applied for the WASM target above.
#[cfg_attr(feature = "standalone", exclude = "async/**")]
// light-warp runs offline, where the welcome slides never show and the cloud agent modals cannot
// open. Their files stay in the repo because the asset macros check paths at compile time.
#[exclude = "async/png/onboarding/agent_intention/**"]
#[exclude = "async/png/onboarding/terminal_intention/**"]
#[exclude = "async/png/onboarding/onboarding_bg.png"]
#[exclude = "async/png/onboarding/welcome_*.png"]
#[exclude = "async/png/onboarding/thirdparty_*.png"]
#[exclude = "async/png/onboarding/auto_handoff_sleep_banner.png"]
#[exclude = "async/png/onboarding/orchestration_launch_banner.png"]
#[exclude = "async/png/oz_*.png"]
#[exclude = "async/png/concurrency_limit_header.png"]
#[exclude = "async/png/build_spiral.png"]
pub struct Assets;

impl AssetProvider for Assets {
    fn get(&self, path: &str) -> Result<Cow<'_, [u8]>> {
        <Assets as RustEmbed>::get(path)
            .map(|f| f.data)
            .ok_or_else(|| anyhow!("no asset exists at path {}", path))
    }
}
