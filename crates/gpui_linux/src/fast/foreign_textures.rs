//! Share the active Linux window renderer with immutable GPU image producers.
use std::{any::Any, sync::Arc};

pub(crate) fn gpu_context(
    renderer: Option<&gpui_wgpu::WgpuRenderer>,
) -> Option<Arc<dyn Any + Send + Sync>> {
    Some(Arc::new(renderer?.shared_context()?))
}
