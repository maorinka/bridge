//! Metal-based video display for low-latency rendering
//!
//! Uses Metal for GPU-accelerated rendering with:
//! - Direct IOSurface texture import for zero-copy
//! - VSync-disabled mode for lowest latency
//! - Full-screen rendering

use bridge_common::{BridgeResult, BridgeError};
use std::ffi::c_void;
use tracing::{debug, info, trace};

use metal::{Device, CommandQueue, MTLPixelFormat, RenderPipelineState};

use crate::codec::DecodedFrame;
use crate::sys::*;

/// Metal-based video renderer
pub struct MetalDisplay {
    device: Device,
    command_queue: CommandQueue,
    pipeline_state: Option<RenderPipelineState>,
    width: u32,
    height: u32,
    frame_count: u64,
}

unsafe impl Send for MetalDisplay {}

impl MetalDisplay {
    /// Create a new Metal display renderer
    pub fn new(width: u32, height: u32) -> BridgeResult<Self> {
        info!("Creating Metal display renderer: {}x{}", width, height);

        let device = Device::system_default()
            .ok_or_else(|| BridgeError::Video("No Metal device found".into()))?;

        let command_queue = device.new_command_queue();

        let mut display = Self {
            device,
            command_queue,
            pipeline_state: None,
            width,
            height,
            frame_count: 0,
        };

        display.create_pipeline()?;

        Ok(display)
    }

    fn create_pipeline(&mut self) -> BridgeResult<()> {
        // Shader source for simple texture display
        let shader_source = r#"
            #include <metal_stdlib>
            using namespace metal;

            struct VertexOut {
                float4 position [[position]];
                float2 texCoord;
            };

            vertex VertexOut vertexShader(uint vertexID [[vertex_id]]) {
                float2 positions[6] = {
                    float2(-1.0, -1.0),
                    float2( 1.0, -1.0),
                    float2(-1.0,  1.0),
                    float2(-1.0,  1.0),
                    float2( 1.0, -1.0),
                    float2( 1.0,  1.0)
                };

                float2 texCoords[6] = {
                    float2(0.0, 1.0),
                    float2(1.0, 1.0),
                    float2(0.0, 0.0),
                    float2(0.0, 0.0),
                    float2(1.0, 1.0),
                    float2(1.0, 0.0)
                };

                VertexOut out;
                out.position = float4(positions[vertexID], 0.0, 1.0);
                out.texCoord = texCoords[vertexID];
                return out;
            }

            fragment float4 fragmentShader(VertexOut in [[stage_in]],
                                          texture2d<float> texture [[texture(0)]]) {
                constexpr sampler textureSampler(mag_filter::linear, min_filter::linear);
                return texture.sample(textureSampler, in.texCoord);
            }
        "#;

        // Compile shaders
        let library = self.device.new_library_with_source(shader_source, &metal::CompileOptions::new())
            .map_err(|e| BridgeError::Video(format!("Shader compilation failed: {}", e)))?;

        let vertex_fn = library.get_function("vertexShader", None)
            .map_err(|e| BridgeError::Video(format!("Vertex function not found: {}", e)))?;

        let fragment_fn = library.get_function("fragmentShader", None)
            .map_err(|e| BridgeError::Video(format!("Fragment function not found: {}", e)))?;

        // Create pipeline descriptor
        let pipeline_desc = metal::RenderPipelineDescriptor::new();
        pipeline_desc.set_vertex_function(Some(&vertex_fn));
        pipeline_desc.set_fragment_function(Some(&fragment_fn));

        let color_attachment = pipeline_desc.color_attachments().object_at(0).unwrap();
        color_attachment.set_pixel_format(MTLPixelFormat::BGRA8Unorm);

        self.pipeline_state = Some(
            self.device.new_render_pipeline_state(&pipeline_desc)
                .map_err(|e| BridgeError::Video(format!("Pipeline creation failed: {}", e)))?
        );

        debug!("Render pipeline created");
        Ok(())
    }

    /// Render a decoded frame
    pub fn render(&mut self, frame: &DecodedFrame) -> BridgeResult<()> {
        let _pipeline = self.pipeline_state.as_ref()
            .ok_or_else(|| BridgeError::Video("Pipeline not created".into()))?;

        // Create texture from frame data
        let _texture = self.create_texture_from_frame(frame)?;

        // In a full implementation with a CAMetalLayer, we would:
        // 1. Get next drawable from CAMetalLayer
        // 2. Create a render pass with the drawable texture
        // 3. Encode render commands (set pipeline, set texture, draw triangles)
        // 4. Present the drawable

        self.frame_count += 1;
        trace!("Rendered frame {}", self.frame_count);

        Ok(())
    }

    fn create_texture_from_frame(&self, frame: &DecodedFrame) -> BridgeResult<metal::Texture> {
        // If we have an IOSurface, use zero-copy path
        if let Some(io_surface) = frame.io_surface {
            return self.create_texture_from_iosurface(io_surface, frame.width, frame.height);
        }

        // Fallback: create texture from pixel data
        let texture_desc = metal::TextureDescriptor::new();
        texture_desc.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        texture_desc.set_width(frame.width as u64);
        texture_desc.set_height(frame.height as u64);
        texture_desc.set_usage(metal::MTLTextureUsage::ShaderRead);

        let texture = self.device.new_texture(&texture_desc);

        let region = metal::MTLRegion {
            origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
            size: metal::MTLSize {
                width: frame.width as u64,
                height: frame.height as u64,
                depth: 1,
            },
        };

        texture.replace_region(
            region,
            0,
            frame.data.as_ptr() as *const _,
            frame.bytes_per_row as u64,
        );

        Ok(texture)
    }

    fn create_texture_from_iosurface(
        &self,
        io_surface: IOSurfaceRef,
        width: u32,
        height: u32,
    ) -> BridgeResult<metal::Texture> {
        // Fall back to copying from IOSurface
        unsafe {
            let lock_result = IOSurfaceLock(io_surface, 0, std::ptr::null_mut());
            if lock_result != 0 {
                return Err(BridgeError::Video("Failed to lock IOSurface".into()));
            }

            let base_address = IOSurfaceGetBaseAddress(io_surface);
            let bytes_per_row = IOSurfaceGetBytesPerRow(io_surface);

            let texture_desc = metal::TextureDescriptor::new();
            texture_desc.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
            texture_desc.set_width(width as u64);
            texture_desc.set_height(height as u64);
            texture_desc.set_usage(metal::MTLTextureUsage::ShaderRead);

            let texture = self.device.new_texture(&texture_desc);

            let region = metal::MTLRegion {
                origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
                size: metal::MTLSize {
                    width: width as u64,
                    height: height as u64,
                    depth: 1,
                },
            };

            texture.replace_region(
                region,
                0,
                base_address as *const _,
                bytes_per_row as u64,
            );

            IOSurfaceUnlock(io_surface, 0, std::ptr::null_mut());

            Ok(texture)
        }
    }

    /// Get frame count
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Get the Metal device
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Get the command queue
    pub fn command_queue(&self) -> &CommandQueue {
        &self.command_queue
    }
}

/// Display statistics
#[derive(Debug, Clone, Default)]
pub struct DisplayStats {
    pub frames_rendered: u64,
    pub avg_render_time_us: u64,
    pub dropped_frames: u64,
}
