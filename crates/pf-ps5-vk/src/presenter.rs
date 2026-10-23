//! The presenter: the [`Display`] seam on the one device. Each frame clears to black, draws the
//! newest stream picture into its fitted rectangle (when the stream layer is on), blends the UI
//! layer over it, and presents at the next vblank.
//!
//! Pictures arrive from the decode lanes through a [`VideoSlot`]: newest wins, and the last one
//! shown is drawn again until a newer one arrives. A picture stays alive until the frame that
//! sampled it has finished on the GPU.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use ash::vk;
use pf_client_video::video_color::csc_rows;
use pf_ps5_app::platform::{Display, Fit, FrameReport, Refresh, Rgba};

use crate::device::GpuDevice;
use crate::output::Output;
use crate::picture::{Picture, Planes, VideoSlot};
use crate::pipelines::Pipelines;
use crate::ui::{UiLayer, FRAMES};

/// Tonemap source peak for a PQ stream on this SDR output, in 203-nit units (~1000 nits).
const TONEMAP_PEAK: f32 = 4.9;

struct FrameRes {
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    /// Signalled by the acquire; waited by this frame's submit.
    ready: vk::Semaphore,
    /// planar, two-plane, overlay.
    sets: [vk::DescriptorSet; 3],
    /// The picture this frame sampled, released once its fence signals.
    in_use: Option<Arc<Picture>>,
}

/// Read-back: an image the composition is drawn into again, and a buffer it is copied to.
struct Readback {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    buffer: vk::Buffer,
    buffer_memory: vk::DeviceMemory,
    ptr: *mut u8,
}

pub struct Presenter {
    dev: Arc<GpuDevice>,
    output: Output,
    pipes: Pipelines,
    framebuffers: Vec<vk::Framebuffer>,
    /// Per output image: signalled when its frame is drawn, waited by the present.
    done: Vec<vk::Semaphore>,
    pool: vk::CommandPool,
    desc_pool: vk::DescriptorPool,
    frames: Vec<FrameRes>,
    frame: usize,
    ui: UiLayer,
    slot: Arc<VideoSlot>,
    current: Option<Arc<Picture>>,
    stream: Option<Fit>,
    last_present: Option<Instant>,
    readback: Option<Readback>,
    readback_sets: [vk::DescriptorSet; 3],
    /// The display refused an HDR10 output once; PQ is tonemapped from then on.
    hdr_refused: bool,
}

// SAFETY: the read-back pointer maps memory this struct owns; the presenter lives on the main
// thread and is moved there once, never shared.
unsafe impl Send for Presenter {}

impl Presenter {
    /// Builds the output, pipelines and UI layer on `dev`. `slot` is shared with the lanes.
    pub fn new(dev: Arc<GpuDevice>, slot: Arc<VideoSlot>) -> Result<Self> {
        let output = Output::new(&dev, false)?;
        let pipes = Pipelines::new(&dev, output.format, output.hdr, output.final_layout)?;
        let framebuffers = output
            .views
            .iter()
            .map(|&view| framebuffer(&dev, pipes.pass_out, view, output.extent))
            .collect::<Result<Vec<_>>>()?;
        let done = (0..output.images.len())
            .map(|_| semaphore(&dev))
            .collect::<Result<Vec<_>>>()?;
        // SAFETY: builders are locals; the pool is owned here and destroyed in `Drop`.
        let pool = unsafe {
            dev.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(dev.qf)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        }?;
        let sets_needed = (FRAMES as u32 + 1) * 3;
        let sizes = [vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(sets_needed * 3)];
        // SAFETY: as above.
        let desc_pool = unsafe {
            dev.device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(sets_needed)
                    .pool_sizes(&sizes),
                None,
            )
        }?;
        // SAFETY: the pool is live; the buffers are freed with it.
        let cmds = unsafe {
            dev.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(FRAMES as u32),
            )
        }?;
        let mut frames = Vec::with_capacity(FRAMES);
        for cmd in cmds {
            frames.push(FrameRes {
                cmd,
                // SAFETY: signalled, so the first wait passes; destroyed in `Drop`.
                fence: unsafe {
                    dev.device.create_fence(
                        &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                        None,
                    )
                }?,
                ready: semaphore(&dev)?,
                sets: alloc_sets(&dev, desc_pool, &pipes)?,
                in_use: None,
            });
        }
        let readback_sets = alloc_sets(&dev, desc_pool, &pipes)?;
        let ui = UiLayer::new(&dev, output.extent)?;
        // The presenter runs on the main thread, decode on the pump: take present turns so a
        // picture handed over is presented ahead of the next decode on the shared queue.
        dev.vkd.queue_lock.take_turns();
        tracing::info!("presenter: {}", dev.feature_line());
        Ok(Self {
            dev,
            output,
            pipes,
            framebuffers,
            done,
            pool,
            desc_pool,
            frames,
            frame: 0,
            ui,
            slot,
            current: None,
            stream: None,
            last_present: None,
            readback: None,
            readback_sets,
            hdr_refused: false,
        })
    }

    /// Swaps the output for an HDR10 (`hdr`) or SDR one, and everything drawn against its format.
    /// An HDR10 output the display refuses falls back to SDR and isn't asked for again.
    fn rebuild(&mut self, hdr: bool) -> Result<()> {
        self.wait_idle();
        let dev = self.dev.clone();
        // SAFETY: the device is idle; every handle below was created by this presenter and is
        // replaced before it is used again.
        unsafe {
            if let Some(rb) = self.readback.take() {
                destroy_readback(&dev, &rb);
            }
            for fb in self.framebuffers.drain(..) {
                dev.device.destroy_framebuffer(fb, None);
            }
            self.pipes.destroy(&dev);
            self.output.destroy(&dev);
        }
        self.output = match Output::new(&dev, hdr) {
            Ok(output) => output,
            Err(e) if hdr => {
                tracing::warn!("presenter: no HDR10 output ({e:#}); tonemapping PQ to SDR");
                self.hdr_refused = true;
                Output::new(&dev, false)?
            }
            Err(e) => return Err(e),
        };
        self.pipes = Pipelines::new(&dev, self.output.format, self.output.hdr, self.output.final_layout)?;
        self.framebuffers = self
            .output
            .views
            .iter()
            .map(|&view| framebuffer(&dev, self.pipes.pass_out, view, self.output.extent))
            .collect::<Result<Vec<_>>>()?;
        while self.done.len() < self.output.images.len() {
            self.done.push(semaphore(&dev)?);
        }
        // SAFETY: the device is idle, so no set from this pool is in use.
        unsafe {
            dev.device
                .reset_descriptor_pool(self.desc_pool, vk::DescriptorPoolResetFlags::empty())
        }?;
        for f in 0..self.frames.len() {
            self.frames[f].sets = alloc_sets(&dev, self.desc_pool, &self.pipes)?;
        }
        self.readback_sets = alloc_sets(&dev, self.desc_pool, &self.pipes)?;
        Ok(())
    }

    pub fn device(&self) -> &Arc<GpuDevice> {
        &self.dev
    }

    /// How the UI layer draws ([`UiLayer::describe`]).
    pub fn ui(&self) -> String {
        self.ui.describe()
    }

    /// The viewport a `w`×`h` picture takes in `extent` under `fit`.
    fn fitted(fit: Fit, w: u32, h: u32, extent: vk::Extent2D) -> vk::Viewport {
        let (ew, eh) = (extent.width as f32, extent.height as f32);
        let (pw, ph) = (w.max(1) as f32, h.max(1) as f32);
        let (vw, vh) = match fit {
            Fit::Stretch => (ew, eh),
            Fit::Contain => {
                let s = (ew / pw).min(eh / ph);
                (pw * s, ph * s)
            }
            Fit::Cover => {
                let s = (ew / pw).max(eh / ph);
                (pw * s, ph * s)
            }
        };
        vk::Viewport {
            x: (ew - vw) / 2.0,
            y: (eh - vh) / 2.0,
            width: vw,
            height: vh,
            min_depth: 0.0,
            max_depth: 1.0,
        }
    }

    /// Records the composition into `fb`: clear, picture, UI layer.
    ///
    /// # Safety
    /// `cmd` is recording, `fb` belongs to `pass`, and `sets` are not read by work in flight.
    unsafe fn compose(
        &self,
        cmd: vk::CommandBuffer,
        pass: vk::RenderPass,
        fb: vk::Framebuffer,
        sets: [vk::DescriptorSet; 3],
    ) {
        let extent = self.output.extent;
        let full = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent,
        };
        let clear = [vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.0, 0.0, 0.0, 1.0],
            },
        }];
        let dev = &self.dev.device;
        // SAFETY: forwarded from the caller; every handle is owned by this presenter or by the
        // picture it holds for the frame.
        unsafe {
            dev.cmd_begin_render_pass(
                cmd,
                &vk::RenderPassBeginInfo::default()
                    .render_pass(pass)
                    .framebuffer(fb)
                    .render_area(full)
                    .clear_values(&clear),
                vk::SubpassContents::INLINE,
            );
            dev.cmd_set_scissor(cmd, 0, &[full]);
            if let (Some(fit), Some(picture)) = (self.stream, self.current.as_ref()) {
                let (pipe, set, views): (_, _, &[vk::ImageView]) = match &picture.planes {
                    Planes::Three(v) => (&self.pipes.planar, sets[0], v),
                    Planes::Two(v) => (&self.pipes.two_plane, sets[1], v),
                };
                self.write_set(set, views, picture.layout);
                let mut rows = csc_rows(picture.color, picture.depth, picture.msb_packed);
                for row in &mut rows {
                    for c in &mut row[..3] {
                        *c *= picture.sample_scale;
                    }
                }
                let mut pc = [0f32; 16];
                pc[..12].copy_from_slice(rows.as_flattened());
                pc[12] = if picture.color.is_pq() && !self.output.hdr { 1.0 } else { 0.0 };
                pc[13] = TONEMAP_PEAK;
                pc[14] = picture.uv_scale[0];
                pc[15] = picture.uv_scale[1];
                let words = pc.map(f32::to_ne_bytes);
                dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe.pipeline);
                dev.cmd_set_viewport(cmd, 0, &[Self::fitted(fit, picture.width, picture.height, extent)]);
                dev.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::GRAPHICS, pipe.layout, 0, &[set], &[]);
                dev.cmd_push_constants(
                    cmd,
                    pipe.layout,
                    vk::ShaderStageFlags::FRAGMENT,
                    0,
                    words.as_flattened(),
                );
                dev.cmd_draw(cmd, 3, 1, 0, 0);
            }
            self.write_set(sets[2], &[self.ui.view()], vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
            dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipes.overlay.pipeline);
            dev.cmd_set_viewport(
                cmd,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: extent.width as f32,
                    height: extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            dev.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipes.overlay.layout,
                0,
                &[sets[2]],
                &[],
            );
            dev.cmd_draw(cmd, 3, 1, 0, 0);
            dev.cmd_end_render_pass(cmd);
        }
    }

    fn write_set(&self, set: vk::DescriptorSet, views: &[vk::ImageView], layout: vk::ImageLayout) {
        let infos: Vec<vk::DescriptorImageInfo> = views
            .iter()
            .map(|&v| {
                vk::DescriptorImageInfo::default()
                    .sampler(self.pipes.sampler)
                    .image_view(v)
                    .image_layout(layout)
            })
            .collect();
        let writes: Vec<vk::WriteDescriptorSet> = infos
            .iter()
            .enumerate()
            .map(|(i, info)| {
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(i as u32)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(std::slice::from_ref(info))
            })
            .collect();
        // SAFETY: `writes` borrows `infos`, both live for the call; the set is not in use by
        // work in flight (its frame's fence was waited, or the device idled for read-back).
        unsafe { self.dev.device.update_descriptor_sets(&writes, &[]) };
    }

    fn ensure_readback(&mut self) -> Result<()> {
        if self.readback.is_some() {
            return Ok(());
        }
        let extent = self.output.extent;
        let format = self.output.format;
        let (image, memory) = crate::util::image(
            &self.dev,
            extent,
            format,
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )?;
        let view = crate::util::view(&self.dev, image, format)?;
        let framebuffer = framebuffer(&self.dev, self.pipes.pass_read, view, extent)?;
        let bytes = u64::from(extent.width) * u64::from(extent.height) * 4;
        let (buffer, buffer_memory, ptr) =
            crate::util::host_buffer(&self.dev, bytes, vk::BufferUsageFlags::TRANSFER_DST)?;
        self.readback = Some(Readback {
            image,
            memory,
            view,
            framebuffer,
            buffer,
            buffer_memory,
            ptr,
        });
        Ok(())
    }

    fn wait_idle(&self) {
        let _q = self.dev.vkd.queue_lock.guard();
        // SAFETY: idling a live device under the queue lock.
        let _ = unsafe { self.dev.device.device_wait_idle() };
    }
}

impl Display for Presenter {
    fn size(&self) -> (u32, u32) {
        (self.output.extent.width, self.output.extent.height)
    }

    fn ui_size(&self) -> (u32, u32) {
        let e = self.ui.extent();
        (e.width, e.height)
    }

    fn refresh(&self) -> Refresh {
        Refresh(self.output.refresh_mhz)
    }

    fn frame(&mut self, ui: &mut dyn FnMut(&skia_safe::Canvas)) -> Result<FrameReport> {
        let f = self.frame;
        let (cmd, fence, ready, sets) = {
            let fr = &self.frames[f];
            (fr.cmd, fr.fence, fr.ready, fr.sets)
        };
        // SAFETY: the fence belongs to this presenter; waiting only reads.
        unsafe { self.dev.device.wait_for_fences(&[fence], true, u64::MAX) }
            .map_err(|e| anyhow!("wait_for_fences: {e}"))?;
        self.frames[f].in_use = None;

        self.ui.draw(ui);
        let mut report = FrameReport::default();
        if self.stream.is_some() {
            if let Some(picture) = self.slot.take() {
                if let Some(turn) = picture.turn {
                    self.dev.vkd.queue_lock.begin_present_turn(turn);
                }
                report.new_picture = true;
                report.decode_to_present_us = u32::try_from(picture.decoded_at.elapsed().as_micros()).ok();
                self.current = Some(picture);
            }
        }
        let want_hdr = !self.hdr_refused
            && self.stream.is_some()
            && self.current.as_ref().is_some_and(|p| p.color.is_pq());
        if want_hdr != self.output.hdr && self.output.presents() {
            self.rebuild(want_hdr)?;
        }
        self.frames[f].in_use = self.current.clone();

        let index = self.output.acquire(ready)?;
        // SAFETY: the command buffer is not in flight (fence waited) and its pool allows reset;
        // every recorded handle is owned here or by the held picture.
        unsafe {
            self.dev
                .device
                .reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())?;
            self.dev.device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            self.ui.finish(&self.dev, cmd, f);
            self.compose(cmd, self.pipes.pass_out, self.framebuffers[index as usize], sets);
            self.dev.device.end_command_buffer(cmd)?;
        }
        let presents = self.output.presents();
        let waits = [ready];
        let stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let signals = [self.done[index as usize]];
        let cmds = [cmd];
        let mut submit = vk::SubmitInfo::default().command_buffers(&cmds);
        if presents {
            submit = submit
                .wait_semaphores(&waits)
                .wait_dst_stage_mask(&stages)
                .signal_semaphores(&signals);
        }
        {
            let _q = self.dev.vkd.queue_lock.guard();
            // SAFETY: the fence is signalled (waited above) and reset here; the submit's borrows
            // outlive the call; the queue is held under the shared lock.
            unsafe {
                self.dev.device.reset_fences(&[fence])?;
                self.dev.device.queue_submit(self.dev.queue, &[submit], fence)?;
            }
            self.output.present(self.dev.queue, index, signals[0])?;
        }
        if let Some(turn) = self.current.as_ref().and_then(|p| p.turn) {
            self.dev.vkd.queue_lock.end_present_turn(turn);
        }
        if !presents {
            // Nothing paces an offscreen target: hold the loop to the nominal refresh.
            let period = Duration::from_nanos(self.refresh().period_ns());
            if let Some(last) = self.last_present {
                if let Some(rest) = period.checked_sub(last.elapsed()) {
                    std::thread::sleep(rest);
                }
            }
        }
        let now = Instant::now();
        let period = Duration::from_nanos(self.refresh().period_ns());
        report.missed = self.last_present.is_some_and(|t| now - t > period + period / 2);
        self.last_present = Some(now);
        if report.new_picture {
            if let Some(p) = &self.current {
                note_displayed(p);
            }
        }
        self.frame = (f + 1) % FRAMES;
        Ok(report)
    }

    fn set_stream(&mut self, on: bool, fit: Fit) {
        self.stream = on.then_some(fit);
        if !on {
            self.slot.clear();
            self.current = None;
        }
    }

    fn read_back(&mut self) -> Result<Option<Rgba>> {
        // The capture is 8-bit sRGB; an HDR10 composition has no such pixels to give.
        if self.output.hdr {
            return Ok(None);
        }
        self.ensure_readback()?;
        self.wait_idle();
        let rb = self.readback.as_ref().expect("just ensured");
        let extent = self.output.extent;
        let (framebuffer, image, buffer, ptr) = (rb.framebuffer, rb.image, rb.buffer, rb.ptr);
        let cmd = self.frames[self.frame].cmd;
        // SAFETY: the device is idle, so the command buffer and the read-back set are free; the
        // fence below is local and destroyed after its wait.
        unsafe {
            self.dev
                .device
                .reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())?;
            self.dev.device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            self.compose(cmd, self.pipes.pass_read, framebuffer, self.readback_sets);
            let region = [vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    layer_count: 1,
                    ..Default::default()
                })
                .image_extent(vk::Extent3D {
                    width: extent.width,
                    height: extent.height,
                    depth: 1,
                })];
            self.dev.device.cmd_copy_image_to_buffer(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer,
                &region,
            );
            self.dev.device.end_command_buffer(cmd)?;
            let fence = self.dev.device.create_fence(&vk::FenceCreateInfo::default(), None)?;
            let cmds = [cmd];
            {
                let _q = self.dev.vkd.queue_lock.guard();
                self.dev.device.queue_submit(
                    self.dev.queue,
                    &[vk::SubmitInfo::default().command_buffers(&cmds)],
                    fence,
                )?;
            }
            let waited = self.dev.device.wait_for_fences(&[fence], true, 5_000_000_000);
            self.dev.device.destroy_fence(fence, None);
            waited.context("read-back fence")?;
        }
        let len = extent.width as usize * extent.height as usize * 4;
        let mut pixels = vec![0u8; len];
        // SAFETY: `ptr` maps `len` bytes of host-coherent memory the copy above finished writing.
        unsafe { std::ptr::copy_nonoverlapping(ptr, pixels.as_mut_ptr(), len) };
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        Ok(Some(Rgba {
            width: extent.width,
            height: extent.height,
            pixels,
        }))
    }
}

/// The overlay's display stage: decoded → handed to scan-out, on the session's HUD.
fn note_displayed(picture: &Picture) {
    if let Some(hud) = &picture.hud {
        let now = u64::try_from(punktfunk_core::client::now_realtime_ns()).unwrap_or(0);
        hud.note_displayed(picture.pts_ns, picture.decoded_ns, now, now);
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        self.wait_idle();
        for fr in &mut self.frames {
            fr.in_use = None;
        }
        self.current = None;
        let dev = &self.dev;
        // SAFETY: the device is idle and every handle below was created by this presenter.
        unsafe {
            if let Some(rb) = self.readback.take() {
                destroy_readback(dev, &rb);
            }
            self.ui.destroy(dev);
            for fr in self.frames.drain(..) {
                dev.device.destroy_fence(fr.fence, None);
                dev.device.destroy_semaphore(fr.ready, None);
            }
            for sem in self.done.drain(..) {
                dev.device.destroy_semaphore(sem, None);
            }
            for fb in self.framebuffers.drain(..) {
                dev.device.destroy_framebuffer(fb, None);
            }
            dev.device.destroy_descriptor_pool(self.desc_pool, None);
            dev.device.destroy_command_pool(self.pool, None);
            self.pipes.destroy(dev);
            self.output.destroy(dev);
        }
    }
}

/// # Safety
/// The device is idle and `rb` was built on it; nothing uses `rb` afterwards.
unsafe fn destroy_readback(dev: &GpuDevice, rb: &Readback) {
    // SAFETY: forwarded from the caller.
    unsafe {
        dev.device.destroy_framebuffer(rb.framebuffer, None);
        dev.device.destroy_image_view(rb.view, None);
        dev.device.destroy_image(rb.image, None);
        dev.device.free_memory(rb.memory, None);
        dev.device.destroy_buffer(rb.buffer, None);
        dev.device.free_memory(rb.buffer_memory, None);
    }
}

/// One planar, one two-plane and one overlay set from `pool`, freed with it.
fn alloc_sets(dev: &GpuDevice, pool: vk::DescriptorPool, pipes: &Pipelines) -> Result<[vk::DescriptorSet; 3]> {
    let layouts = [
        pipes.planar.set_layout,
        pipes.two_plane.set_layout,
        pipes.overlay.set_layout,
    ];
    // SAFETY: the pool and layouts are live; the sets are freed with the pool.
    let sets = unsafe {
        dev.device.allocate_descriptor_sets(
            &vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(pool)
                .set_layouts(&layouts),
        )
    }?;
    Ok([sets[0], sets[1], sets[2]])
}

fn semaphore(dev: &GpuDevice) -> Result<vk::Semaphore> {
    // SAFETY: the create-info is a local; the semaphore is destroyed by its owner.
    Ok(unsafe { dev.device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }?)
}

fn framebuffer(
    dev: &GpuDevice,
    pass: vk::RenderPass,
    view: vk::ImageView,
    extent: vk::Extent2D,
) -> Result<vk::Framebuffer> {
    let attachments = [view];
    // SAFETY: the builder and the view it borrows outlive the call; the owner destroys it.
    Ok(unsafe {
        dev.device.create_framebuffer(
            &vk::FramebufferCreateInfo::default()
                .render_pass(pass)
                .attachments(&attachments)
                .width(extent.width)
                .height(extent.height)
                .layers(1),
            None,
        )
    }?)
}
