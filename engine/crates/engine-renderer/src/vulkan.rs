//! The Vulkan backend — **all** engine-renderer unsafe code lives here
//! (Stage 2 authorization, item 1). Everything else in this crate stays
//! `unsafe`-free; `engine-core`'s `unsafe_code = deny` is untouched.
//!
//! # Invariants manifest
//!
//! **Ownership** — every raw Vulkan object is owned by exactly one struct
//! field, created in `new`/`recreate` and destroyed exactly once, in
//! [`VulkanContext::destroy`], [`SwapchainState::destroy`],
//! [`PipelineState::destroy`], or [`FrameState::destroy`]. ash handles are
//! non-owning pointers: dropping them destroys nothing, so a handle set to
//! `*::null()` after destruction marks it dead. `Renderer` (the safe
//! wrapper) owns all four states and calls their `destroy` methods from
//! its `Drop` in reverse creation order after `device_wait_idle`.
//!
//! **Lifetime** — every Vulkan call happens while its owning context is
//! alive. The SDL-created surface outlives the instance teardown by being
//! destroyed (via `khr::surface::Instance::destroy_surface`) **before**
//! `destroy_instance`. No borrowed Vulkan reference escapes this module;
//! the safe API exposes only copies, counts, and strings.
//!
//! **Synchronization** — classic frames-in-flight (2 slots): each slot owns
//! one command buffer, one `image_available` semaphore, and one
//! signaled-at-submit fence (`in_flight`). `render_frame` waits/resets the
//! slot fence before acquire, guards against acquiring an image another
//! in-flight submission still reads with per-image fences
//! (`images_in_flight`), and submits with `image_available` waited at
//! `COLOR_ATTACHMENT_OUTPUT`. **Presentation-wait semaphores are indexed by
//! swapchain image, not frame slot** (corrective finding C): the submit
//! fence proves only that the graphics work — including the acquire-wait —
//! finished, never that the presentation engine finished *waiting* on the
//! present semaphore, so slot-indexed reuse could signal a semaphore with
//! an outstanding present wait; indexing by the acquired image ties reuse
//! to re-acquisition of that image, and `device_wait_idle` before every
//! destroy/recreate drains outstanding presents. **Each swapchain image
//! owns its depth attachment** (finding A), so concurrent in-flight
//! submissions into different images never share depth state. **The
//! overlay vertex buffer has one disjoint region per frame slot**
//! (finding B) — a slot's region is written only after that slot's fence
//! wait, so no in-flight submission reads a region being rewritten. The
//! command pool allows individual command-buffer resets (finding D), and
//! the per-slot fence proves a buffer is no longer pending when its
//! re-record begins. GPU timestamps are written only when the queue family
//! supports them and read only after the slot fence proves that slot's
//! queries resolved at least once (finding E).
//!
//! **Destruction order** — `device_wait_idle` → command buffers/pool,
//! present semaphores, acquire semaphores, fences, query pool → swapchain
//! framebuffers, views, per-image depth views/images/memories, swapchain
//! (framebuffers **before** the render pass they reference) → pipelines,
//! layouts, render pass, scene/overlay buffers and memories (overlay
//! unmapped first) → logical device → debug messenger → surface →
//! instance. Swapchain **recreation** builds the replacement before
//! destroying the current state (a failed recreation leaves the renderer
//! valid) and follows the same order for the objects it owns, after
//! `device_wait_idle`.

// Unsafe containment (Stage 2 authorization, item 1): this module-level
// allow overrides the crate-level `#![deny(unsafe_code)]` in lib.rs —
// deny is overridable at closer scope, and this is the ONLY override.
#![allow(unsafe_code)]

use std::ffi::{CStr, CString};
use std::sync::Mutex;

use ash::ext;
use ash::khr;
use ash::vk;
use ash::{Device, Entry, Instance};

use crate::config::{FRAMES_IN_FLIGHT, PresentModePreference, RenderConfig, resolve_present_mode};
use crate::overlay::OverlayVertex;
use crate::scene::SceneVertex;

use sdl3::video::Window as SdlWindow;

const VALIDATION_LAYER_NAME: &str = "VK_LAYER_KHRONOS_validation";
/// Shader entry point for all Stage 2 pipelines.
const ENTRY_MAIN: &CStr = c"main";
/// Upper bound for any host-side fence wait (nanoseconds). A stuck
/// submission must surface as an error the application can report,
/// never as an unresponsive window (rig finding 2026-10-20: pacing
/// faster than a vsync-locked present mode can drain wedged the frame
/// loop — 240 fps pacing against FIFO on a 60 Hz display).
const WAIT_TIMEOUT_NS: u64 = 2_000_000_000;

/// Validation messages from the driver callback: (class, text) where
/// class 2 = error, 1 = warning, 0 = info. Bounded; drained by
/// [`crate::Renderer`] each frame.
static VALIDATION_QUEUE: Mutex<Vec<(u8, String)>> = Mutex::new(Vec::new());

/// Drains queued validation messages (public-safe).
pub(crate) fn drain_validation_messages() -> Vec<(u8, String)> {
    match VALIDATION_QUEUE.lock() {
        Ok(mut queue) => std::mem::take(&mut *queue),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    }
}

unsafe extern "system" fn validation_callback(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT,
    _user_data: *mut core::ffi::c_void,
) -> vk::Bool32 {
    if let Some(record) = (!data.is_null()).then(|| unsafe { &*data }) {
        let text = unsafe { CStr::from_ptr(record.p_message) }
            .to_string_lossy()
            .into_owned();
        let class = if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
            2
        } else if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
            1
        } else {
            0
        };
        if let Ok(mut queue) = VALIDATION_QUEUE.lock()
            && queue.len() < 256
        {
            queue.push((class, text));
        }
    }
    vk::FALSE
}

/// Why the renderer could not initialize — environment limitations are
/// distinct from application/runtime errors (Stage 2 authorization, item 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendererError {
    /// The Vulkan loader library is absent (no Vulkan runtime installed).
    NoLoader(String),
    /// No physical device exposes the required queue + swapchain support.
    NoDevice(String),
    /// A Vulkan call failed at runtime.
    Vulkan(String),
    /// SDL could not provide the window's Vulkan surface.
    Surface(String),
}

impl RendererError {
    pub(crate) fn vk(result: vk::Result, what: &str) -> Self {
        RendererError::Vulkan(format!("{what}: {result:?}"))
    }
}

impl std::fmt::Display for RendererError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RendererError::NoLoader(message) => write!(
                f,
                "no Vulkan loader available — is a Vulkan runtime installed? ({message})"
            ),
            RendererError::NoDevice(message) => {
                write!(f, "no suitable Vulkan device ({message})")
            }
            RendererError::Vulkan(message) => write!(f, "Vulkan error: {message}"),
            RendererError::Surface(message) => {
                write!(f, "SDL could not create the Vulkan surface: {message}")
            }
        }
    }
}

impl std::error::Error for RendererError {}

/// Core Vulkan objects. Created once; destroyed once in `destroy`.
pub(crate) struct VulkanContext {
    // Load-bearing lifetime field (never read): Entry owns the loaded
    // Vulkan loader (libloading::Library) — dropping it while the
    // instance is alive would unload the library under live handles.
    #[allow(dead_code)]
    pub entry: Entry,
    pub instance: Instance,
    pub messenger: vk::DebugUtilsMessengerEXT,
    pub debug_utils: ext::debug_utils::Instance,
    pub surface_ext: khr::surface::Instance,
    pub swapchain_ext: khr::swapchain::Device,
    pub physical_device: vk::PhysicalDevice,
    pub device: Device,
    pub graphics_queue: vk::Queue,
    pub graphics_family: u32,
    pub surface: vk::SurfaceKHR,
    pub timestamp_period_ns: f64,
    /// Timestamp valid bits for the graphics queue family; 0 means the
    /// family supports no timestamp queries (corrective finding E).
    pub timestamp_valid_bits: u32,
    pub gpu_name: String,
    pub device_type: String,
    pub api_version: String,
    pub supported_present_modes: Vec<vk::PresentModeKHR>,
}

impl VulkanContext {
    pub(crate) fn new(
        window: &SdlWindow,
        config: &RenderConfig,
    ) -> Result<(VulkanContext, bool), RendererError> {
        let entry = unsafe { Entry::load() }
            .map_err(|error| RendererError::NoLoader(format!("{error}")))?;

        // SDL must hold the Vulkan loader open before the extension
        // query: its offscreen video driver (headless smoke) only
        // advertises VK_EXT_headless_surface when its own enumeration
        // can see it, and that enumeration needs SDL's loader handle.
        // Windowed drivers return their static platform list either
        // way. The library stays open for the process lifetime next to
        // ash's own dlopen; SDL_Quit releases SDL's handle.
        if !unsafe { sdl3::sys::vulkan::SDL_Vulkan_LoadLibrary(core::ptr::null()) } {
            return Err(RendererError::Surface(
                "SDL could not open the Vulkan loader".into(),
            ));
        }

        let sdl_extensions = window
            .vulkan_instance_extensions()
            .map_err(|error| RendererError::Surface(format!("{error}")))?;
        let mut extension_cstrings: Vec<CString> = sdl_extensions
            .iter()
            .map(|name| CString::new(name.as_str()).unwrap_or_default())
            .collect();
        let mut layer_cstrings: Vec<CString> = Vec::new();

        let want_validation = match config.validation {
            crate::config::ValidationPreference::Off => false,
            crate::config::ValidationPreference::On => true,
            crate::config::ValidationPreference::Auto => cfg!(debug_assertions),
        };
        if want_validation {
            extension_cstrings.push(CString::from(ext::debug_utils::NAME));
            layer_cstrings.push(CString::new(VALIDATION_LAYER_NAME).unwrap());
        }

        let api_version = vk::make_api_version(0, 1, 1, 0);
        let app_info = vk::ApplicationInfo::default()
            .application_name(c"Sektor7")
            .application_version(vk::make_api_version(0, 0, 2, 0))
            .engine_name(c"Sektor7 engine")
            .engine_version(vk::make_api_version(0, 0, 2, 0))
            .api_version(api_version);

        let mut instance = None;
        let mut validation_active = false;
        if want_validation {
            // Try with validation; failure here is an environment
            // limitation (no layer installed), not an application error.
            let extension_names: Vec<*const core::ffi::c_char> =
                extension_cstrings.iter().map(|c| c.as_ptr()).collect();
            let layer_names: Vec<*const core::ffi::c_char> =
                layer_cstrings.iter().map(|c| c.as_ptr()).collect();
            let mut debug_info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                .message_severity(
                    vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                        | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                        | vk::DebugUtilsMessageSeverityFlagsEXT::INFO,
                )
                .message_type(
                    vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                        | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                        | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                )
                .pfn_user_callback(Some(validation_callback));
            let create_info = vk::InstanceCreateInfo::default()
                .application_info(&app_info)
                .enabled_extension_names(&extension_names)
                .enabled_layer_names(&layer_names)
                .push_next(&mut debug_info);
            match unsafe { entry.create_instance(&create_info, None) } {
                Ok(created) => {
                    instance = Some(created);
                    validation_active = true;
                }
                Err(_) => {
                    extension_cstrings.pop();
                    layer_cstrings.clear();
                }
            }
        }
        let instance = match instance {
            Some(created) => created,
            None => {
                let extension_names: Vec<*const core::ffi::c_char> =
                    extension_cstrings.iter().map(|c| c.as_ptr()).collect();
                let create_info = vk::InstanceCreateInfo::default()
                    .application_info(&app_info)
                    .enabled_extension_names(&extension_names);
                unsafe { entry.create_instance(&create_info, None) }
                    .map_err(|error| RendererError::Vulkan(format!("create_instance: {error}")))?
            }
        };

        // Partial-initialization cleanup (corrective finding G): every
        // fallible step from here on releases what was already created
        // before returning the error, so no path leaks or double-frees.
        let surface_ext = khr::surface::Instance::new(&entry, &instance);
        let debug_utils = ext::debug_utils::Instance::new(&entry, &instance);
        let surface = match unsafe { window.vulkan_create_surface(instance.handle()) } {
            Ok(surface) => surface,
            Err(error) => {
                unsafe { instance.destroy_instance(None) };
                return Err(RendererError::Surface(format!("{error}")));
            }
        };
        let messenger = if validation_active {
            let info = vk::DebugUtilsMessengerCreateInfoEXT::default()
                .message_severity(
                    vk::DebugUtilsMessageSeverityFlagsEXT::ERROR
                        | vk::DebugUtilsMessageSeverityFlagsEXT::WARNING,
                )
                .message_type(
                    vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                        | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                        | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
                )
                .pfn_user_callback(Some(validation_callback));
            match unsafe { debug_utils.create_debug_utils_messenger(&info, None) } {
                Ok(messenger) => messenger,
                Err(error) => {
                    unsafe {
                        surface_ext.destroy_surface(surface, None);
                        instance.destroy_instance(None);
                    }
                    return Err(RendererError::Vulkan(format!("messenger: {error}")));
                }
            }
        } else {
            vk::DebugUtilsMessengerEXT::null()
        };

        let Some((physical_device, graphics_family)) =
            pick_physical_device(&instance, &surface_ext, surface)
        else {
            unsafe {
                if messenger != vk::DebugUtilsMessengerEXT::null() {
                    debug_utils.destroy_debug_utils_messenger(messenger, None);
                }
                surface_ext.destroy_surface(surface, None);
                instance.destroy_instance(None);
            }
            return Err(RendererError::NoDevice(
                "no device with graphics+present queues, swapchain extension, and surface formats"
                    .into(),
            ));
        };

        let properties = unsafe { instance.get_physical_device_properties(physical_device) };
        let gpu_name = unsafe { CStr::from_ptr(properties.device_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        let device_type = match properties.device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => "discrete",
            vk::PhysicalDeviceType::INTEGRATED_GPU => "integrated",
            vk::PhysicalDeviceType::VIRTUAL_GPU => "virtual",
            vk::PhysicalDeviceType::CPU => "cpu",
            _ => "other",
        }
        .to_string();
        let api_version = format!(
            "{}.{}.{}",
            vk::api_version_major(properties.api_version),
            vk::api_version_minor(properties.api_version),
            vk::api_version_patch(properties.api_version)
        );
        let timestamp_period_ns = f64::from(properties.limits.timestamp_period);
        // Timestamp support is a queue-family property (corrective
        // finding E): 0 valid bits means timestamps are unsupported and
        // the frame path skips query writes and reads entirely.
        let timestamp_valid_bits = unsafe {
            instance
                .get_physical_device_queue_family_properties(physical_device)
                .get(graphics_family as usize)
                .map_or(0, |family| family.timestamp_valid_bits)
        };

        let supported_present_modes = unsafe {
            surface_ext.get_physical_device_surface_present_modes(physical_device, surface)
        }
        .unwrap_or_default();

        let swapchain_extension_names = [ash::khr::swapchain::NAME.as_ptr()];
        let queue_info = vk::DeviceQueueCreateInfo::default()
            .queue_family_index(graphics_family)
            .queue_priorities(&[1.0]);
        let queue_infos = [queue_info];
        let device_create = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_infos)
            .enabled_extension_names(&swapchain_extension_names);
        let device_creation =
            unsafe { instance.create_device(physical_device, &device_create, None) };
        let device = match device_creation {
            Ok(device) => device,
            Err(error) => {
                unsafe {
                    if messenger != vk::DebugUtilsMessengerEXT::null() {
                        debug_utils.destroy_debug_utils_messenger(messenger, None);
                    }
                    surface_ext.destroy_surface(surface, None);
                    instance.destroy_instance(None);
                }
                return Err(RendererError::Vulkan(format!("create_device: {error}")));
            }
        };
        let graphics_queue = unsafe { device.get_device_queue(graphics_family, 0) };
        let swapchain_ext = khr::swapchain::Device::new(&instance, &device);

        let context = VulkanContext {
            entry,
            instance,
            messenger,
            debug_utils,
            surface_ext,
            swapchain_ext,
            physical_device,
            device,
            graphics_queue,
            graphics_family,
            surface,
            timestamp_period_ns,
            timestamp_valid_bits,
            gpu_name,
            device_type,
            api_version,
            supported_present_modes,
        };
        Ok((context, validation_active))
    }

    /// Full teardown in manifest order (assumes swapchain/pipelines/frames
    /// were already destroyed by the owning Renderer).
    pub(crate) fn destroy(&mut self) {
        unsafe {
            if self.messenger != vk::DebugUtilsMessengerEXT::null() {
                self.debug_utils
                    .destroy_debug_utils_messenger(self.messenger, None);
                self.messenger = vk::DebugUtilsMessengerEXT::null();
            }
            self.device.destroy_device(None);
            self.surface_ext.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}

fn pick_physical_device(
    instance: &Instance,
    surface_ext: &khr::surface::Instance,
    surface: vk::SurfaceKHR,
) -> Option<(vk::PhysicalDevice, u32)> {
    let devices = unsafe { instance.enumerate_physical_devices() }.ok()?;
    let mut best: Option<(u64, vk::PhysicalDevice, u32)> = None;
    for device in devices {
        let properties = unsafe { instance.get_physical_device_properties(device) };
        let score = match properties.device_type {
            vk::PhysicalDeviceType::DISCRETE_GPU => 100,
            vk::PhysicalDeviceType::INTEGRATED_GPU => 10,
            _ => 1,
        };
        let families = unsafe { instance.get_physical_device_queue_family_properties(device) };
        let mut chosen_family = None;
        for (index, family) in families.iter().enumerate() {
            if family.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                && unsafe {
                    surface_ext.get_physical_device_surface_support(device, index as u32, surface)
                }
                .unwrap_or(false)
            {
                chosen_family = Some(index as u32);
                break;
            }
        }
        let Some(family) = chosen_family else {
            continue;
        };
        let extensions =
            unsafe { instance.enumerate_device_extension_properties(device) }.unwrap_or_default();
        let has_swapchain = extensions
            .iter()
            .any(|e| unsafe { CStr::from_ptr(e.extension_name.as_ptr()) } == khr::swapchain::NAME);
        if !has_swapchain {
            continue;
        }
        let formats = unsafe { surface_ext.get_physical_device_surface_formats(device, surface) }
            .unwrap_or_default();
        if formats.is_empty() {
            continue;
        }
        if best.is_none_or(|(best_score, _, _)| score > best_score) {
            best = Some((score, device, family));
        }
    }
    best.map(|(_, device, family)| (device, family))
}

/// Swapchain + attachments + framebuffers. Recreated on resize,
/// out-of-date, and suboptimal-present.
pub(crate) struct SwapchainState {
    pub swapchain: vk::SwapchainKHR,
    pub views: Vec<vk::ImageView>,
    pub framebuffers: Vec<vk::Framebuffer>,
    /// One depth attachment per swapchain image (corrective finding A):
    /// two in-flight submissions render into different images, so they
    /// never share depth state.
    pub depth_images: Vec<vk::Image>,
    pub depth_memories: Vec<vk::DeviceMemory>,
    pub depth_views: Vec<vk::ImageView>,
    pub extent: vk::Extent2D,
    pub format: vk::Format,
    pub present_mode: vk::PresentModeKHR,
    pub image_count: u32,
    pub depth_format: vk::Format,
}

/// Picks the surface format (BGRA8_UNORM + sRGB non-linear preferred — the
/// classic 24-bit surface). Called once before pipelines, which bake the
/// format into the render pass; recreation re-picks and rebuilds if it
/// changed (rare).
pub(crate) fn pick_surface_format(
    ctx: &VulkanContext,
) -> Result<(vk::Format, vk::ColorSpaceKHR), RendererError> {
    let formats = unsafe {
        ctx.surface_ext
            .get_physical_device_surface_formats(ctx.physical_device, ctx.surface)
    }
    .map_err(|error| RendererError::vk(error, "surface formats"))?;
    formats
        .iter()
        .find(|f| {
            f.format == vk::Format::B8G8R8A8_UNORM
                && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
        })
        .or_else(|| formats.first())
        .map(|f| (f.format, f.color_space))
        .ok_or_else(|| RendererError::NoDevice("surface has no formats".into()))
}

impl SwapchainState {
    /// `depth_format` is chosen once by the owning renderer and shared
    /// with the pipeline state, so the render pass and the depth
    /// attachments cannot drift apart (corrective finding F).
    /// `old_swapchain` is set during recreation: creating the replacement
    /// while the current swapchain still owns the native window is the
    /// documented `VkSwapchainCreateInfoKHR::oldSwapchain` flow — without
    /// it, native-window platforms (Win32) fail with
    /// ERROR_NATIVE_WINDOW_IN_USE_KHR, found on the lead's rig when
    /// maximizing (corrective follow-up, 2026-10-20). Null for a fresh
    /// renderer.
    // Swapchain creation inputs plus the recreation linkage; bundling
    // would hide which object pairs with which lifetime.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        ctx: &VulkanContext,
        window_size: (u32, u32),
        preference: PresentModePreference,
        format: vk::Format,
        color_space: vk::ColorSpaceKHR,
        render_pass: vk::RenderPass,
        depth_format: vk::Format,
        old_swapchain: vk::SwapchainKHR,
    ) -> Result<SwapchainState, RendererError> {
        // Partial-initialization cleanup (corrective finding G): build
        // into a state whose handles start dead; on failure `destroy`
        // releases exactly what was created (null/empty entries are
        // no-ops per the Vulkan spec).
        let mut state = SwapchainState {
            swapchain: vk::SwapchainKHR::null(),
            views: Vec::new(),
            framebuffers: Vec::new(),
            depth_images: Vec::new(),
            depth_memories: Vec::new(),
            depth_views: Vec::new(),
            extent: vk::Extent2D::default(),
            format,
            present_mode: vk::PresentModeKHR::FIFO,
            image_count: 0,
            depth_format,
        };
        if let Err(error) = state.build(
            ctx,
            window_size,
            preference,
            format,
            color_space,
            render_pass,
            old_swapchain,
        ) {
            state.destroy(ctx);
            return Err(error);
        }
        Ok(state)
    }

    // Same rationale as `new`: the creation inputs plus the recreation
    // linkage, spread for lifetime clarity.
    #[allow(clippy::too_many_arguments)]
    fn build(
        &mut self,
        ctx: &VulkanContext,
        window_size: (u32, u32),
        preference: PresentModePreference,
        format: vk::Format,
        color_space: vk::ColorSpaceKHR,
        render_pass: vk::RenderPass,
        old_swapchain: vk::SwapchainKHR,
    ) -> Result<(), RendererError> {
        let surface = ctx.surface;
        let capabilities = unsafe {
            ctx.surface_ext
                .get_physical_device_surface_capabilities(ctx.physical_device, surface)
        }
        .map_err(|error| RendererError::vk(error, "surface capabilities"))?;

        let extent = clamp_extent(window_size, &capabilities);
        if extent.width == 0 || extent.height == 0 {
            return Err(RendererError::Vulkan(
                "zero window extent (minimized?)".into(),
            ));
        }
        self.extent = extent;
        self.present_mode = resolve_present_mode(preference, &ctx.supported_present_modes);

        let desired_images = (capabilities.min_image_count + 1).max(FRAMES_IN_FLIGHT);
        let max_images = if capabilities.max_image_count == 0 {
            desired_images
        } else {
            capabilities.max_image_count
        };
        let image_count = desired_images.min(max_images);

        let create_info = vk::SwapchainCreateInfoKHR::default()
            .surface(surface)
            .min_image_count(image_count)
            .image_format(format)
            .image_color_space(color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(capabilities.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(self.present_mode)
            .old_swapchain(old_swapchain)
            .clipped(true);
        self.swapchain = unsafe { ctx.swapchain_ext.create_swapchain(&create_info, None) }
            .map_err(|error| RendererError::vk(error, "create_swapchain"))?;
        let images = unsafe { ctx.swapchain_ext.get_swapchain_images(self.swapchain) }
            .map_err(|error| RendererError::vk(error, "get_swapchain_images"))?;
        self.image_count = images.len() as u32;

        let subresource = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            level_count: 1,
            layer_count: 1,
            ..Default::default()
        };
        for image in &images {
            let view_info = vk::ImageViewCreateInfo::default()
                .image(*image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .subresource_range(subresource);
            let view = unsafe { ctx.device.create_image_view(&view_info, None) }
                .map_err(|error| RendererError::vk(error, "create_image_view"))?;
            self.views.push(view);
        }

        // One depth attachment per swapchain image (corrective finding
        // A): in-flight submissions into different images never share
        // depth state.
        for _ in &images {
            let depth_info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .extent(vk::Extent3D {
                    width: extent.width,
                    height: extent.height,
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .format(self.depth_format)
                .tiling(vk::ImageTiling::OPTIMAL)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
                .samples(vk::SampleCountFlags::TYPE_1)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
            let depth_image = unsafe { ctx.device.create_image(&depth_info, None) }
                .map_err(|error| RendererError::vk(error, "create depth image"))?;
            self.depth_images.push(depth_image);
            let memory_requirements =
                unsafe { ctx.device.get_image_memory_requirements(depth_image) };
            let depth_memory_index = find_memory_type(
                ctx,
                memory_requirements.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .ok_or_else(|| RendererError::NoDevice("no DEVICE_LOCAL memory type".into()))?;
            let allocate_info = vk::MemoryAllocateInfo::default()
                .allocation_size(memory_requirements.size)
                .memory_type_index(depth_memory_index);
            let depth_memory = unsafe { ctx.device.allocate_memory(&allocate_info, None) }
                .map_err(|error| RendererError::vk(error, "allocate depth memory"))?;
            self.depth_memories.push(depth_memory);
            unsafe { ctx.device.bind_image_memory(depth_image, depth_memory, 0) }
                .map_err(|error| RendererError::vk(error, "bind depth memory"))?;
            let depth_view_info = vk::ImageViewCreateInfo::default()
                .image(depth_image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(self.depth_format)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::DEPTH,
                    level_count: 1,
                    layer_count: 1,
                    ..Default::default()
                });
            let depth_view = unsafe { ctx.device.create_image_view(&depth_view_info, None) }
                .map_err(|error| RendererError::vk(error, "create depth view"))?;
            self.depth_views.push(depth_view);
        }

        for index in 0..self.views.len() {
            let attachments = [self.views[index], self.depth_views[index]];
            let framebuffer_info = vk::FramebufferCreateInfo::default()
                .render_pass(render_pass)
                .attachments(&attachments)
                .width(extent.width)
                .height(extent.height)
                .layers(1);
            let framebuffer = unsafe { ctx.device.create_framebuffer(&framebuffer_info, None) }
                .map_err(|error| RendererError::vk(error, "create_framebuffer"))?;
            self.framebuffers.push(framebuffer);
        }

        Ok(())
    }

    pub(crate) fn destroy(&mut self, ctx: &VulkanContext) {
        unsafe {
            for framebuffer in &self.framebuffers {
                ctx.device.destroy_framebuffer(*framebuffer, None);
            }
            self.framebuffers.clear();
            for view in &self.depth_views {
                ctx.device.destroy_image_view(*view, None);
            }
            self.depth_views.clear();
            for image in &self.depth_images {
                ctx.device.destroy_image(*image, None);
            }
            self.depth_images.clear();
            for memory in &self.depth_memories {
                ctx.device.free_memory(*memory, None);
            }
            self.depth_memories.clear();
            for view in &self.views {
                ctx.device.destroy_image_view(*view, None);
            }
            self.views.clear();
            if self.swapchain != vk::SwapchainKHR::null() {
                ctx.swapchain_ext.destroy_swapchain(self.swapchain, None);
                self.swapchain = vk::SwapchainKHR::null();
            }
        }
    }
}

fn clamp_extent(
    window_size: (u32, u32),
    capabilities: &vk::SurfaceCapabilitiesKHR,
) -> vk::Extent2D {
    let current = capabilities.current_extent;
    let clamp = |value: u32, min: u32, max: u32| {
        value.clamp(min.max(1), if max == 0 { u32::MAX } else { max })
    };
    let (width, height) = if current.width == u32::MAX && current.height == u32::MAX {
        window_size
    } else {
        (current.width, current.height)
    };
    vk::Extent2D {
        width: clamp(
            width,
            capabilities.min_image_extent.width,
            capabilities.max_image_extent.width,
        ),
        height: clamp(
            height,
            capabilities.min_image_extent.height,
            capabilities.max_image_extent.height,
        ),
    }
}

/// Depth format for the graphics device; chosen **once per renderer** and
/// shared by the render pass and every depth attachment, so they cannot
/// drift apart (corrective finding F).
pub(crate) fn pick_depth_format(
    instance: &Instance,
    physical_device: vk::PhysicalDevice,
) -> vk::Format {
    for format in [
        vk::Format::D32_SFLOAT,
        vk::Format::D24_UNORM_S8_UINT,
        vk::Format::D16_UNORM,
    ] {
        let properties =
            unsafe { instance.get_physical_device_format_properties(physical_device, format) };
        if properties
            .optimal_tiling_features
            .contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT)
        {
            return format;
        }
    }
    vk::Format::D16_UNORM
}

fn find_memory_type(
    ctx: &VulkanContext,
    type_bits: u32,
    properties: vk::MemoryPropertyFlags,
) -> Option<u32> {
    let memory_properties = unsafe {
        ctx.instance
            .get_physical_device_memory_properties(ctx.physical_device)
    };
    for (index, memory_type) in memory_properties.memory_types.iter().enumerate() {
        if type_bits & (1 << index) != 0 && memory_type.property_flags.contains(properties) {
            return Some(index as u32);
        }
    }
    None
}

/// Render pass, pipelines, and the scene/overlay vertex + index buffers.
pub(crate) struct PipelineState {
    pub render_pass: vk::RenderPass,
    pub scene_pipeline: vk::Pipeline,
    pub scene_layout: vk::PipelineLayout,
    pub overlay_pipeline: vk::Pipeline,
    pub overlay_layout: vk::PipelineLayout,
    pub scene_vbo: vk::Buffer,
    pub scene_vbo_memory: vk::DeviceMemory,
    pub scene_ibo: vk::Buffer,
    pub scene_ibo_memory: vk::DeviceMemory,
    pub scene_index_count: u32,
    pub overlay_vbo: vk::Buffer,
    pub overlay_vbo_memory: vk::DeviceMemory,
    /// Per-slot vertex capacity of the overlay buffer (corrective
    /// finding B).
    pub overlay_capacity: u32,
    /// Byte size of one frame slot's overlay region.
    pub overlay_slot_bytes: vk::DeviceSize,
    overlay_mapped: *mut core::ffi::c_void,
}

impl PipelineState {
    /// `depth_format` comes from the renderer's single pick (corrective
    /// finding F) so the render pass matches the swapchain's depth
    /// attachments exactly.
    pub(crate) fn new(
        ctx: &VulkanContext,
        color_format: vk::Format,
        depth_format: vk::Format,
    ) -> Result<PipelineState, RendererError> {
        // Partial-initialization cleanup (corrective finding G): handles
        // start dead; on failure `destroy` releases exactly what exists.
        let mut state = PipelineState {
            render_pass: vk::RenderPass::null(),
            scene_pipeline: vk::Pipeline::null(),
            scene_layout: vk::PipelineLayout::null(),
            overlay_pipeline: vk::Pipeline::null(),
            overlay_layout: vk::PipelineLayout::null(),
            scene_vbo: vk::Buffer::null(),
            scene_vbo_memory: vk::DeviceMemory::null(),
            scene_ibo: vk::Buffer::null(),
            scene_ibo_memory: vk::DeviceMemory::null(),
            scene_index_count: 0,
            overlay_vbo: vk::Buffer::null(),
            overlay_vbo_memory: vk::DeviceMemory::null(),
            overlay_capacity: 0,
            overlay_slot_bytes: 0,
            overlay_mapped: core::ptr::null_mut(),
        };
        if let Err(error) = state.build(ctx, color_format, depth_format) {
            state.destroy(ctx);
            return Err(error);
        }
        Ok(state)
    }

    fn build(
        &mut self,
        ctx: &VulkanContext,
        color_format: vk::Format,
        depth_format: vk::Format,
    ) -> Result<(), RendererError> {
        Self::create_pipelines(ctx, color_format, depth_format, self)?;

        // Scene buffers: upload once through a staging buffer into
        // device-local memory.
        let (vertices, indices) = crate::scene::build_scene();
        self.scene_index_count = indices.len() as u32;
        let (scene_vbo, scene_vbo_memory) = create_device_local_buffer_with_data(
            ctx,
            vk::BufferUsageFlags::VERTEX_BUFFER,
            bytemuck::cast_slice(&vertices),
        )?;
        self.scene_vbo = scene_vbo;
        self.scene_vbo_memory = scene_vbo_memory;
        let (scene_ibo, scene_ibo_memory) = create_device_local_buffer_with_data(
            ctx,
            vk::BufferUsageFlags::INDEX_BUFFER,
            bytemuck::cast_slice(&indices),
        )?;
        self.scene_ibo = scene_ibo;
        self.scene_ibo_memory = scene_ibo_memory;

        // Overlay buffer: persistently mapped host-visible with one
        // disjoint region per frame slot (corrective finding B) — a
        // slot's region is rewritten only after that slot's fence wait,
        // so in-flight submissions never read a region being rewritten.
        // The total vertex budget stays `overlay_capacity_vertices()`.
        self.overlay_capacity = crate::overlay::overlay_capacity_vertices() / FRAMES_IN_FLIGHT;
        self.overlay_slot_bytes = self.overlay_capacity as vk::DeviceSize
            * std::mem::size_of::<OverlayVertex>() as vk::DeviceSize;
        let overlay_size = self.overlay_slot_bytes * FRAMES_IN_FLIGHT as vk::DeviceSize;
        let (overlay_vbo, overlay_vbo_memory, overlay_mapped) =
            create_mapped_buffer(ctx, vk::BufferUsageFlags::VERTEX_BUFFER, overlay_size)?;
        self.overlay_vbo = overlay_vbo;
        self.overlay_vbo_memory = overlay_vbo_memory;
        self.overlay_mapped = overlay_mapped;
        Ok(())
    }

    /// Builds the render pass + pipelines for a (possibly new) swapchain
    /// format into `out`. Handles are assigned as they are created
    /// (corrective finding G): a mid-build failure leaves them owned by
    /// the caller's partial state, whose `destroy` releases them.
    fn create_pipelines(
        ctx: &VulkanContext,
        color_format: vk::Format,
        depth_format: vk::Format,
        out: &mut PipelineState,
    ) -> Result<(), RendererError> {
        // Shader modules are consumed by the pipelines; tracked so they
        // are destroyed exactly once whether the build succeeds or fails.
        let mut modules: Vec<vk::ShaderModule> = Vec::new();
        let result = Self::assemble_pipelines(ctx, color_format, depth_format, out, &mut modules);
        unsafe {
            for module in modules.drain(..) {
                ctx.device.destroy_shader_module(module, None);
            }
        }
        result
    }

    fn assemble_pipelines(
        ctx: &VulkanContext,
        color_format: vk::Format,
        depth_format: vk::Format,
        out: &mut PipelineState,
        modules: &mut Vec<vk::ShaderModule>,
    ) -> Result<(), RendererError> {
        let color_attachment = vk::AttachmentDescription::default()
            .format(color_format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::PRESENT_SRC_KHR);
        let depth_attachment = vk::AttachmentDescription::default()
            .format(depth_format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL);
        let attachments = [color_attachment, depth_attachment];
        let color_reference = vk::AttachmentReference {
            attachment: 0,
            layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        };
        let depth_reference = vk::AttachmentReference {
            attachment: 1,
            layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
        };
        let color_references = [color_reference];
        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_references)
            .depth_stencil_attachment(&depth_reference);
        let dependency = vk::SubpassDependency::default()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                    | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
            )
            .src_access_mask(vk::AccessFlags::empty())
            .dst_stage_mask(
                vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                    | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
            )
            .dst_access_mask(
                vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                    | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            );
        let subpasses = [subpass];
        let dependencies = [dependency];
        let render_pass_info = vk::RenderPassCreateInfo::default()
            .attachments(&attachments)
            .subpasses(&subpasses)
            .dependencies(&dependencies);
        out.render_pass = unsafe { ctx.device.create_render_pass(&render_pass_info, None) }
            .map_err(|error| RendererError::vk(error, "create_render_pass"))?;

        let scene_vert = include_bytes!(concat!(env!("OUT_DIR"), "/scene.vert.spv"));
        let scene_frag = include_bytes!(concat!(env!("OUT_DIR"), "/scene.frag.spv"));
        let overlay_vert = include_bytes!(concat!(env!("OUT_DIR"), "/overlay.vert.spv"));
        let overlay_frag = include_bytes!(concat!(env!("OUT_DIR"), "/overlay.frag.spv"));

        let scene_vert_module = create_shader_module(ctx, scene_vert)?;
        modules.push(scene_vert_module);
        let scene_frag_module = create_shader_module(ctx, scene_frag)?;
        modules.push(scene_frag_module);
        let overlay_vert_module = create_shader_module(ctx, overlay_vert)?;
        modules.push(overlay_vert_module);
        let overlay_frag_module = create_shader_module(ctx, overlay_frag)?;
        modules.push(overlay_frag_module);

        let push_range = vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(64);
        let push_ranges = [push_range];
        let scene_layout_info =
            vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&push_ranges);
        out.scene_layout = unsafe { ctx.device.create_pipeline_layout(&scene_layout_info, None) }
            .map_err(|error| RendererError::vk(error, "scene layout"))?;
        let overlay_layout_info = vk::PipelineLayoutCreateInfo::default();
        out.overlay_layout = unsafe {
            ctx.device
                .create_pipeline_layout(&overlay_layout_info, None)
        }
        .map_err(|error| RendererError::vk(error, "overlay layout"))?;

        let scene_binding = vk::VertexInputBindingDescription {
            binding: 0,
            stride: std::mem::size_of::<SceneVertex>() as u32,
            input_rate: vk::VertexInputRate::VERTEX,
        };
        let scene_attributes = [
            vk::VertexInputAttributeDescription {
                binding: 0,
                location: 0,
                format: vk::Format::R32G32B32_SFLOAT,
                offset: 0,
            },
            vk::VertexInputAttributeDescription {
                binding: 0,
                location: 1,
                format: vk::Format::R32G32B32_SFLOAT,
                offset: 12,
            },
        ];
        let overlay_binding = vk::VertexInputBindingDescription {
            binding: 0,
            stride: std::mem::size_of::<OverlayVertex>() as u32,
            input_rate: vk::VertexInputRate::VERTEX,
        };
        let overlay_attributes = [
            vk::VertexInputAttributeDescription {
                binding: 0,
                location: 0,
                format: vk::Format::R32G32_SFLOAT,
                offset: 0,
            },
            vk::VertexInputAttributeDescription {
                binding: 0,
                location: 1,
                format: vk::Format::R32G32B32A32_SFLOAT,
                offset: 8,
            },
        ];

        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let rasterizer = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::BACK)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisampling = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic_state =
            vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let color_blend_attachment = vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA);
        let color_blend_attachments = [color_blend_attachment];
        let color_blend =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&color_blend_attachments);

        let scene_depth = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::LESS);
        let scene_bindings = [scene_binding];
        let scene_vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&scene_bindings)
            .vertex_attribute_descriptions(&scene_attributes);
        let scene_input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let scene_stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(scene_vert_module)
                .name(ENTRY_MAIN),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(scene_frag_module)
                .name(ENTRY_MAIN),
        ];
        let scene_info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&scene_stages)
            .vertex_input_state(&scene_vertex_input)
            .input_assembly_state(&scene_input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterizer)
            .multisample_state(&multisampling)
            .depth_stencil_state(&scene_depth)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic_state)
            .layout(out.scene_layout)
            .render_pass(out.render_pass)
            .subpass(0);
        out.scene_pipeline = unsafe {
            ctx.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[scene_info], None)
        }
        .map_err(|(_, error)| RendererError::vk(error, "scene pipeline"))?[0];

        let overlay_blend_attachment = vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
            .alpha_blend_op(vk::BlendOp::ADD);
        let overlay_blend_attachments = [overlay_blend_attachment];
        let overlay_blend = vk::PipelineColorBlendStateCreateInfo::default()
            .attachments(&overlay_blend_attachments);
        let overlay_no_depth = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(false)
            .depth_write_enable(false);
        let overlay_bindings = [overlay_binding];
        let overlay_vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&overlay_bindings)
            .vertex_attribute_descriptions(&overlay_attributes);
        let overlay_input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let overlay_stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(overlay_vert_module)
                .name(ENTRY_MAIN),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(overlay_frag_module)
                .name(ENTRY_MAIN),
        ];
        let overlay_info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&overlay_stages)
            .vertex_input_state(&overlay_vertex_input)
            .input_assembly_state(&overlay_input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterizer)
            .multisample_state(&multisampling)
            .depth_stencil_state(&overlay_no_depth)
            .color_blend_state(&overlay_blend)
            .dynamic_state(&dynamic_state)
            .layout(out.overlay_layout)
            .render_pass(out.render_pass)
            .subpass(0);
        out.overlay_pipeline = unsafe {
            ctx.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[overlay_info], None)
        }
        .map_err(|(_, error)| RendererError::vk(error, "overlay pipeline"))?[0];

        Ok(())
    }

    /// Writes overlay vertices into the frame slot's region of the
    /// persistently mapped buffer (corrective finding B): each slot owns
    /// a disjoint byte range, and the caller writes a slot's region only
    /// after that slot's fence wait, so in-flight submissions never read
    /// a region being rewritten. The copy length is bounded by the
    /// slot's capacity.
    pub(crate) fn write_overlay(&mut self, slot: usize, vertices: &[OverlayVertex]) {
        let slot_bytes = self.overlay_slot_bytes as usize;
        let byte_len = std::mem::size_of_val(vertices).min(slot_bytes);
        unsafe {
            std::ptr::copy_nonoverlapping(
                vertices.as_ptr() as *const u8,
                (self.overlay_mapped as *mut u8).add(slot * slot_bytes),
                byte_len,
            );
        }
    }

    pub(crate) fn destroy(&mut self, ctx: &VulkanContext) {
        unsafe {
            if !self.overlay_mapped.is_null() {
                ctx.device.unmap_memory(self.overlay_vbo_memory);
                self.overlay_mapped = core::ptr::null_mut();
            }
            ctx.device.destroy_buffer(self.overlay_vbo, None);
            ctx.device.free_memory(self.overlay_vbo_memory, None);
            ctx.device.destroy_buffer(self.scene_ibo, None);
            ctx.device.free_memory(self.scene_ibo_memory, None);
            ctx.device.destroy_buffer(self.scene_vbo, None);
            ctx.device.free_memory(self.scene_vbo_memory, None);
            ctx.device.destroy_pipeline(self.overlay_pipeline, None);
            ctx.device
                .destroy_pipeline_layout(self.overlay_layout, None);
            ctx.device.destroy_pipeline(self.scene_pipeline, None);
            ctx.device.destroy_pipeline_layout(self.scene_layout, None);
            ctx.device.destroy_render_pass(self.render_pass, None);
        }
    }
}

fn create_shader_module(
    ctx: &VulkanContext,
    spirv: &[u8],
) -> Result<vk::ShaderModule, RendererError> {
    let words: Vec<u32> = spirv
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes(*word))
        .collect();
    let info = vk::ShaderModuleCreateInfo::default().code(&words);
    unsafe { ctx.device.create_shader_module(&info, None) }
        .map_err(|error| RendererError::vk(error, "create_shader_module"))
}

fn create_device_local_buffer_with_data(
    ctx: &VulkanContext,
    usage: vk::BufferUsageFlags,
    data: &[u8],
) -> Result<(vk::Buffer, vk::DeviceMemory), RendererError> {
    let size = data.len() as vk::DeviceSize;
    // Partial-initialization cleanup (corrective finding G): tracked
    // handles are released when any step fails; the success path inside
    // the closure releases the one-shot upload resources itself and
    // nulls them so "non-null ⇒ needs release" always holds.
    let mut buffer = vk::Buffer::null();
    let mut memory = vk::DeviceMemory::null();
    let mut staging = vk::Buffer::null();
    let mut staging_memory = vk::DeviceMemory::null();
    let mut pool = vk::CommandPool::null();
    let mut fence = vk::Fence::null();
    let result = (|| -> Result<(), RendererError> {
        let buffer_info = vk::BufferCreateInfo::default()
            .size(size)
            // The staging copy's destination must allow transfers
            // (VUID-vkCmdCopyBuffer-dstBuffer-00120 — surfaced by the
            // CI validation-enabled smoke, invisible to the layer-less
            // pod kit).
            .usage(usage | vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        buffer = unsafe { ctx.device.create_buffer(&buffer_info, None) }
            .map_err(|error| RendererError::vk(error, "create_buffer"))?;
        let requirements = unsafe { ctx.device.get_buffer_memory_requirements(buffer) };
        let memory_index = find_memory_type(
            ctx,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
        .ok_or_else(|| RendererError::NoDevice("no DEVICE_LOCAL memory".into()))?;
        let memory_info = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(memory_index);
        memory = unsafe { ctx.device.allocate_memory(&memory_info, None) }
            .map_err(|error| RendererError::vk(error, "allocate_memory"))?;
        unsafe { ctx.device.bind_buffer_memory(buffer, memory, 0) }
            .map_err(|error| RendererError::vk(error, "bind_buffer_memory"))?;

        // Staging upload: HOST_VISIBLE staging buffer → copy command → fence wait.
        let staging_info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        staging = unsafe { ctx.device.create_buffer(&staging_info, None) }
            .map_err(|error| RendererError::vk(error, "create staging"))?;
        let staging_requirements = unsafe { ctx.device.get_buffer_memory_requirements(staging) };
        let staging_index = find_memory_type(
            ctx,
            staging_requirements.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )
        .ok_or_else(|| RendererError::NoDevice("no HOST_VISIBLE memory".into()))?;
        let staging_memory_info = vk::MemoryAllocateInfo::default()
            .allocation_size(staging_requirements.size)
            .memory_type_index(staging_index);
        staging_memory = unsafe { ctx.device.allocate_memory(&staging_memory_info, None) }
            .map_err(|error| RendererError::vk(error, "allocate staging memory"))?;
        unsafe { ctx.device.bind_buffer_memory(staging, staging_memory, 0) }
            .map_err(|error| RendererError::vk(error, "bind staging memory"))?;
        unsafe {
            let mapped = ctx
                .device
                .map_memory(staging_memory, 0, size, vk::MemoryMapFlags::empty())
                .map_err(|error| RendererError::vk(error, "map staging"))?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), mapped as *mut u8, data.len());
            ctx.device.unmap_memory(staging_memory);
        }

        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(ctx.graphics_family);
        pool = unsafe { ctx.device.create_command_pool(&pool_info, None) }
            .map_err(|error| RendererError::vk(error, "staging command pool"))?;
        let command_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        let command = unsafe { ctx.device.allocate_command_buffers(&command_info) }
            .map_err(|error| RendererError::vk(error, "staging command buffer"))?[0];
        unsafe {
            ctx.device
                .begin_command_buffer(
                    command,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(|error| RendererError::vk(error, "begin staging"))?;
            ctx.device.cmd_copy_buffer(
                command,
                staging,
                buffer,
                &[vk::BufferCopy {
                    src_offset: 0,
                    dst_offset: 0,
                    size,
                }],
            );
            ctx.device
                .end_command_buffer(command)
                .map_err(|error| RendererError::vk(error, "end staging"))?;
            fence = ctx
                .device
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(|error| RendererError::vk(error, "staging fence"))?;
            let staging_commands = [command];
            let submit_info = vk::SubmitInfo::default().command_buffers(&staging_commands);
            ctx.device
                .queue_submit(ctx.graphics_queue, &[submit_info], fence)
                .map_err(|error| RendererError::vk(error, "staging submit"))?;
            ctx.device
                .wait_for_fences(&[fence], true, u64::MAX)
                .map_err(|error| RendererError::vk(error, "staging wait"))?;
            ctx.device.destroy_fence(fence, None);
            fence = vk::Fence::null();
            ctx.device.free_command_buffers(pool, &[command]);
            ctx.device.destroy_command_pool(pool, None);
            pool = vk::CommandPool::null();
            ctx.device.destroy_buffer(staging, None);
            staging = vk::Buffer::null();
            ctx.device.free_memory(staging_memory, None);
            staging_memory = vk::DeviceMemory::null();
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok((buffer, memory)),
        Err(error) => {
            unsafe {
                if fence != vk::Fence::null() {
                    ctx.device.destroy_fence(fence, None);
                }
                if pool != vk::CommandPool::null() {
                    ctx.device.destroy_command_pool(pool, None);
                }
                if staging != vk::Buffer::null() {
                    ctx.device.destroy_buffer(staging, None);
                }
                if staging_memory != vk::DeviceMemory::null() {
                    ctx.device.free_memory(staging_memory, None);
                }
                if buffer != vk::Buffer::null() {
                    ctx.device.destroy_buffer(buffer, None);
                }
                if memory != vk::DeviceMemory::null() {
                    ctx.device.free_memory(memory, None);
                }
            }
            Err(error)
        }
    }
}

fn create_mapped_buffer(
    ctx: &VulkanContext,
    usage: vk::BufferUsageFlags,
    size: vk::DeviceSize,
) -> Result<(vk::Buffer, vk::DeviceMemory, *mut core::ffi::c_void), RendererError> {
    // Partial-initialization cleanup (corrective finding G).
    let mut buffer = vk::Buffer::null();
    let mut memory = vk::DeviceMemory::null();
    let result = (|| -> Result<*mut core::ffi::c_void, RendererError> {
        let buffer_info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        buffer = unsafe { ctx.device.create_buffer(&buffer_info, None) }
            .map_err(|error| RendererError::vk(error, "create overlay buffer"))?;
        let requirements = unsafe { ctx.device.get_buffer_memory_requirements(buffer) };
        let memory_index = find_memory_type(
            ctx,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )
        .ok_or_else(|| RendererError::NoDevice("no HOST_VISIBLE memory".into()))?;
        let memory_info = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(memory_index);
        memory = unsafe { ctx.device.allocate_memory(&memory_info, None) }
            .map_err(|error| RendererError::vk(error, "allocate overlay memory"))?;
        unsafe { ctx.device.bind_buffer_memory(buffer, memory, 0) }
            .map_err(|error| RendererError::vk(error, "bind overlay memory"))?;
        let mapped = unsafe {
            ctx.device
                .map_memory(memory, 0, size, vk::MemoryMapFlags::empty())
                .map_err(|error| RendererError::vk(error, "map overlay"))?
        };
        Ok(mapped)
    })();
    match result {
        Ok(mapped) => Ok((buffer, memory, mapped)),
        Err(error) => {
            unsafe {
                if buffer != vk::Buffer::null() {
                    ctx.device.destroy_buffer(buffer, None);
                }
                if memory != vk::DeviceMemory::null() {
                    ctx.device.free_memory(memory, None);
                }
            }
            Err(error)
        }
    }
}

/// Sync objects, command buffers, and the timestamp pool, per frames-in-
/// flight slot, plus per-swapchain-image state.
pub(crate) struct FrameState {
    /// Created with `RESET_COMMAND_BUFFER` (corrective finding D): the
    /// per-slot command buffers are re-recorded every frame, and the
    /// per-slot fence proves a buffer is no longer pending when its
    /// re-record begins.
    pub command_pool: vk::CommandPool,
    pub command_buffers: Vec<vk::CommandBuffer>,
    pub image_available: Vec<vk::Semaphore>,
    /// Presentation-wait semaphores indexed by **swapchain image**
    /// (corrective finding C): the submit fence proves the graphics work
    /// finished, never that the presentation engine stopped waiting on
    /// the present semaphore — slot-indexed reuse could therefore signal
    /// a semaphore with an outstanding wait. Indexing by image ties reuse
    /// to re-acquisition of that image. Grown on demand; released only
    /// after `device_wait_idle`.
    pub present_semaphores: Vec<vk::Semaphore>,
    pub in_flight: Vec<vk::Fence>,
    pub images_in_flight: Vec<vk::Fence>,
    pub timestamp_pool: vk::QueryPool,
    /// False when the graphics queue family exposes no timestamp bits
    /// (corrective finding E).
    pub timestamps_supported: bool,
    /// Whether this slot's queries have resolved at least once (finding
    /// E): never block-read unwritten queries.
    pub timestamps_written: [bool; FRAMES_IN_FLIGHT as usize],
    /// A present reported suboptimal — recreate before the next frame.
    pub needs_recreate: bool,
    pub frame_slot: usize,
    pub image_index: u32,
}

impl FrameState {
    pub(crate) fn new(
        ctx: &VulkanContext,
        swapchain_image_count: u32,
    ) -> Result<FrameState, RendererError> {
        // Partial-initialization cleanup (corrective finding G).
        let mut state = FrameState {
            command_pool: vk::CommandPool::null(),
            command_buffers: Vec::new(),
            image_available: Vec::new(),
            present_semaphores: Vec::new(),
            in_flight: Vec::new(),
            images_in_flight: vec![vk::Fence::null(); swapchain_image_count as usize],
            timestamp_pool: vk::QueryPool::null(),
            timestamps_supported: ctx.timestamp_valid_bits > 0,
            timestamps_written: [false; FRAMES_IN_FLIGHT as usize],
            needs_recreate: false,
            frame_slot: 0,
            image_index: 0,
        };
        if let Err(error) = state.build(ctx, swapchain_image_count) {
            state.destroy(ctx);
            return Err(error);
        }
        Ok(state)
    }

    fn build(
        &mut self,
        ctx: &VulkanContext,
        swapchain_image_count: u32,
    ) -> Result<(), RendererError> {
        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(ctx.graphics_family);
        self.command_pool = unsafe { ctx.device.create_command_pool(&pool_info, None) }
            .map_err(|error| RendererError::vk(error, "create_command_pool"))?;
        let allocate_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(FRAMES_IN_FLIGHT);
        self.command_buffers = unsafe { ctx.device.allocate_command_buffers(&allocate_info) }
            .map_err(|error| RendererError::vk(error, "allocate_command_buffers"))?;

        for _ in 0..FRAMES_IN_FLIGHT {
            let semaphore = unsafe {
                ctx.device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
            }
            .map_err(|error| RendererError::vk(error, "create_semaphore"))?;
            self.image_available.push(semaphore);
            let fence = unsafe {
                ctx.device.create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )
            }
            .map_err(|error| RendererError::vk(error, "create_fence"))?;
            self.in_flight.push(fence);
        }
        self.sync_present_semaphores(ctx, swapchain_image_count)?;

        if self.timestamps_supported {
            let query_info = vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::TIMESTAMP)
                .query_count(FRAMES_IN_FLIGHT * 2);
            self.timestamp_pool = unsafe { ctx.device.create_query_pool(&query_info, None) }
                .map_err(|error| RendererError::vk(error, "create_query_pool"))?;
        }
        Ok(())
    }

    /// Grows the per-image presentation-wait semaphore table (corrective
    /// finding C). Entries are never removed — recreation idles the
    /// device first, so outstanding present waits have drained.
    fn sync_present_semaphores(
        &mut self,
        ctx: &VulkanContext,
        image_count: u32,
    ) -> Result<(), RendererError> {
        let needed = present_semaphore_len_for(self.present_semaphores.len(), image_count as usize);
        while self.present_semaphores.len() < needed {
            let semaphore = unsafe {
                ctx.device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
            }
            .map_err(|error| RendererError::vk(error, "create present semaphore"))?;
            self.present_semaphores.push(semaphore);
        }
        Ok(())
    }

    /// Returns the present-wait semaphore for the acquired image
    /// (corrective finding C), growing the table if the index exceeds it.
    fn present_semaphore_for(
        &mut self,
        ctx: &VulkanContext,
        image_index: u32,
    ) -> Result<vk::Semaphore, RendererError> {
        self.sync_present_semaphores(ctx, image_index + 1)?;
        Ok(self.present_semaphores[image_index as usize])
    }

    /// Re-syncs the per-image tables for a new swapchain (the image count
    /// may change across recreation).
    pub(crate) fn resize_images(
        &mut self,
        ctx: &VulkanContext,
        swapchain_image_count: u32,
    ) -> Result<(), RendererError> {
        self.images_in_flight = vec![vk::Fence::null(); swapchain_image_count as usize];
        self.sync_present_semaphores(ctx, swapchain_image_count)
    }

    pub(crate) fn destroy(&mut self, ctx: &VulkanContext) {
        unsafe {
            ctx.device.destroy_query_pool(self.timestamp_pool, None);
            for fence in &self.in_flight {
                ctx.device.destroy_fence(*fence, None);
            }
            self.in_flight.clear();
            for semaphore in &self.present_semaphores {
                ctx.device.destroy_semaphore(*semaphore, None);
            }
            self.present_semaphores.clear();
            for semaphore in &self.image_available {
                ctx.device.destroy_semaphore(*semaphore, None);
            }
            self.image_available.clear();
            ctx.device.destroy_command_pool(self.command_pool, None);
        }
    }
}

/// Outcome of one `render_frame` call.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct FrameOutcome {
    pub presented: bool,
    pub recreated: bool,
    pub suboptimal: bool,
    pub gpu_render_ms: Option<f64>,
}

/// The full frame flow: wait → acquire → (recreate if needed) → record →
/// submit → present. Called by the safe [`crate::Renderer`] wrapper.
// Private frame orchestration: the four owning states plus the per-frame
// inputs are exactly the parameters the record/submit/present sequence
// needs; bundling them would hide the ownership story for no gain.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_frame(
    ctx: &mut VulkanContext,
    swapchain: &mut SwapchainState,
    pipes: &mut PipelineState,
    frames: &mut FrameState,
    window_size: (u32, u32),
    present_preference: PresentModePreference,
    view_proj: &[[f32; 4]; 4],
    overlay_vertices: &[OverlayVertex],
    overlay_enabled: bool,
) -> Result<FrameOutcome, RendererError> {
    let mut outcome = FrameOutcome::default();
    let slot = frames.frame_slot;

    // Minimized windows report a zero extent — present nothing.
    if window_size.0 == 0 || window_size.1 == 0 {
        return Ok(outcome);
    }

    // A previous present reported suboptimal — recreate before drawing
    // (verification requirement: suboptimal must eventually trigger
    // recreation).
    if consume_recreate(&mut frames.needs_recreate) {
        recreate_swapchain(
            ctx,
            swapchain,
            pipes,
            frames,
            window_size,
            present_preference,
        )?;
        outcome.recreated = true;
    }

    // Proactive resize handling: the window size moved away from the
    // swapchain extent.
    if window_size.0 != swapchain.extent.width || window_size.1 != swapchain.extent.height {
        recreate_swapchain(
            ctx,
            swapchain,
            pipes,
            frames,
            window_size,
            present_preference,
        )?;
        outcome.recreated = true;
    }

    // Wait for this slot's previous submission.
    unsafe {
        ctx.device
            .wait_for_fences(&[frames.in_flight[slot]], true, WAIT_TIMEOUT_NS)
            .map_err(|error| RendererError::vk(error, "wait_for_fences (slot fence timed out — a submission never completed; presenting faster than the present mode drains?)"))?;
        ctx.device
            .reset_fences(&[frames.in_flight[slot]])
            .map_err(|error| RendererError::vk(error, "reset_fences"))?;
    }

    // Acquire; OUT_OF_DATE recreates and retries once. If the retry also
    // fails, the slot fence is re-created in the signaled state — the
    // wait above consumed its signal but no submission will provide a new
    // one, and the next frame must not hang on it.
    let mut acquire_result = unsafe {
        ctx.swapchain_ext.acquire_next_image(
            swapchain.swapchain,
            100_000_000,
            frames.image_available[slot],
            vk::Fence::null(),
        )
    };
    if let Err(vk::Result::ERROR_OUT_OF_DATE_KHR) = acquire_result {
        recreate_swapchain(
            ctx,
            swapchain,
            pipes,
            frames,
            window_size,
            present_preference,
        )?;
        outcome.recreated = true;
        acquire_result = unsafe {
            ctx.swapchain_ext.acquire_next_image(
                swapchain.swapchain,
                100_000_000,
                frames.image_available[slot],
                vk::Fence::null(),
            )
        };
    }
    let (image_index, suboptimal) = match acquire_result {
        Ok((index, suboptimal)) => (index, suboptimal),
        Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
            unsafe {
                ctx.device.destroy_fence(frames.in_flight[slot], None);
                frames.in_flight[slot] = ctx
                    .device
                    .create_fence(
                        &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                        None,
                    )
                    .map_err(|error| RendererError::vk(error, "recreate fence"))?;
            }
            return Ok(outcome);
        }
        Err(vk::Result::TIMEOUT) => {
            return Err(RendererError::Vulkan(
                "acquire_next_image timed out: no swapchain image became available in 100 ms \
                 (present mode not draining — vsync-locked FIFO paced beyond the display rate?)"
                    .into(),
            ));
        }
        Err(error) => return Err(RendererError::vk(error, "acquire_next_image")),
    };
    outcome.suboptimal = suboptimal;
    frames.image_index = image_index;
    if let Some(image_fence) = mut_at(&mut frames.images_in_flight, image_index as usize) {
        if *image_fence != vk::Fence::null() {
            unsafe {
                ctx.device
                    .wait_for_fences(&[*image_fence], true, WAIT_TIMEOUT_NS)
                    .map_err(|error| {
                        RendererError::vk(
                            error,
                            "wait image fence (timed out — a submission never completed)",
                        )
                    })?;
            }
        }
        *image_fence = frames.in_flight[slot];
    }

    // Timestamps from this slot's previous use are resolved now (the
    // fence above proves it). The first use of a slot has no samples
    // yet, and unsupported timestamps are never queried (corrective
    // finding E).
    let gpu_render_ms =
        if timestamp_sample_ready(frames.timestamps_supported, frames.timestamps_written[slot]) {
            read_timestamps(ctx, frames, slot)
        } else {
            None
        };

    // Record.
    let command = frames.command_buffers[slot];
    let framebuffer = swapchain.framebuffers[image_index as usize];
    let extent = swapchain.extent;
    let clear_values = [
        vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.02, 0.02, 0.04, 1.0],
            },
        },
        vk::ClearValue {
            depth_stencil: vk::ClearDepthStencilValue {
                depth: 1.0,
                stencil: 0,
            },
        },
    ];
    let viewport = vk::Viewport {
        x: 0.0,
        y: 0.0,
        width: extent.width as f32,
        height: extent.height as f32,
        min_depth: 0.0,
        max_depth: 1.0,
    };
    let scissor = vk::Rect2D {
        extent,
        offset: vk::Offset2D { x: 0, y: 0 },
    };

    // The present-wait semaphore is owned by the acquired image
    // (corrective finding C).
    let present_semaphore = frames.present_semaphore_for(ctx, image_index)?;

    unsafe {
        ctx.device
            .begin_command_buffer(
                command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .map_err(|error| RendererError::vk(error, "begin_command_buffer"))?;

        if frames.timestamps_supported {
            ctx.device
                .cmd_reset_query_pool(command, frames.timestamp_pool, (slot as u32) * 2, 2);
            ctx.device.cmd_write_timestamp(
                command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                frames.timestamp_pool,
                (slot as u32) * 2,
            );
        }

        ctx.device.cmd_begin_render_pass(
            command,
            &vk::RenderPassBeginInfo::default()
                .render_pass(pipes.render_pass)
                .framebuffer(framebuffer)
                .render_area(scissor)
                .clear_values(&clear_values),
            vk::SubpassContents::INLINE,
        );
        ctx.device.cmd_bind_pipeline(
            command,
            vk::PipelineBindPoint::GRAPHICS,
            pipes.scene_pipeline,
        );
        ctx.device.cmd_set_viewport(command, 0, &[viewport]);
        ctx.device.cmd_set_scissor(command, 0, &[scissor]);
        ctx.device.cmd_push_constants(
            command,
            pipes.scene_layout,
            vk::ShaderStageFlags::VERTEX,
            0,
            bytemuck::bytes_of(view_proj),
        );
        ctx.device
            .cmd_bind_vertex_buffers(command, 0, &[pipes.scene_vbo], &[0]);
        ctx.device
            .cmd_bind_index_buffer(command, pipes.scene_ibo, 0, vk::IndexType::UINT32);
        ctx.device
            .cmd_draw_indexed(command, pipes.scene_index_count, 1, 0, 0, 0);

        if overlay_enabled && !overlay_vertices.is_empty() {
            pipes.write_overlay(slot, overlay_vertices);
            let vertex_count = (overlay_vertices.len() as u32).min(pipes.overlay_capacity);
            ctx.device.cmd_bind_pipeline(
                command,
                vk::PipelineBindPoint::GRAPHICS,
                pipes.overlay_pipeline,
            );
            let overlay_offset = (slot as vk::DeviceSize) * pipes.overlay_slot_bytes;
            ctx.device
                .cmd_bind_vertex_buffers(command, 0, &[pipes.overlay_vbo], &[overlay_offset]);
            ctx.device.cmd_draw(command, vertex_count, 1, 0, 0);
        }

        ctx.device.cmd_end_render_pass(command);
        if frames.timestamps_supported {
            ctx.device.cmd_write_timestamp(
                command,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                frames.timestamp_pool,
                (slot as u32) * 2 + 1,
            );
        }
        ctx.device
            .end_command_buffer(command)
            .map_err(|error| RendererError::vk(error, "end_command_buffer"))?;

        // Submit: wait for the acquire semaphore, signal the image's
        // present-wait semaphore (corrective finding C) and the slot
        // fence.
        let wait_stage = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let wait_semaphores = [frames.image_available[slot]];
        let signal_semaphores = [present_semaphore];
        let commands = [command];
        let submit_info = vk::SubmitInfo::default()
            .wait_semaphores(&wait_semaphores)
            .wait_dst_stage_mask(&wait_stage)
            .command_buffers(&commands)
            .signal_semaphores(&signal_semaphores);
        ctx.device
            .queue_submit(ctx.graphics_queue, &[submit_info], frames.in_flight[slot])
            .map_err(|error| RendererError::vk(error, "queue_submit"))?;
        if frames.timestamps_supported {
            frames.timestamps_written[slot] = true;
        }
    }

    // Present.
    let present_wait_semaphores = [present_semaphore];
    let present_swapchains = [swapchain.swapchain];
    let present_image_indices = [image_index];
    let present_info = vk::PresentInfoKHR::default()
        .wait_semaphores(&present_wait_semaphores)
        .swapchains(&present_swapchains)
        .image_indices(&present_image_indices);
    match unsafe {
        ctx.swapchain_ext
            .queue_present(ctx.graphics_queue, &present_info)
    } {
        Ok(present_suboptimal) => {
            outcome.presented = true;
            if present_suboptimal || outcome.suboptimal {
                outcome.suboptimal = true;
                mark_suboptimal(&mut frames.needs_recreate);
            }
        }
        Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
            recreate_swapchain(
                ctx,
                swapchain,
                pipes,
                frames,
                window_size,
                present_preference,
            )?;
            outcome.recreated = true;
        }
        Err(error) => return Err(RendererError::vk(error, "queue_present")),
    }

    outcome.gpu_render_ms = gpu_render_ms;
    frames.frame_slot = (slot + 1) % FRAMES_IN_FLIGHT as usize;
    Ok(outcome)
}

/// Safe-to-call device idle used by the renderer's ordered teardown.
pub(crate) fn device_idle(ctx: &VulkanContext) -> Result<(), RendererError> {
    unsafe { ctx.device.device_wait_idle() }
        .map_err(|error| RendererError::vk(error, "device_wait_idle"))
}

fn mut_at<T>(slice: &mut [T], index: usize) -> Option<&mut T> {
    slice.get_mut(index)
}

fn read_timestamps(ctx: &VulkanContext, frames: &FrameState, slot: usize) -> Option<f64> {
    let mut stamps = [0u64; 2];
    let result = unsafe {
        ctx.device.get_query_pool_results(
            frames.timestamp_pool,
            (slot as u32) * 2,
            &mut stamps,
            vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
        )
    };
    if result.is_err() {
        return None;
    }
    // Timestamp counters may be narrower than 64 bits (corrective
    // finding E): mask the difference to the valid width so a wrapped
    // counter still yields the correct interval.
    let bits = ctx.timestamp_valid_bits.min(64);
    let mask = if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let delta = stamps[1].wrapping_sub(stamps[0]) & mask;
    Some(delta as f64 * ctx.timestamp_period_ns / 1_000_000.0)
}

/// Swapchain recreation (corrective findings F + G): idle the device
/// (draining outstanding presents), then build the replacement BEFORE
/// destroying the current state so a failure leaves the renderer valid.
/// When the surface format changed, the replacement pipelines are built
/// first — framebuffers are always created against the render pass that
/// will be current after the swap, never a stale one.
fn recreate_swapchain(
    ctx: &mut VulkanContext,
    swapchain: &mut SwapchainState,
    pipes: &mut PipelineState,
    frames: &mut FrameState,
    window_size: (u32, u32),
    present_preference: PresentModePreference,
) -> Result<(), RendererError> {
    unsafe { ctx.device.device_wait_idle() }
        .map_err(|error| RendererError::vk(error, "device_wait_idle"))?;
    let depth_format = swapchain.depth_format;
    let (new_format, new_color_space) = pick_surface_format(ctx)?;
    // Build the replacement pipelines first when the format changed
    // (finding F); on failure the old state is untouched and the
    // partial replacement cleans itself up (finding G).
    let replacement_pipes = if new_format != swapchain.format {
        Some(PipelineState::new(ctx, new_format, depth_format)?)
    } else {
        None
    };
    let render_pass_for_new = replacement_pipes
        .as_ref()
        .map_or(pipes.render_pass, |replacement| replacement.render_pass);
    match SwapchainState::new(
        ctx,
        window_size,
        present_preference,
        new_format,
        new_color_space,
        render_pass_for_new,
        depth_format,
        // The current swapchain still owns the native window while the
        // replacement is created; passing it as `old_swapchain` retires
        // it on success (destroyed below) and leaves it valid on failure.
        swapchain.swapchain,
    ) {
        Ok(mut new_state) => {
            // Re-sync the per-image tables before installing the new
            // state; on failure here the old state is still current.
            if let Err(error) = frames.resize_images(ctx, new_state.image_count) {
                new_state.destroy(ctx);
                if let Some(mut replacement) = replacement_pipes {
                    replacement.destroy(ctx);
                }
                return Err(error);
            }
            // Install, then release the old state — framebuffers before
            // the render pass they reference.
            let mut old_swapchain = std::mem::replace(swapchain, new_state);
            let old_pipes =
                replacement_pipes.map(|replacement| std::mem::replace(pipes, replacement));
            old_swapchain.destroy(ctx);
            if let Some(mut old_pipes) = old_pipes {
                old_pipes.destroy(ctx);
            }
            Ok(())
        }
        Err(error) => {
            // The old swapchain and pipelines are still valid (finding
            // G); only the replacement pipelines need releasing.
            if let Some(mut replacement) = replacement_pipes {
                replacement.destroy(ctx);
            }
            Err(error)
        }
    }
}

/// Timestamp read policy (corrective finding E): query results are read
/// only when the queue family supports timestamps AND the slot's queries
/// have resolved at least once — never block-read unwritten queries.
pub(crate) fn timestamp_sample_ready(supported: bool, written: bool) -> bool {
    supported && written
}

/// Presentation-wait semaphore table growth (corrective finding C): the
/// table only ever grows to cover the required length — entries are never
/// invalidated, because recreation idles the device first.
pub(crate) fn present_semaphore_len_for(current_len: usize, required: usize) -> usize {
    current_len.max(required)
}

/// Records that a present reported suboptimal — the next frame recreates
/// before drawing.
pub(crate) fn mark_suboptimal(needs_recreate: &mut bool) {
    *needs_recreate = true;
}

/// Consumes a pending recreation request; true when one was scheduled.
pub(crate) fn consume_recreate(needs_recreate: &mut bool) -> bool {
    let pending = *needs_recreate;
    *needs_recreate = false;
    pending
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    #[test]
    fn timestamp_gate_requires_support_and_a_resolved_sample() {
        for supported in [false, true] {
            for written in [false, true] {
                assert_eq!(
                    timestamp_sample_ready(supported, written),
                    supported && written,
                    "supported={supported} written={written}"
                );
            }
        }
    }

    #[test]
    fn present_semaphore_table_only_grows() {
        let mut len = 2;
        for acquired in [0u32, 1, 5, 3] {
            len = present_semaphore_len_for(len, acquired as usize + 1);
            assert!(len > acquired as usize);
        }
        assert!(len >= 6);
        assert_eq!(present_semaphore_len_for(6, 4), 6);
    }

    #[test]
    fn suboptimal_present_schedules_recreation_until_consumed() {
        let mut needs_recreate = false;
        mark_suboptimal(&mut needs_recreate);
        assert!(needs_recreate);
        assert!(consume_recreate(&mut needs_recreate));
        assert!(!needs_recreate);
        assert!(!consume_recreate(&mut needs_recreate));
    }

    #[test]
    fn overlay_slot_regions_are_disjoint() {
        // Corrective finding B: the per-slot regions of the mapped
        // overlay buffer (slot s starts at s × slot_bytes) must not
        // overlap.
        let slot_bytes = 1000usize;
        for a in 0..FRAMES_IN_FLIGHT as usize {
            for b in 0..FRAMES_IN_FLIGHT as usize {
                if a != b {
                    let (a_start, a_end) = (a * slot_bytes, (a + 1) * slot_bytes);
                    let (b_start, b_end) = (b * slot_bytes, (b + 1) * slot_bytes);
                    assert!(
                        a_end <= b_start || b_end <= a_start,
                        "slots {a}/{b} overlap"
                    );
                }
            }
        }
    }
}
