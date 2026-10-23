//! Where frames go: a `VK_KHR_display` swapchain on `VideoOut`, or offscreen images on a box
//! without a display. Both hand the presenter an image index to render into.

use anyhow::{anyhow, Context, Result};
use ash::vk;

use crate::device::{GpuDevice, Target};

/// The SDR format. The CSC writes non-linear values, so an `_SRGB` view would apply the
/// transfer twice.
pub const FORMAT: vk::Format = vk::Format::B8G8R8A8_UNORM;
/// The HDR10 format: PQ code values in BT.2020, red in the low bits as `VideoOut` scans it out.
pub const FORMAT_HDR10: vk::Format = vk::Format::A2B10G10R10_UNORM_PACK32;

/// The highest refresh this client asks for, in mHz: 119.88 Hz with a little slack.
const MAX_REFRESH_MHZ: u32 = 120_500;

enum Kind {
    Swapchain {
        surface_i: ash::khr::surface::Instance,
        swap_d: ash::khr::swapchain::Device,
        surface: vk::SurfaceKHR,
        swapchain: vk::SwapchainKHR,
    },
    Offscreen {
        memory: Vec<vk::DeviceMemory>,
        next: u32,
    },
}

pub struct Output {
    kind: Kind,
    pub extent: vk::Extent2D,
    pub refresh_mhz: u32,
    /// [`FORMAT_HDR10`] in `HDR10_ST2084`, else [`FORMAT`] in sRGB.
    pub hdr: bool,
    pub format: vk::Format,
    pub images: Vec<vk::Image>,
    pub views: Vec<vk::ImageView>,
    /// The layout a finished frame leaves the image in.
    pub final_layout: vk::ImageLayout,
}

impl Output {
    /// An HDR10 output where the target can show one; an offscreen target stays SDR.
    pub fn new(dev: &GpuDevice, hdr: bool) -> Result<Self> {
        match dev.target {
            Target::Display => Self::display(dev, hdr),
            Target::Offscreen { width, height } => Self::offscreen(dev, width, height),
        }
    }

    fn display(dev: &GpuDevice, hdr: bool) -> Result<Self> {
        let display_i = ash::khr::display::Instance::new(&dev.entry, &dev.instance);
        // SAFETY: read-only queries on a live physical device.
        let displays = unsafe { display_i.get_physical_device_display_properties(dev.pdev) }?;
        let screen = displays
            .first()
            .ok_or_else(|| anyhow!("the driver reports no display"))?;
        // SAFETY: the driver owns the name string for the instance's life; it is read here only.
        let name = unsafe { screen.display_name_as_c_str() }.unwrap_or_default();
        tracing::info!(
            "display: {:?} {}x{}",
            name,
            screen.physical_resolution.width,
            screen.physical_resolution.height
        );
        // SAFETY: as above.
        let modes = unsafe { display_i.get_display_mode_properties(dev.pdev, screen.display) }?;
        for m in &modes {
            let p = m.parameters;
            tracing::info!(
                "display mode {}x{} @ {}.{:03} Hz",
                p.visible_region.width,
                p.visible_region.height,
                p.refresh_rate / 1000,
                p.refresh_rate % 1000
            );
        }
        let mode = modes
            .iter()
            .filter(|m| m.parameters.refresh_rate <= MAX_REFRESH_MHZ)
            .max_by_key(|m| {
                let r = m.parameters.visible_region;
                (u64::from(r.width) * u64::from(r.height), m.parameters.refresh_rate)
            })
            .ok_or_else(|| anyhow!("no display mode at or under 120 Hz"))?;
        // SAFETY: as above.
        let planes = unsafe { display_i.get_physical_device_display_plane_properties(dev.pdev) }?;
        let plane = (0..planes.len() as u32)
            .find(|&i| {
                // SAFETY: as above; `i` is a valid plane index.
                unsafe { display_i.get_display_plane_supported_displays(dev.pdev, i) }
                    .is_ok_and(|d| d.contains(&screen.display))
            })
            .ok_or_else(|| anyhow!("no display plane drives the display"))?;
        let extent = mode.parameters.visible_region;
        let ci = vk::DisplaySurfaceCreateInfoKHR::default()
            .display_mode(mode.display_mode)
            .plane_index(plane)
            .plane_stack_index(planes[plane as usize].current_stack_index)
            .transform(vk::SurfaceTransformFlagsKHR::IDENTITY)
            .alpha_mode(vk::DisplayPlaneAlphaFlagsKHR::OPAQUE)
            .global_alpha(1.0)
            .image_extent(extent);
        // SAFETY: `ci` outlives the call; the surface is destroyed in `Drop`.
        let surface = unsafe { display_i.create_display_plane_surface(&ci, None) }.context("display plane surface")?;
        let surface_i = ash::khr::surface::Instance::new(&dev.entry, &dev.instance);
        let swap_d = ash::khr::swapchain::Device::new(&dev.instance, &dev.device);
        // SAFETY: live physical device and surface.
        let caps = unsafe { surface_i.get_physical_device_surface_capabilities(dev.pdev, surface) }?;
        let mut count = 3.max(caps.min_image_count);
        if caps.max_image_count > 0 {
            count = count.min(caps.max_image_count);
        }
        // SAFETY: as above.
        let formats = unsafe { surface_i.get_physical_device_surface_formats(dev.pdev, surface) }?;
        let (want, space) = if hdr {
            (FORMAT_HDR10, vk::ColorSpaceKHR::HDR10_ST2084_EXT)
        } else {
            (FORMAT, vk::ColorSpaceKHR::SRGB_NONLINEAR)
        };
        let format = formats
            .iter()
            .find(|f| f.format == want && f.color_space == space)
            .ok_or_else(|| anyhow!("the display surface offers no {want:?} in {space:?}: {formats:?}"))?;
        let ci = vk::SwapchainCreateInfoKHR::default()
            .surface(surface)
            .min_image_count(count)
            .image_format(want)
            .image_color_space(format.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(vk::SurfaceTransformFlagsKHR::IDENTITY)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(vk::PresentModeKHR::FIFO)
            .clipped(true);
        // SAFETY: `ci` outlives the call; the swapchain is destroyed in `Drop`.
        let swapchain = unsafe { swap_d.create_swapchain(&ci, None) }.context("display swapchain")?;
        // SAFETY: the swapchain is live; its images belong to it.
        let images = unsafe { swap_d.get_swapchain_images(swapchain) }?;
        let views = views(dev, &images, want)?;
        tracing::info!(
            "swapchain {}x{} @ {} mHz, {} images, FIFO, {}",
            extent.width,
            extent.height,
            mode.parameters.refresh_rate,
            images.len(),
            if hdr { "HDR10" } else { "SDR" }
        );
        Ok(Self {
            kind: Kind::Swapchain {
                surface_i,
                swap_d,
                surface,
                swapchain,
            },
            extent,
            refresh_mhz: mode.parameters.refresh_rate,
            hdr,
            format: want,
            images,
            views,
            final_layout: vk::ImageLayout::PRESENT_SRC_KHR,
        })
    }

    fn offscreen(dev: &GpuDevice, width: u32, height: u32) -> Result<Self> {
        let extent = vk::Extent2D { width, height };
        let mut images = Vec::new();
        let mut memory = Vec::new();
        for _ in 0..2 {
            let (image, mem) = crate::util::image(
                dev,
                extent,
                FORMAT,
                vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )?;
            images.push(image);
            memory.push(mem);
        }
        let views = views(dev, &images, FORMAT)?;
        Ok(Self {
            kind: Kind::Offscreen { memory, next: 0 },
            extent,
            refresh_mhz: 60_000,
            hdr: false,
            format: FORMAT,
            images,
            views,
            final_layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        })
    }

    /// Whether frames reach a display, so a present waits on a semaphore.
    pub fn presents(&self) -> bool {
        matches!(self.kind, Kind::Swapchain { .. })
    }

    /// The next image to render into. A swapchain signals `ready` when it may be written.
    pub fn acquire(&mut self, ready: vk::Semaphore) -> Result<u32> {
        match &mut self.kind {
            Kind::Swapchain { swap_d, swapchain, .. } => {
                // SAFETY: `ready` is unsignalled: its previous signal was consumed by the submit
                // whose fence the presenter waited before calling this.
                let (index, _suboptimal) =
                    unsafe { swap_d.acquire_next_image(*swapchain, u64::MAX, ready, vk::Fence::null()) }
                        .map_err(|e| anyhow!("acquire_next_image: {e}"))?;
                Ok(index)
            }
            Kind::Offscreen { next, .. } => {
                let index = *next;
                *next = (*next + 1) % 2;
                Ok(index)
            }
        }
    }

    /// Queues image `index` for scan-out once `done` signals. Hold the queue lock.
    pub fn present(&self, queue: vk::Queue, index: u32, done: vk::Semaphore) -> Result<()> {
        let Kind::Swapchain { swap_d, swapchain, .. } = &self.kind else {
            return Ok(());
        };
        let waits = [done];
        let swapchains = [*swapchain];
        let indices = [index];
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&waits)
            .swapchains(&swapchains)
            .image_indices(&indices);
        // SAFETY: the caller holds the queue lock; every handle is live and owned here.
        unsafe { swap_d.queue_present(queue, &info) }.map_err(|e| anyhow!("queue_present: {e}"))?;
        Ok(())
    }

    /// Destroys everything this output owns. The device must be idle.
    ///
    /// # Safety
    /// `dev` is the device this output was built on, and no queued work uses it.
    pub unsafe fn destroy(&mut self, dev: &GpuDevice) {
        // SAFETY: the caller guarantees the device is idle and owns these handles.
        unsafe {
            for view in self.views.drain(..) {
                dev.device.destroy_image_view(view, None);
            }
            match &mut self.kind {
                Kind::Swapchain {
                    surface_i,
                    swap_d,
                    surface,
                    swapchain,
                } => {
                    swap_d.destroy_swapchain(*swapchain, None);
                    surface_i.destroy_surface(*surface, None);
                }
                Kind::Offscreen { memory, .. } => {
                    for image in self.images.drain(..) {
                        dev.device.destroy_image(image, None);
                    }
                    for mem in memory.drain(..) {
                        dev.device.free_memory(mem, None);
                    }
                }
            }
        }
    }
}

fn views(dev: &GpuDevice, images: &[vk::Image], format: vk::Format) -> Result<Vec<vk::ImageView>> {
    images
        .iter()
        .map(|&image| crate::util::view(dev, image, format))
        .collect()
}
