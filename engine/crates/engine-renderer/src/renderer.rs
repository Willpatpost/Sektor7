//! Safe public renderer API (Stage 2). All Vulkan unsafe lives in
//! [`crate::vulkan`]; this module owns the four Vulkan states, exposes the
//! safe surface, and collects instrumentation.

use ash::vk;

use sdl3::video::Window as SdlWindow;

use crate::config::{RenderConfig, present_mode_str};
use crate::overlay::{OverlayVertex, build_overlay_vertices_into};
use crate::scene::SceneVertex;
use crate::vulkan::{
    self, CapturedFrame, FrameState, PipelineState, RendererError, SwapchainState, VulkanContext,
    drain_validation_messages, pick_depth_format, pick_surface_format,
};

/// A batch of triangles with its own transform into clip space. A
/// foreground batch typically supplies camera-local vertices and a projection.
#[derive(Debug, Clone, Copy)]
pub struct SceneLayer<'a> {
    pub vertices: &'a [SceneVertex],
    pub view_proj: &'a [[f32; 4]; 4],
}

/// Actual dynamic uploads and draws recorded in this frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GeometryStats {
    /// World plus foreground vertices.
    pub scene_drawn: u32,
    /// Portion of `scene_drawn` used by the foreground.
    pub foreground_drawn: u32,
    /// HUD plus diagnostic vertices.
    pub overlay_drawn: u32,
    pub scene_omitted: u32,
    pub overlay_omitted: u32,
    pub dynamic_draw_calls: u32,
    pub uploaded_bytes: u64,
}

/// Per-frame result surfaced to the application.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FrameResult {
    pub presented: bool,
    pub recreated: bool,
    pub suboptimal: bool,
    /// The drawable was temporarily unavailable (zero-sized surface
    /// capabilities — minimized or mid-transition): nothing happened;
    /// poll and try again next frame.
    pub deferred: bool,
    pub gpu_render_ms: Option<f64>,
    pub geometry: GeometryStats,
}

/// Long-lived renderer statistics (Stage 2 authorization, item 4).
///
/// These are instrumentation readings of the simple Stage 2 scene — they
/// are not claims about the first game's performance (TECH-ARCHITECTURE
/// §6, stage-table policy).
#[derive(Debug, Clone)]
pub struct RenderStats {
    pub gpu_name: String,
    pub device_type: String,
    pub api_version: String,
    pub present_mode: String,
    pub supported_present_modes: Vec<String>,
    pub surface_format: String,
    pub extent: (u32, u32),
    pub swapchain_images: u32,
    pub frames_presented: u64,
    pub swapchain_recreations: u64,
    pub validation_active: bool,
    pub validation_errors: u64,
    pub validation_warnings: u64,
    /// Last validation messages (bounded), most recent last.
    pub recent_validation: Vec<String>,
    /// F-03 Step 1 (fail-visible over fail-silent): cumulative
    /// dynamic-geometry vertices omitted because a frame's supply
    /// exceeded the frame slot's buffer capacity, with the peak
    /// supplied and the per-slot capacity for context. Zero in every
    /// ordinary run (the H3 proof); nonzero is REPORTED in the run
    /// summary, never fatal — the capacity/refusal policy is Step 2,
    /// separately authorized. The region capacity reads 0 until the
    /// lazy region buffer allocates (no region frames).
    pub region_truncated_vertices: u64,
    pub region_supplied_peak_vertices: u32,
    pub region_capacity_vertices: u32,
    pub overlay_truncated_vertices: u64,
    pub overlay_supplied_peak_vertices: u32,
    pub overlay_capacity_vertices: u32,
    gpu_history_ms: Vec<f64>,
}

impl RenderStats {
    fn percentile(&self, quantile: f64) -> f64 {
        if self.gpu_history_ms.is_empty() {
            return 0.0;
        }
        let mut sorted = self.gpu_history_ms.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let index = ((quantile * sorted.len() as f64).ceil() as usize)
            .saturating_sub(1)
            .min(sorted.len() - 1);
        sorted[index]
    }

    /// GPU render time (timestamp-derived), millisecond percentiles.
    pub fn gpu_ms_p50(&self) -> f64 {
        self.percentile(0.50)
    }

    pub fn gpu_ms_p95(&self) -> f64 {
        self.percentile(0.95)
    }

    pub fn gpu_ms_p99(&self) -> f64 {
        self.percentile(0.99)
    }

    pub fn gpu_sample_count(&self) -> usize {
        self.gpu_history_ms.len()
    }
}

/// The Stage 2 Vulkan renderer. Rendering cadence is caller-driven — the
/// renderer imposes no frame-rate ceiling, and rendering is deliberately
/// independent of the future fixed-rate campaign simulation (Stage 3,
/// gated). Drop performs the ordered teardown documented in
/// [`crate::vulkan`].
pub struct Renderer {
    ctx: VulkanContext,
    swapchain: SwapchainState,
    pipes: PipelineState,
    frames: FrameState,
    config: RenderConfig,
    stats: RenderStats,
    overlay_enabled: bool,
    overlay_color: [f32; 4],
    /// The F-05 retained overlay-vertex buffer: rebuilt in place every
    /// frame (the capacity from the first frames is reused — no
    /// per-frame allocation or growth; the vertex content is
    /// identical to the allocating form).
    overlay_vertices: Vec<crate::overlay::OverlayVertex>,
}

impl Renderer {
    pub fn new(window: &SdlWindow, config: RenderConfig) -> Result<Renderer, RendererError> {
        let (mut ctx, validation_active) = VulkanContext::new(window, &config)?;
        // Cross-stage cleanup (corrective finding G): each constructor
        // cleans up its own partials; this ladder releases completed
        // stages when a later stage fails.
        let (format, color_space) = match pick_surface_format(&ctx) {
            Ok(picked) => picked,
            Err(error) => {
                ctx.destroy();
                return Err(error);
            }
        };
        // The depth format is chosen once and shared by the render pass
        // and every depth attachment (corrective finding F).
        let depth_format = pick_depth_format(&ctx.instance, ctx.physical_device);
        // The demo-scene preference is fixed at construction (P3): the
        // static Stage-2 demonstration scene uploads only on the
        // explicit opt-in, and every later pipeline replacement is
        // built with this same live preference.
        let mut pipes = match PipelineState::new(&ctx, format, depth_format, config.demo_scene) {
            Ok(pipes) => pipes,
            Err(error) => {
                ctx.destroy();
                return Err(error);
            }
        };
        let (width, height) = window.size();
        let mut swapchain = match SwapchainState::new(
            &ctx,
            (width, height),
            config.present_mode,
            format,
            color_space,
            pipes.render_pass,
            depth_format,
            vk::SwapchainKHR::null(),
        ) {
            Ok(swapchain) => swapchain,
            Err(error) => {
                pipes.destroy(&ctx);
                ctx.destroy();
                return Err(error);
            }
        };
        let frames = match FrameState::new(&ctx, swapchain.image_count) {
            Ok(frames) => frames,
            Err(error) => {
                swapchain.destroy(&ctx);
                pipes.destroy(&ctx);
                ctx.destroy();
                return Err(error);
            }
        };
        let stats = RenderStats {
            gpu_name: ctx.gpu_name.clone(),
            device_type: ctx.device_type.clone(),
            api_version: ctx.api_version.clone(),
            present_mode: present_mode_str(swapchain.present_mode).to_string(),
            supported_present_modes: ctx
                .supported_present_modes
                .iter()
                .map(|mode| present_mode_str(*mode).to_string())
                .collect(),
            surface_format: format!("{:?}", swapchain.format),
            extent: (swapchain.extent.width, swapchain.extent.height),
            swapchain_images: swapchain.image_count,
            frames_presented: 0,
            swapchain_recreations: 0,
            validation_active,
            validation_errors: 0,
            validation_warnings: 0,
            recent_validation: Vec::new(),
            region_truncated_vertices: 0,
            region_supplied_peak_vertices: 0,
            region_capacity_vertices: 0,
            overlay_truncated_vertices: 0,
            overlay_supplied_peak_vertices: 0,
            overlay_capacity_vertices: 0,
            gpu_history_ms: Vec::new(),
        };
        Ok(Renderer {
            ctx,
            swapchain,
            pipes,
            frames,
            config,
            stats,
            overlay_enabled: true,
            overlay_color: [0.72, 1.0, 0.78, 0.85],
            overlay_vertices: Vec::new(),
        })
    }

    /// Renders one frame. `window_size` is the current window size in
    /// pixels — the application owns window state (the Stage 1 shell), and
    /// the renderer never queries SDL after construction. A minimized
    /// window (zero extent) presents nothing. `region_scene` is dynamic
    /// per-frame geometry (Stage 4 graphical integration: region area
    /// plates + NPC render proxies) drawn through the scene pipeline from
    /// a per-frame-slot persistently mapped buffer — the overlay's
    /// dynamic-update pattern; an empty slice draws nothing (the Stage
    /// 2/3 default path). `overlay_text` renders the debug overlay when
    /// overlays are enabled.
    pub fn render_frame(
        &mut self,
        view_proj: &[[f32; 4]; 4],
        region_scene: &[SceneVertex],
        overlay_text: Option<&str>,
        window_size: (u32, u32),
    ) -> Result<FrameResult, RendererError> {
        self.render_layers(
            view_proj,
            region_scene,
            None,
            &[],
            overlay_text,
            window_size,
        )
    }

    /// Draws the world, an optional foreground with independent depth,
    /// and one screen-space batch. World and foreground share the same
    /// scene slot, in that order. HUD and diagnostics share the overlay
    /// slot in that order. All supplied batches are triangle lists (three
    /// vertices per primitive). The diagnostics toggle controls only the text.
    pub fn render_layers(
        &mut self,
        view_proj: &[[f32; 4]; 4],
        world: &[SceneVertex],
        foreground: Option<SceneLayer<'_>>,
        hud: &[OverlayVertex],
        debug_text: Option<&str>,
        window_size: (u32, u32),
    ) -> Result<FrameResult, RendererError> {
        // The F-05 overlay-vertex reuse (the audit's measured
        // candidate): the glyph vertices are rebuilt into the
        // renderer's own retained buffer — the capacity from the
        // first frames is reused every frame (no per-frame
        // allocation or growth; the vertex content is identical).
        if self.overlay_enabled {
            if let Some(text) = debug_text {
                build_overlay_vertices_into(
                    &mut self.overlay_vertices,
                    text,
                    window_size,
                    self.overlay_color,
                );
            } else {
                self.overlay_vertices.clear();
            }
        } else {
            self.overlay_vertices.clear();
        }
        let overlay = &self.overlay_vertices;
        let outcome = vulkan::render_frame(
            &mut self.ctx,
            &mut self.swapchain,
            &mut self.pipes,
            &mut self.frames,
            window_size,
            self.config.present_mode,
            self.config.replace_pipelines,
            self.config.demo_scene,
            view_proj,
            world,
            foreground,
            hud,
            overlay,
        )?;
        // F-03 Step 1: accumulate the frame's capacity decisions —
        // cumulative omitted vertices, peak supplied, and the per-slot
        // capacities (region stays 0 until its lazy buffer allocates).
        // Deferred/minimized frames carry all-zero decisions, so they
        // contribute nothing. Nonzero truncation is REPORTED in the run
        // summary, never fatal.
        self.stats.region_truncated_vertices += u64::from(outcome.region.truncated);
        self.stats.region_supplied_peak_vertices = self
            .stats
            .region_supplied_peak_vertices
            .max(outcome.region.supplied);
        self.stats.region_capacity_vertices = self
            .stats
            .region_capacity_vertices
            .max(outcome.region.capacity);
        self.stats.overlay_truncated_vertices += u64::from(outcome.overlay.truncated);
        self.stats.overlay_supplied_peak_vertices = self
            .stats
            .overlay_supplied_peak_vertices
            .max(outcome.overlay.supplied);
        self.stats.overlay_capacity_vertices = self
            .stats
            .overlay_capacity_vertices
            .max(outcome.overlay.capacity);
        if outcome.presented {
            self.stats.frames_presented += 1;
        }
        if outcome.recreated {
            self.stats.swapchain_recreations += 1;
            self.stats.extent = (self.swapchain.extent.width, self.swapchain.extent.height);
            self.stats.swapchain_images = self.swapchain.image_count;
        }
        if let Some(milliseconds) = outcome.gpu_render_ms {
            self.stats.gpu_history_ms.push(milliseconds);
            let len = self.stats.gpu_history_ms.len();
            if len > 600 {
                self.stats.gpu_history_ms.drain(0..len - 600);
            }
        }
        for (class, message) in drain_validation_messages() {
            match class {
                2 => {
                    self.stats.validation_errors += 1;
                    self.push_validation_message(message);
                }
                1 => {
                    self.stats.validation_warnings += 1;
                    self.push_validation_message(message);
                }
                _ => {}
            }
        }
        Ok(FrameResult {
            presented: outcome.presented,
            recreated: outcome.recreated,
            suboptimal: outcome.suboptimal,
            deferred: outcome.deferred,
            gpu_render_ms: outcome.gpu_render_ms,
            geometry: GeometryStats {
                scene_drawn: outcome.region.drawn,
                foreground_drawn: outcome.foreground_drawn,
                overlay_drawn: outcome.overlay.drawn,
                scene_omitted: outcome.region.truncated,
                overlay_omitted: outcome.overlay.truncated,
                dynamic_draw_calls: u32::from(outcome.region.drawn > outcome.foreground_drawn)
                    + u32::from(outcome.foreground_drawn > 0)
                    + u32::from(outcome.overlay.drawn > 0),
                uploaded_bytes: u64::from(outcome.region.drawn) * size_of::<SceneVertex>() as u64
                    + u64::from(outcome.overlay.drawn) * size_of::<OverlayVertex>() as u64,
            },
        })
    }

    pub fn stats(&self) -> &RenderStats {
        &self.stats
    }

    pub fn set_overlay_enabled(&mut self, enabled: bool) {
        self.overlay_enabled = enabled;
    }

    pub fn overlay_enabled(&self) -> bool {
        self.overlay_enabled
    }

    /// Requests that the NEXT rendered frame be copied back to the
    /// host (the bounded frame-capture readback). ONE capture
    /// outstanding at a time: refused with a typed error while an
    /// earlier request is pending or ready-but-unconsumed (the
    /// single-flight lifecycle policy — a second copy could otherwise
    /// overwrite the earlier readback's destination). A capture
    /// REFUSAL never poisons frame synchronization: the frame renders
    /// normally and a later request can try again.
    pub fn request_frame_capture(&mut self) -> Result<(), RendererError> {
        vulkan::request_capture(&mut self.frames)
    }

    /// Takes the completed frame capture, if one is ready: waits the
    /// recording frame's submission fence and returns the presented
    /// image's texels (top row first, the swapchain's channel order).
    /// `Ok(None)` when no capture completed. Read promptly — a later
    /// capture overwrites the shared destination buffer.
    pub fn take_frame_capture(&mut self) -> Result<Option<CapturedFrame>, RendererError> {
        vulkan::take_capture(&self.ctx, &mut self.frames)
    }

    /// The most recent capture refusal or drop, if any (`None` in the
    /// ordinary path) — observable for diagnosis. A refused or
    /// dropped capture is never a frame error: the frame rendered
    /// normally regardless.
    pub fn last_capture_refusal(&self) -> Option<&RendererError> {
        vulkan::last_capture_refusal(&self.frames)
    }

    fn push_validation_message(&mut self, message: String) {
        self.stats.recent_validation.push(message);
        let len = self.stats.recent_validation.len();
        if len > 16 {
            self.stats.recent_validation.drain(0..len - 16);
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        vulkan::device_idle(&self.ctx).ok();
        self.frames.destroy(&self.ctx);
        // Framebuffers before the render pass they reference.
        self.swapchain.destroy(&self.ctx);
        self.pipes.destroy(&self.ctx);
        self.ctx.destroy();
    }
}
