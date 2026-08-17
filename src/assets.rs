use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

/// daku ships no bundled assets; GPUI still needs an `AssetSource`.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, _path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(None)
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}
