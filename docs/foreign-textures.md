# Foreign GPU images

This topic branch imports the foreign-texture bridge from `lnicola/t-gis`
commit `51148960832428eb43af6f03130b4d8dac6c775a` onto GPUI Fast main.
The implementation is organized under `src/fast/foreign_textures.rs` in core,
wgpu and Linux; the viewer integration stays in t-gis. The original unused-binding
fix is omitted because main has already changed the affected retained-view code.

## Producing an image

1. Call `Window::gpu_context()` and downcast its opaque payload to
   `gpui_wgpu::SharedGpuContext` on Linux Wayland or X11.
2. Create and render a texture using that exact `Arc<wgpu::Device>` and its
   shared queue. Import accepts a sampleable, single-level, single-sample 2D
   `Rgba8Unorm` or `Bgra8Unorm` texture with one array layer and straight alpha.
3. Submit producer writes on the shared queue before publishing the image.
4. Wrap `gpui_wgpu::WgpuImage { device, texture }` in an `Arc`, pass it to
   `RenderImage::from_gpu(size, payload)`, and paint it as a normal image.

The imported texture is referenced directly as a standalone atlas texture:
there is no CPU backing, readback, upload or atlas-packing copy. Ordinary image
painting supplies clipping, opacity, placement and rounded corners. Imports
validate payload type, the supplied device Arc, texture properties and dimensions.
The supplied device must really own the texture; wgpu performs its own resource
validation as well. Different-device and external-memory imports are unsupported.

## Lifetime and recovery

Textures must remain immutable while any scene can display them. For new pixels,
create a new image rather than overwriting a previously published texture.
An image has one frame, its size is available without pixels, and `as_bytes`
returns `None`. CPU and GPU images share the same image-id allocator.

The atlas owns a separate texture reference. Once an image is no longer cached or
displayed, use the normal `App::drop_image` / `Window::drop_image` retirement path;
dropping only the `RenderImage` Arc does not evict its atlas entry. t-gis handles
this with an App-owned task so retained views do not postpone eviction.

Check the context before scheduling or painting. On device recovery, its device
Arc changes; invalidate cached images and outstanding jobs from the old device.
A lost or released renderer exposes no shared context, and importing an image
with an old device identity fails. Atlas generation changes prevent stale
bind groups from being reused when a texture slot is recycled. Imported tile ids
also carry the generation so scroll-layer hashes detect a recycled slot.

Native devices request standard WebGPU limits when available, falling back to
GPUI's downlevel limits otherwise. A compute renderer may still reject the
fallback limits. There is no configurable features/limits negotiation API.

## Verification

The core tests cover image metadata, shared ids and headless retirement. The wgpu
pixel tests compare GPU imports against CPU uploads for RGBA/BGRA, transparency,
opacity, clipping and resizing; they check atlas reuse/generation retirement and
reject invalid payloads, dimensions and a different device. Readback is confined
to the test renderer. Wayland and X11 expose the same context through platform
hooks. Other backends keep the default unsupported-image/context behavior.

## Downstream dependencies

This branch uses published wgpu 30.0.1 so Vello 0.11 can share its device and
texture types. It applies the same API adaptation as GPUI Fast’s release tooling.
It consequently lacks the unreleased Vulkan surface-drop and WebGL resize fixes
noted in `release.toml`. The `gpui-pre-*` compatibility manifests match GPUI Kit
0.9.0’s 0.3.11 snapshot requirements.
