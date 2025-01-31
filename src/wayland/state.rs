use super::DisplayWrapper;
use crate::wayland::{drm::wl_drm::WlDrm, seat::SeatData};
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use smithay::{
	backend::{
		allocator::{dmabuf::Dmabuf, Fourcc},
		egl::EGLDevice,
		renderer::gles::GlesRenderer,
	},
	delegate_dmabuf, delegate_output, delegate_shm,
	output::{Mode, Output, Scale, Subpixel},
	reexports::{
		wayland_protocols::xdg::{
			decoration::zv1::server::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
			shell::server::xdg_wm_base::XdgWmBase,
		},
		wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration_manager::Mode as DecorationMode,
		wayland_server::{
			backend::{ClientData, ClientId, DisconnectReason},
			protocol::{wl_buffer::WlBuffer, wl_data_device_manager::WlDataDeviceManager},
			DisplayHandle,
		},
	},
	utils::{Size, Transform},
	wayland::{
		buffer::BufferHandler,
		compositor::{CompositorClientState, CompositorState},
		dmabuf::{
			self, DmabufFeedback, DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState,
		},
		shell::kde::decoration::KdeDecorationState,
		shm::{ShmHandler, ShmState},
	},
};
use std::sync::{Arc, Weak};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{info, warn};

pub struct ClientState {
	pub id: OnceCell<ClientId>,
	pub compositor_state: CompositorClientState,
	pub display: Weak<DisplayWrapper>,
	pub seat: Arc<SeatData>,
}
impl ClientState {
	pub fn flush(&self) {
		let Some(display) = self.display.upgrade() else {
			return;
		};
		let _ = display.flush_clients(self.id.get().cloned());
	}
}
impl ClientData for ClientState {
	fn initialized(&self, client_id: ClientId) {
		info!("Wayland client {:?} connected", client_id);
		let _ = self.id.set(client_id);
	}

	fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
		info!(
			"Wayland client {:?} disconnected because {:#?}",
			client_id, reason
		);
	}
}

pub struct WaylandState {
	pub weak_ref: Weak<Mutex<WaylandState>>,
	pub display_handle: DisplayHandle,

	pub compositor_state: CompositorState,
	// pub xdg_activation_state: XdgActivationState,
	pub kde_decoration_state: KdeDecorationState,
	pub shm_state: ShmState,
	dmabuf_state: (DmabufState, DmabufGlobal, Option<DmabufFeedback>),
	pub drm_formats: Vec<Fourcc>,
	pub dmabuf_tx: UnboundedSender<(Dmabuf, Option<dmabuf::ImportNotifier>)>,
	pub output: Output,
}

impl WaylandState {
	pub fn new(
		display_handle: DisplayHandle,
		renderer: &GlesRenderer,
		dmabuf_tx: UnboundedSender<(Dmabuf, Option<dmabuf::ImportNotifier>)>,
	) -> Arc<Mutex<Self>> {
		let compositor_state = CompositorState::new::<Self>(&display_handle);
		// let xdg_activation_state = XdgActivationState::new::<Self, _>(&display_handle);
		let kde_decoration_state =
			KdeDecorationState::new::<Self>(&display_handle, DecorationMode::Server);
		let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
		let render_node = EGLDevice::device_for_display(renderer.egl_context().display())
			.and_then(|device| device.try_get_render_node());
		let dmabuf_formats = renderer
			.egl_context()
			.dmabuf_render_formats()
			.iter()
			.cloned()
			.collect::<Vec<_>>();
		let drm_formats = dmabuf_formats.iter().map(|f| f.code).collect();

		let dmabuf_default_feedback = match render_node {
			Ok(Some(node)) => DmabufFeedbackBuilder::new(node.dev_id(), dmabuf_formats.clone())
				.build()
				.ok(),
			Ok(None) => {
				warn!("failed to query render node, dmabuf will use v3");
				None
			}
			Err(err) => {
				warn!(?err, "failed to egl device for display, dmabuf will use v3");
				None
			}
		};
		// if we failed to build dmabuf feedback we fall back to dmabuf v3
		// Note: egl on Mesa requires either v4 or wl_drm (initialized with bind_wl_display)
		let dmabuf_state = if let Some(default_feedback) = dmabuf_default_feedback {
			let mut dmabuf_state = DmabufState::new();
			let dmabuf_global = dmabuf_state.create_global_with_default_feedback::<WaylandState>(
				&display_handle,
				&default_feedback,
			);
			(dmabuf_state, dmabuf_global, Some(default_feedback))
		} else {
			let mut dmabuf_state = DmabufState::new();
			let dmabuf_global =
				dmabuf_state.create_global::<WaylandState>(&display_handle, dmabuf_formats.clone());
			(dmabuf_state, dmabuf_global, None)
		};

		let output = Output::new(
			"1x".to_owned(),
			smithay::output::PhysicalProperties {
				size: Size::default(),
				subpixel: Subpixel::None,
				make: "Virtual XR Display".to_owned(),
				model: "Your Headset Name Here".to_owned(),
			},
		);
		let _output_global = output.create_global::<Self>(&display_handle);
		let mode = Mode {
			size: (2048, 2048).into(),
			refresh: 60000,
		};
		output.change_current_state(
			Some(mode),
			Some(Transform::Normal),
			Some(Scale::Integer(2)),
			None,
		);
		output.set_preferred(mode);
		display_handle.create_global::<Self, WlDataDeviceManager, _>(3, ());
		display_handle.create_global::<Self, XdgWmBase, _>(5, ());
		display_handle.create_global::<Self, ZxdgDecorationManagerV1, _>(1, ());
		display_handle.create_global::<Self, WlDrm, _>(2, ());

		info!("Init Wayland compositor");

		Arc::new_cyclic(|weak| {
			Mutex::new(WaylandState {
				weak_ref: weak.clone(),
				display_handle,

				compositor_state,
				// xdg_activation_state,
				kde_decoration_state,
				shm_state,
				drm_formats,
				dmabuf_state,
				dmabuf_tx,
				output,
			})
		})
	}
}
impl Drop for WaylandState {
	fn drop(&mut self) {
		info!("Cleanly shut down the Wayland compositor");
	}
}
impl BufferHandler for WaylandState {
	fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}
impl ShmHandler for WaylandState {
	fn shm_state(&self) -> &ShmState {
		&self.shm_state
	}
}
impl DmabufHandler for WaylandState {
	fn dmabuf_state(&mut self) -> &mut DmabufState {
		&mut self.dmabuf_state.0
	}

	fn dmabuf_imported(
		&mut self,
		_global: &DmabufGlobal,
		dmabuf: Dmabuf,
		notifier: dmabuf::ImportNotifier,
	) {
		self.dmabuf_tx.send((dmabuf, Some(notifier))).unwrap();
	}
}
delegate_dmabuf!(WaylandState);
delegate_shm!(WaylandState);
delegate_output!(WaylandState);
