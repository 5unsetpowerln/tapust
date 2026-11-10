//! Window and OpenGL context creation (winit and glutin on Wayland with EGL).

use std::error::Error;
use std::num::NonZeroU32;

use glutin::config::{Config, ConfigTemplateBuilder, GlConfig};
use glutin::context::{ContextApi, ContextAttributesBuilder, GlProfile, PossiblyCurrentContext, Version};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::prelude::{GlSurface, NotCurrentGlContext};
use glutin::surface::{Surface, SwapInterval, WindowSurface};
use glutin_winit::{DisplayBuilder, GlWindow as _};
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::raw_window_handle::HasWindowHandle;
use winit::window::Window;

use crate::font::Fonts;
use crate::frame::Frame;
use mux::Metrics;
use crate::renderer::Renderer;
use crate::theme;

pub struct GlWindow {
    pub window: Window,
    surface: Surface<WindowSurface>,
    context: PossiblyCurrentContext,
    pub renderer: Renderer,
}

fn pick_config(configs: Box<dyn Iterator<Item = Config> + '_>) -> Config {
    // The terminal never needs multisampling or a transparent surface.
    configs
        .min_by_key(|c| (c.num_samples(), c.supports_transparency().unwrap_or(false)))
        .expect("no GL config")
}

impl GlWindow {
    pub fn new(event_loop: &ActiveEventLoop) -> Result<GlWindow, Box<dyn Error>> {
        let attributes = Window::default_attributes()
            .with_title("tapust")
            .with_name("tapust", "tapust")
            .with_inner_size(LogicalSize::new(800.0, 500.0));
        let template = ConfigTemplateBuilder::new().with_depth_size(0).with_stencil_size(0);
        let (window, config) = DisplayBuilder::new()
            .with_window_attributes(Some(attributes))
            .build(event_loop, template, pick_config)?;
        let window = window.ok_or("no window")?;
        let display = config.display();
        let context_attributes = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 3))))
            .with_profile(GlProfile::Core)
            .build(Some(window.window_handle()?.as_raw()));
        let context = unsafe { display.create_context(&config, &context_attributes)? };
        let surface_attributes = window.build_surface_attributes(Default::default())?;
        let surface = unsafe { display.create_window_surface(&config, &surface_attributes)? };
        let context = context.make_current(&surface)?;
        // Frames are paced by Wayland frame callbacks, not by blocking in swap_buffers.
        let _ = surface.set_swap_interval(&context, SwapInterval::DontWait);
        let gl = unsafe { glow::Context::from_loader_function_cstr(|name| display.get_proc_address(name)) };
        let fonts = Fonts::new(
            theme::FONT_FAMILY,
            theme::FONT_POINTS,
            theme::UI_FONT_FAMILY,
            theme::UI_FONT_POINTS,
            window.scale_factor(),
        )?;
        Ok(GlWindow { window, surface, context, renderer: Renderer::new(gl, fonts) })
    }

    /// Resizes the surface and returns the geometry of the pane area below
    /// the `top` pixels reserved for the tab bar.
    pub fn resize(&mut self, size: PhysicalSize<u32>, top: u32) -> Metrics {
        let (w, h) = (size.width.max(1), size.height.max(1));
        self.surface.resize(&self.context, NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap());
        self.renderer.resize(w, h);
        let (cell_width, cell_height) = self.renderer.cell_size();
        let scale = self.window.scale_factor();
        // ghostty converts the padding from points at 96 dpi and floors it.
        let padding = (theme::PADDING_POINTS * 96.0 / 72.0 * scale).floor() as u32;
        Metrics {
            width: w,
            height: h.saturating_sub(top),
            cell_width,
            cell_height,
            padding_x: padding,
            padding_y: padding,
            // libadwaita's one logical pixel separator.
            divider: (scale.round() as u32).max(1),
        }
    }

    /// The tab bar height in pixels for this window's scale.
    pub fn tab_bar_height(&self) -> u32 {
        (theme::TAB_BAR_HEIGHT * self.window.scale_factor()).round() as u32
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        self.window.pre_present_notify();
        self.renderer.draw(frame);
        if let Err(err) = self.surface.swap_buffers(&self.context) {
            eprintln!("tapust: swap_buffers failed: {err}");
        }
    }
}
