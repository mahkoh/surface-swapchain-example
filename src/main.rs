use {
    crate::protocols::wayland::{
        wl_compositor::WlCompositor, wl_display::WlDisplay, wl_registry::WlRegistry,
    },
    ash::{
        Entry,
        ext::swapchain_colorspace,
        khr::{surface, swapchain, wayland_surface},
        vk::{
            API_VERSION_1_3, ApplicationInfo, ColorSpaceKHR, CommandBufferAllocateInfo,
            CommandBufferBeginInfo, CommandPoolCreateInfo, CompositeAlphaFlagsKHR, DependencyFlags,
            DeviceCreateInfo, DeviceQueueCreateInfo, Extent2D, Fence, FenceCreateInfo, Format,
            ImageAspectFlags, ImageLayout, ImageMemoryBarrier, ImageSubresourceRange,
            ImageUsageFlags, InstanceCreateInfo, PipelineStageFlags, PresentInfoKHR,
            PresentModeKHR, Semaphore, SubmitInfo, SurfaceKHR, SurfaceTransformFlagsKHR,
            SwapchainCreateInfoKHR, SwapchainKHR, WaylandSurfaceCreateInfoKHR,
        },
    },
    std::{array, cell::Cell, slice},
    wl_client::{
        Libwayland, Queue,
        proxy::{self, OwnedProxy},
    },
};

mod protocols {
    include!(concat!(env!("OUT_DIR"), "/wayland-protocols/mod.rs"));
}

fn main() {
    let lib = Libwayland::open().unwrap();
    let con = lib.connect_to_default_display().unwrap();
    let queue = con.create_local_queue(c"registry");
    let compositor = get_compositor(&queue);
    let surface = compositor.create_surface();

    let entry = Entry::linked();
    let instance = {
        let app_info = ApplicationInfo::default()
            .api_version(API_VERSION_1_3)
            .application_name(c"test");
        let ext = [
            surface::NAME.as_ptr(),
            wayland_surface::NAME.as_ptr(),
            swapchain_colorspace::NAME.as_ptr(),
        ];
        let create_info = InstanceCreateInfo::default()
            .enabled_extension_names(&ext)
            .application_info(&app_info);
        unsafe { entry.create_instance(&create_info, None).unwrap() }
    };
    let wayland_surface = wayland_surface::Instance::new(&entry, &instance);
    let vk_surfaces = array::from_fn::<_, 2, _>(|_| {
        let create_info = WaylandSurfaceCreateInfoKHR::default()
            .display(con.wl_display().as_ptr().cast())
            .surface(proxy::wl_proxy(&*surface).unwrap().as_ptr().cast());
        unsafe {
            wayland_surface
                .create_wayland_surface(&create_info, None)
                .unwrap()
        }
    });
    let phy = unsafe { instance.enumerate_physical_devices().unwrap()[0] };
    let dev = {
        let queue_create_info = DeviceQueueCreateInfo::default()
            .queue_family_index(0)
            .queue_priorities(&[1.0]);
        let ext = [swapchain::NAME.as_ptr()];
        let create_info = DeviceCreateInfo::default()
            .queue_create_infos(slice::from_ref(&queue_create_info))
            .enabled_extension_names(&ext);
        unsafe { instance.create_device(phy, &create_info, None).unwrap() }
    };
    let vk_queue = unsafe { dev.get_device_queue(0, 0) };
    let swapchain_dev = swapchain::Device::new(&instance, &dev);
    let create_swapchain = |surface: SurfaceKHR, old: Option<SwapchainKHR>| {
        let create_info = SwapchainCreateInfoKHR::default()
            .surface(surface)
            .old_swapchain(old.unwrap_or_default())
            .min_image_count(3)
            .image_format(Format::R8G8B8A8_UNORM)
            .image_color_space(ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT)
            .image_extent(Extent2D {
                width: 100,
                height: 100,
            })
            .image_array_layers(1)
            .image_usage(ImageUsageFlags::COLOR_ATTACHMENT)
            .queue_family_indices(&[0])
            .pre_transform(SurfaceTransformFlagsKHR::IDENTITY)
            .composite_alpha(CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(PresentModeKHR::MAILBOX);
        unsafe { swapchain_dev.create_swapchain(&create_info, None).unwrap() }
    };
    let mut command_buffers = {
        let create_info = CommandPoolCreateInfo::default().queue_family_index(0);
        let pool = unsafe { dev.create_command_pool(&create_info, None).unwrap() };
        let allocate_info = CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .command_buffer_count(2);
        unsafe { dev.allocate_command_buffers(&allocate_info).unwrap() }
    };
    let mut present = |sc: SwapchainKHR| {
        let imgs = unsafe { swapchain_dev.get_swapchain_images(sc).unwrap() };
        let fence = unsafe { dev.create_fence(&FenceCreateInfo::default(), None).unwrap() };
        let (idx, _) = unsafe {
            swapchain_dev
                .acquire_next_image(sc, 0, Semaphore::null(), fence)
                .unwrap()
        };
        unsafe { dev.wait_for_fences(&[fence], true, u64::MAX).unwrap() }
        let img = imgs[idx as usize];
        let cmd = command_buffers.pop().unwrap();
        unsafe {
            let begin_info = CommandBufferBeginInfo::default();
            dev.begin_command_buffer(cmd, &begin_info).unwrap()
        }
        unsafe {
            let barrier = ImageMemoryBarrier::default()
                .image(img)
                .subresource_range(ImageSubresourceRange {
                    aspect_mask: ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .new_layout(ImageLayout::PRESENT_SRC_KHR);
            dev.cmd_pipeline_barrier(
                cmd,
                PipelineStageFlags::BOTTOM_OF_PIPE,
                PipelineStageFlags::TOP_OF_PIPE,
                DependencyFlags::empty(),
                &[],
                &[],
                slice::from_ref(&barrier),
            );
        }
        unsafe {
            dev.end_command_buffer(cmd).unwrap();
        }
        unsafe {
            let submit_info = SubmitInfo::default().command_buffers(slice::from_ref(&cmd));
            dev.queue_submit(vk_queue, &[submit_info], Fence::null())
                .unwrap();
        }
        let present_info = PresentInfoKHR::default()
            .image_indices(slice::from_ref(&idx))
            .swapchains(slice::from_ref(&sc));
        unsafe {
            swapchain_dev
                .queue_present(vk_queue, &present_info)
                .unwrap();
        }
    };

    let swapchain1 = create_swapchain(vk_surfaces[0], None);
    present(swapchain1);
    let swapchain2 = create_swapchain(vk_surfaces[1], Some(swapchain1));
    present(swapchain2);

    queue.dispatch_roundtrip_blocking().unwrap();
}

fn get_compositor(queue: &Queue) -> WlCompositor {
    let compositor = Cell::new(None::<WlCompositor>);
    let reg = queue.display::<WlDisplay>().get_registry();
    queue.dispatch_scope_blocking(|scope| {
        scope.set_event_handler_local(
            &reg,
            WlRegistry::on_global(|_, name, interface, _version| {
                if interface == WlCompositor::INTERFACE {
                    compositor.set(Some(reg.bind(name, 1)));
                }
            }),
        );
        queue.dispatch_roundtrip_blocking().unwrap();
    });
    compositor.take().unwrap()
}
