//! Immutable GPU-backed images using the ordinary sprite atlas and paint path.
use crate::{
    AtlasKey, AtlasState, AtlasTile, DevicePixels, HeadlessAtlas, ImageId, PlatformAtlas,
    RenderImage, RenderImageParams, Result, Size, Window, size,
};
use anyhow::Context as _;
use smallvec::SmallVec;
use std::collections::hash_map::Entry;
use std::{
    any::Any,
    borrow::Cow,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
};

pub(crate) struct GpuImage {
    pub(crate) size: Size<DevicePixels>,
    pub(crate) payload: Arc<dyn Any + Send + Sync>,
}

pub(crate) fn next_image_id() -> ImageId {
    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
    ImageId(NEXT_ID.fetch_add(1, SeqCst))
}

impl RenderImage {
    /// Retain a backend-specific GPU image without transferring pixels to the CPU.
    /// The renderer must support the payload type and share its originating device.
    pub fn from_gpu(size: Size<DevicePixels>, payload: Arc<dyn Any + Send + Sync>) -> Self {
        Self {
            id: next_image_id(),
            scale_factor: 1.0,
            data: SmallVec::new(),
            fast_gpu: Some(GpuImage { size, payload }),
        }
    }

    /// The backend-specific resource, if this image lives on the GPU.
    pub fn gpu_data(&self) -> Option<&(dyn Any + Send + Sync)> {
        self.fast_gpu.as_ref().map(|gpu| gpu.payload.as_ref())
    }
}

pub(crate) fn image_size(image: &RenderImage, frame_index: usize) -> Size<DevicePixels> {
    if let Some(GpuImage { size, .. }) = &image.fast_gpu {
        return if frame_index == 0 {
            *size
        } else {
            Size::default()
        };
    }
    image
        .data
        .get(frame_index)
        .map(|frame| {
            let (width, height) = frame.buffer().dimensions();
            size(width.into(), height.into())
        })
        .unwrap_or_default()
}

pub(crate) fn frame_count(image: &RenderImage) -> usize {
    if image.fast_gpu.is_some() {
        1
    } else {
        image.data.len()
    }
}

impl<Backend> AtlasState<Backend> {
    pub fn get_or_insert_tile(
        &mut self,
        key: AtlasKey,
        build: impl FnOnce(&mut Backend) -> Result<AtlasTile>,
    ) -> Result<AtlasTile> {
        match self.tiles_by_key.entry(key) {
            Entry::Occupied(entry) => Ok(*entry.get()),
            Entry::Vacant(entry) => {
                let tile = build(&mut self.backend)?;
                entry.insert(tile);
                Ok(tile)
            }
        }
    }
}

impl Window {
    /// Backend-specific shared GPU context for producing retained images.
    pub fn gpu_context(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        self.platform_window.gpu_context()
    }
}

pub(crate) fn unavailable_context() -> Option<Arc<dyn Any + Send + Sync>> {
    None
}

pub(crate) fn unsupported_image() -> Result<Option<AtlasTile>> {
    anyhow::bail!("this renderer does not support GPU images")
}

pub(crate) fn headless_gpu_image(
    atlas: &HeadlessAtlas,
    image: &RenderImage,
) -> Result<Option<AtlasTile>> {
    atlas.get_or_insert_with(
        RenderImageParams {
            image_id: image.id,
            frame_index: 0,
        }
        .into(),
        &mut || Ok(Some((image.size(0), Cow::Borrowed(&[])))),
    )
}

pub(crate) fn image_tile(
    window: &Window,
    data: &RenderImage,
    frame_index: usize,
) -> Result<AtlasTile> {
    let atlas = window.sprite_atlas.as_ref();
    let params = RenderImageParams {
        image_id: data.id,
        frame_index,
    };

    if data.gpu_data().is_some() {
        anyhow::ensure!(frame_index == 0, "GPU images have only one frame");
        atlas.get_or_insert_gpu_image(data)?
    } else {
        atlas.get_or_insert_with(params.into(), &mut || {
            Ok(Some((
                data.size(frame_index),
                Cow::Borrowed(
                    data.as_bytes(frame_index)
                        .expect("It's the caller's job to pass a valid frame index"),
                ),
            )))
        })?
    }
    .context("image is unavailable in the sprite atlas")
}

#[cfg(test)]
mod tests {
    use crate::{
        DevicePixels, HeadlessAtlas, PlatformAtlas, RenderImage, RenderImageParams, Size, size,
    };
    use std::sync::Arc;

    #[test]
    fn cpu_and_gpu_images_share_ids_but_only_cpu_images_expose_bytes() {
        let cpu = RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(3, 2))]);
        let gpu = RenderImage::from_gpu(size(DevicePixels(3), DevicePixels(2)), Arc::new(17u8));
        assert_ne!(cpu.id, gpu.id);
        assert_eq!(cpu.size(0), gpu.size(0));
        assert_eq!(gpu.size(1), Size::default());
        assert_eq!(gpu.frame_count(), 1);
        assert_eq!(cpu.as_bytes(0).unwrap().len(), 24);
        assert!(gpu.as_bytes(0).is_none());
        assert_eq!(gpu.gpu_data().unwrap().downcast_ref::<u8>(), Some(&17));
        assert!(cpu.gpu_data().is_none());
    }

    #[test]
    fn headless_gpu_images_follow_atlas_retirement() {
        let atlas = HeadlessAtlas::default();
        let image = RenderImage::from_gpu(size(DevicePixels(3), DevicePixels(2)), Arc::new(()));
        let first = atlas.get_or_insert_gpu_image(&image).unwrap().unwrap();
        assert_eq!(
            first,
            atlas.get_or_insert_gpu_image(&image).unwrap().unwrap()
        );
        assert_eq!(first.bounds.size, image.size(0));
        atlas.remove(
            &RenderImageParams {
                image_id: image.id,
                frame_index: 0,
            }
            .into(),
        );
        let second = atlas.get_or_insert_gpu_image(&image).unwrap().unwrap();
        assert_ne!(first.texture_id, second.texture_id);
    }
}
