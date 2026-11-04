//! API adaptation for the published wgpu version used by downstream renderers.
pub(crate) fn is_srgb(format: &wgpu::TextureFormat) -> bool {
    format.is_srgb()
}
