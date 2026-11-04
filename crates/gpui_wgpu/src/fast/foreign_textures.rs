//! Import immutable images produced on the renderer's shared wgpu device.
use crate::wgpu_atlas::{WgpuAtlasTexture, WgpuAtlasTextures, device_size_to_etagere};
use crate::{WgpuAtlas, WgpuContext, WgpuRenderer};
use anyhow::{Context as _, Result};
use collections::FxHashSet;
use etagere::BucketedAtlasAllocator;
use gpui::{
    AtlasTextureId, AtlasTextureKind, AtlasTile, Bounds, DevicePixels, Point, PrimitiveBatch,
    RenderImage, RenderImageParams, Scene, Size, TileId,
};
use std::sync::Arc;

/// A sampled, straight-alpha RGBA or BGRA image on the renderer's device.
/// The producer must submit writes to the shared queue before painting this image,
/// and must not mutate the texture while GPUI can still display it.
#[derive(Clone)]
pub struct WgpuImage {
    pub device: Arc<wgpu::Device>,
    pub texture: wgpu::Texture,
}

/// Device and queue shared with a window renderer. Submit image production before painting.
/// A changed device identity invalidates all previously produced GPU images.
#[derive(Clone)]
pub struct SharedGpuContext {
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    pub adapter: wgpu::AdapterInfo,
}

pub(crate) fn import_image(atlas: &WgpuAtlas, image: &RenderImage) -> Result<Option<AtlasTile>> {
    let gpu = image
        .gpu_data()
        .and_then(|data| data.downcast_ref::<WgpuImage>())
        .context("expected a WgpuImage payload")?;
    let mut state = atlas.0.lock();
    anyhow::ensure!(
        Arc::ptr_eq(&gpu.device, &state.backend.device),
        "GPU image belongs to a different device"
    );
    let texture = &gpu.texture;
    anyhow::ensure!(
        texture.dimension() == wgpu::TextureDimension::D2
            && texture.depth_or_array_layers() == 1
            && texture.sample_count() == 1
            && texture.mip_level_count() == 1
            && texture
                .usage()
                .contains(wgpu::TextureUsages::TEXTURE_BINDING)
            && matches!(
                texture.format(),
                wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
            ),
        "GPU image must be a sampleable single-level RGBA8/BGRA8 2D texture"
    );
    let size = image.size(0);
    anyhow::ensure!(
        size.width.0 > 0
            && size.height.0 > 0
            && size.width.0 as u32 == texture.width()
            && size.height.0 as u32 == texture.height(),
        "GPU image dimensions do not match its texture"
    );
    let key = RenderImageParams {
        image_id: image.id,
        frame_index: 0,
    }
    .into();
    state
        .get_or_insert_tile(key, |backend| {
            let kind = AtlasTextureKind::Polychrome;
            let list = &mut backend.storage[kind];
            let index = list.free_list.pop().unwrap_or(list.textures.len());
            let id = AtlasTextureId {
                kind,
                index: index as u32,
            };
            let generation = backend.next_texture_generation;
            backend.next_texture_generation = generation.wrapping_add(1);
            let entry = WgpuAtlasTexture {
                id,
                generation,
                fast_allocator: None,
                texture: texture.clone(),
                view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
                format: texture.format(),
                live_atlas_keys: 1,
            };
            if index == list.textures.len() {
                list.textures.push(Some(entry));
            } else {
                list.textures[index] = Some(entry);
            }
            Ok(AtlasTile {
                texture_id: id,
                // Scroll-layer hashes include tile_id, but not the atlas texture's
                // generation. A recycled texture slot must change both identities.
                tile_id: TileId(generation as u32),
                padding: 0,
                bounds: Bounds {
                    origin: Point::default(),
                    size,
                },
            })
        })
        .map(Some)
}

pub(crate) type AtlasAllocator = Option<BucketedAtlasAllocator>;

pub(crate) fn allocator(size: Size<DevicePixels>) -> AtlasAllocator {
    Some(BucketedAtlasAllocator::new(device_size_to_etagere(size)))
}

pub(crate) fn allocate(
    allocator: &mut AtlasAllocator,
    size: Size<DevicePixels>,
) -> Option<etagere::Allocation> {
    allocator.as_mut()?.allocate(device_size_to_etagere(size))
}

pub(crate) fn deallocate(allocator: &mut AtlasAllocator, id: TileId) {
    if let Some(allocator) = allocator {
        allocator.deallocate(id.into());
    }
}

pub(crate) fn reset_texture_limit(textures: &mut WgpuAtlasTextures, context: &WgpuContext) {
    textures.max_texture_size = context.device.limits().max_texture_dimension_2d;
}

#[cfg(not(target_family = "wasm"))]
pub(crate) fn native_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    // Compute-based image producers need WebGPU's standard storage-buffer limits.
    // Keep downlevel adapters usable by the UI, even if they cannot produce images.
    let limits = if wgpu::Limits::default().check_limits(&adapter.limits()) {
        wgpu::Limits::default()
    } else {
        wgpu::Limits::downlevel_defaults()
    };
    limits
        .using_resolution(adapter.limits())
        .using_alignment(adapter.limits())
}

impl WgpuRenderer {
    /// The active renderer's device and queue. Device identity changes on recovery.
    pub fn shared_context(&self) -> Option<crate::SharedGpuContext> {
        if self.device_lost() {
            return None;
        }
        let core = self.core()?;
        Some(crate::SharedGpuContext {
            device: core.resources.device.clone(),
            queue: core.resources.queue.clone(),
            adapter: core.adapter_info.clone(),
        })
    }
}

#[cfg(all(
    not(target_family = "wasm"),
    any(test, feature = "bench-support", feature = "test-support")
))]
impl crate::WgpuHeadlessRenderer {
    pub fn sprite_atlas(&self) -> &Arc<WgpuAtlas> {
        &self.core.atlas
    }
    /// The headless renderer's device, for testing GPU image producers.
    pub fn shared_context(&self) -> crate::SharedGpuContext {
        crate::SharedGpuContext {
            device: self.core.resources.device.clone(),
            queue: self.core.resources.queue.clone(),
            adapter: self.core.adapter_info.clone(),
        }
    }
}

pub(crate) fn texture_ids(scene: &Scene) -> FxHashSet<AtlasTextureId> {
    let mut texture_ids = FxHashSet::default();
    for batch in scene.batches() {
        let texture_id = match batch {
            PrimitiveBatch::MonochromeSprites { texture_id, .. }
            | PrimitiveBatch::SubpixelSprites { texture_id, .. }
            | PrimitiveBatch::PolychromeSprites { texture_id, .. } => texture_id,
            _ => continue,
        };
        texture_ids.insert(texture_id);
    }
    texture_ids
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use crate::{WgpuHeadlessRenderer, wgpu};
    use gpui::{
        BorderStyle, Bounds, ContentMask, Corners, DevicePixels, Edges, Hsla,
        PlatformHeadlessRenderer, Point, PolychromeSprite, Quad, Scene, Size,
    };
    use std::sync::Arc;
    fn device_size(width: i32, height: i32) -> Size<DevicePixels> {
        gpui::size(width.into(), height.into())
    }
    fn solid_quad(x: f32, y: f32, width: f32, height: f32, color: Hsla) -> Quad {
        let bounds = Bounds {
            origin: gpui::point(x.into(), y.into()),
            size: gpui::size(width.into(), height.into()),
        };
        Quad {
            order: 0,
            border_style: BorderStyle::Solid,
            bounds,
            content_mask: ContentMask { bounds },
            background: color.into(),
            border_color: color,
            corner_radii: Corners::default(),
            border_widths: Edges::default(),
        }
    }
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    fn assert_pixel(image: &image::RgbaImage, x: u32, y: u32, expected: [u8; 4]) {
        let actual = image.get_pixel(x, y).0;
        assert!(
            actual.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 3),
            "{actual:?} != {expected:?}"
        );
    }

    #[test]
    fn foreign_images_match_uploads_with_clipping_opacity_and_resizing() -> gpui::Result<()> {
        use crate::WgpuImage;
        use gpui::{PlatformAtlas, RenderImage, RenderImageParams};
        let mut renderer = WgpuHeadlessRenderer::new()?;
        let context = renderer.shared_context();
        let atlas = renderer.sprite_atlas().clone();
        for format in [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Bgra8Unorm,
        ] {
            let texture = context.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("foreign image test"),
                size: wgpu::Extent3d {
                    width: 4,
                    height: 4,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let rgba = [240, 40, 80, 128];
            let bgra = [80, 40, 240, 128];
            let bytes = if format == wgpu::TextureFormat::Rgba8Unorm {
                rgba
            } else {
                bgra
            }
            .repeat(16);
            context.queue.write_texture(
                texture.as_image_copy(),
                &bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(16),
                    rows_per_image: None,
                },
                texture.size(),
            );
            // Flush producer writes before handing the immutable image to GPUI.
            context.queue.submit([]);
            let image = RenderImage::from_gpu(
                device_size(4, 4),
                Arc::new(WgpuImage {
                    device: context.device.clone(),
                    texture,
                }),
            );
            let foreign = atlas.get_or_insert_gpu_image(&image)?.unwrap();
            assert_eq!(foreign, atlas.get_or_insert_gpu_image(&image)?.unwrap());
            assert!(image.as_bytes(0).is_none());
            let cpu = RenderImage::new(vec![image::Frame::new(
                image::RgbaImage::from_raw(4, 4, bgra.repeat(16)).unwrap(),
            )]);
            let cpu_key: gpui::AtlasKey = RenderImageParams {
                image_id: cpu.id,
                frame_index: 0,
            }
            .into();
            let uploaded = atlas
                .get_or_insert_with(cpu_key.clone(), &mut || {
                    Ok(Some((
                        cpu.size(0),
                        std::borrow::Cow::Borrowed(cpu.as_bytes(0).unwrap()),
                    )))
                })?
                .unwrap();
            for size in [
                device_size(32, 16),
                device_size(65, 33),
                device_size(32, 16),
            ] {
                let scene = |tile| {
                    let mut scene = Scene::default();
                    scene.insert_primitive(solid_quad(
                        0.,
                        0.,
                        size.width.0 as f32,
                        size.height.0 as f32,
                        gpui::blue(),
                    ));
                    let bounds = Bounds {
                        origin: Point::default(),
                        size: Size {
                            width: (size.width.0 as f32).into(),
                            height: (size.height.0 as f32).into(),
                        },
                    };
                    scene.insert_primitive(PolychromeSprite {
                        order: 0,
                        pad: 0,
                        grayscale: false.into(),
                        opacity: 0.5,
                        bounds,
                        content_mask: ContentMask {
                            bounds: Bounds {
                                origin: Point {
                                    x: 8f32.into(),
                                    y: 4f32.into(),
                                },
                                size: Size {
                                    width: 16f32.into(),
                                    height: bounds.size.height - 8f32.into(),
                                },
                            },
                        },
                        corner_radii: Corners::default(),
                        tile,
                    });
                    scene.finish();
                    scene
                };
                let actual = renderer.render_scene_to_image(&scene(foreign), size)?;
                let expected = renderer.render_scene_to_image(&scene(uploaded), size)?;
                // Compare the interior: packed CPU atlas tiles bleed into their
                // empty padding at outer edges; whole GPU textures clamp correctly.
                assert!(
                    actual
                        .as_raw()
                        .iter()
                        .zip(expected.as_raw())
                        .all(|(a, b)| a.abs_diff(*b) <= 1)
                );
                assert_pixel(&actual, 2, 4, BLUE);
                assert_pixel(&actual, 12, 4, [60, 10, 211, 255]);
            }
            let key = RenderImageParams {
                image_id: image.id,
                frame_index: 0,
            }
            .into();
            let generation = atlas
                .get_texture_info(foreign.texture_id)
                .unwrap()
                .generation;
            atlas.remove(&key);
            assert!(atlas.get_texture_info(foreign.texture_id).is_none());
            let again = atlas.get_or_insert_gpu_image(&image)?.unwrap();
            assert_eq!(foreign.texture_id, again.texture_id);
            assert_ne!(foreign.tile_id, again.tile_id);
            assert_ne!(
                generation,
                atlas.get_texture_info(again.texture_id).unwrap().generation
            );
            atlas.remove(&key);
            atlas.remove(&cpu_key);
        }
        Ok(())
    }

    #[test]
    fn foreign_import_rejects_wrong_devices_and_invalid_metadata() -> gpui::Result<()> {
        use crate::{WgpuAtlas, WgpuImage};
        use gpui::{PlatformAtlas, RenderImage};
        let renderer = WgpuHeadlessRenderer::new()?;
        let context = renderer.shared_context();
        let texture = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("import validation"),
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let payload = Arc::new(WgpuImage {
            device: context.device.clone(),
            texture,
        });
        let wrong_size = RenderImage::from_gpu(device_size(3, 4), payload.clone());
        let image = RenderImage::from_gpu(device_size(4, 4), payload);
        let atlas = renderer.sprite_atlas();
        assert!(atlas.get_or_insert_gpu_image(&wrong_size).is_err());
        assert!(
            atlas
                .get_or_insert_gpu_image(&RenderImage::from_gpu(device_size(4, 4), Arc::new(0u8)))
                .is_err()
        );
        let (other_device, other_queue) = gpui::block_on(async {
            wgpu::Instance::default()
                .request_adapter(&Default::default())
                .await?
                .request_device(&Default::default())
                .await
                .map_err(anyhow::Error::from)
        })?;
        let other_atlas = WgpuAtlas::new(
            Arc::new(other_device),
            Arc::new(other_queue),
            wgpu::TextureFormat::Rgba8Unorm,
        );
        assert!(
            other_atlas
                .get_or_insert_gpu_image(&image)
                .unwrap_err()
                .to_string()
                .contains("different device")
        );
        // A rejected import must leave the original atlas usable.
        assert!(atlas.get_or_insert_gpu_image(&image)?.is_some());
        Ok(())
    }
}
