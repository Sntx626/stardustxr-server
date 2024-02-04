pub mod alias;
pub mod audio;
pub mod data;
pub mod drawable;
pub mod fields;
pub mod hmd;
pub mod input;
pub mod items;
pub mod root;
pub mod spatial;

use color_eyre::eyre::{eyre, Result};
use nanoid::nanoid;
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use portable_atomic::{AtomicBool, Ordering};
use rustc_hash::FxHashMap;
use serde::{de::DeserializeOwned, Serialize};
use stardust_xr::messenger::MessageSenderHandle;
use stardust_xr::scenegraph::ScenegraphError;
use stardust_xr::schemas::flex::{deserialize, serialize};
use std::fmt::Debug;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Weak};
use std::vec::Vec;

use crate::core::client::Client;
use crate::core::registry::Registry;
use crate::core::scenegraph::MethodResponseSender;

use self::alias::Alias;
use self::audio::Sound;
use self::data::{PulseReceiver, PulseSender};
use self::drawable::Drawable;
use self::fields::Field;
use self::input::{InputHandler, InputMethod};
use self::items::{Item, ItemAcceptor, ItemUI};
use self::spatial::zone::Zone;
use self::spatial::Spatial;

#[derive(Default)]
pub struct Message {
	pub data: Vec<u8>,
	pub fds: Vec<OwnedFd>,
}
impl From<Vec<u8>> for Message {
	fn from(data: Vec<u8>) -> Self {
		Message {
			data,
			fds: Vec::new(),
		}
	}
}
impl AsRef<[u8]> for Message {
	fn as_ref(&self) -> &[u8] {
		&self.data
	}
}

pub type Signal = fn(Arc<Node>, Arc<Client>, Message) -> Result<()>;
pub type Method = fn(Arc<Node>, Arc<Client>, Message, MethodResponseSender);

stardust_xr_server_codegen::codegen_node_protocol!();

pub struct Node {
	pub enabled: Arc<AtomicBool>,
	pub(super) uid: String,
	path: String,
	client: Weak<Client>,
	message_sender_handle: Option<MessageSenderHandle>,
	// trailing_slash_pos: usize,
	local_signals: Mutex<FxHashMap<String, Signal>>,
	local_methods: Mutex<FxHashMap<String, Method>>,
	destroyable: bool,

	pub alias: OnceCell<Arc<Alias>>,
	aliases: Registry<Alias>,

	pub spatial: OnceCell<Arc<Spatial>>,
	pub field: OnceCell<Arc<Field>>,
	pub zone: OnceCell<Arc<Zone>>,

	// Data
	pub pulse_sender: OnceCell<Arc<PulseSender>>,
	pub pulse_receiver: OnceCell<Arc<PulseReceiver>>,

	// Drawable
	pub drawable: OnceCell<Drawable>,

	// Input
	pub input_method: OnceCell<Arc<InputMethod>>,
	pub input_handler: OnceCell<Arc<InputHandler>>,

	// Item
	pub item: OnceCell<Arc<Item>>,
	pub item_acceptor: OnceCell<Arc<ItemAcceptor>>,
	pub item_ui: OnceCell<Arc<ItemUI>>,

	// Sound
	pub sound: OnceCell<Arc<Sound>>,
}
impl Node {
	pub fn get_client(&self) -> Option<Arc<Client>> {
		self.client.upgrade()
	}
	// pub fn get_name(&self) -> &str {
	// 	&self.path[self.trailing_slash_pos + 1..]
	// }
	pub fn get_path(&self) -> &str {
		self.path.as_str()
	}

	pub fn create_parent_name(
		client: &Arc<Client>,
		parent: &str,
		name: &str,
		destroyable: bool,
	) -> Self {
		let mut path = parent.to_string();
		path.push('/');
		path.push_str(name);
		Self::create_path(client, path, destroyable)
	}
	pub fn create_path(client: &Arc<Client>, path: impl ToString, destroyable: bool) -> Self {
		let node = Node {
			enabled: Arc::new(AtomicBool::new(true)),
			uid: nanoid!(),
			client: Arc::downgrade(client),
			message_sender_handle: client.message_sender_handle.clone(),
			path: path.to_string(),
			// trailing_slash_pos: parent.len(),
			local_signals: Default::default(),
			local_methods: Default::default(),
			destroyable,

			alias: OnceCell::new(),
			aliases: Registry::new(),

			spatial: OnceCell::new(),
			field: OnceCell::new(),
			zone: OnceCell::new(),
			pulse_sender: OnceCell::new(),
			pulse_receiver: OnceCell::new(),
			drawable: OnceCell::new(),
			input_method: OnceCell::new(),
			input_handler: OnceCell::new(),
			item: OnceCell::new(),
			item_acceptor: OnceCell::new(),
			item_ui: OnceCell::new(),
			sound: OnceCell::new(),
		};
		<Node as NodeAspect>::add_node_members(&node);
		node
	}
	pub fn add_to_scenegraph(self) -> Result<Arc<Node>> {
		Ok(self
			.get_client()
			.ok_or_else(|| eyre!("Internal: Unable to get client"))?
			.scenegraph
			.add_node(self))
	}
	pub fn destroy(&self) {
		if let Some(client) = self.get_client() {
			client.scenegraph.remove_node(self.get_path());
		}
	}

	// very much up for debate if we should allow this, as you can match objects using this
	// pub fn get_client_pid_flex(
	// 	node: Arc<Node>,
	// 	_calling_client: Arc<Client>,
	// 	_message: Message,
	// ) -> Result<Message> {
	// 	let client = node
	// 		.client
	// 		.upgrade()
	// 		.ok_or_else(|| eyre!("Could not get client for node?"))?;
	// 	let pid = client.pid.ok_or_else(|| eyre!("Client PID is unknown"))?;
	// 	Ok(serialize(pid)?.into())
	// }

	pub fn add_local_signal(&self, name: &str, signal: Signal) {
		self.local_signals.lock().insert(name.to_string(), signal);
	}
	pub fn add_local_method(&self, name: &str, method: Method) {
		self.local_methods.lock().insert(name.to_string(), method);
	}

	pub fn get_aspect<F, T>(&self, node_name: &str, aspect_type: &str, aspect_fn: F) -> Result<&T>
	where
		F: FnOnce(&Node) -> &OnceCell<T>,
	{
		aspect_fn(self)
			.get()
			.ok_or_else(|| eyre!("{} is not a {} node", node_name, aspect_type))
	}

	pub fn send_local_signal(
		self: Arc<Self>,
		calling_client: Arc<Client>,
		method: &str,
		message: Message,
	) -> Result<(), ScenegraphError> {
		if let Some(alias) = self.alias.get() {
			if !alias.info.server_signals.iter().any(|e| e == &method) {
				return Err(ScenegraphError::SignalNotFound);
			}
			alias
				.original
				.upgrade()
				.ok_or(ScenegraphError::BrokenAlias)?
				.send_local_signal(calling_client, method, message)
		} else {
			let signal = self
				.local_signals
				.lock()
				.get(method)
				.cloned()
				.ok_or(ScenegraphError::SignalNotFound)?;
			signal(self, calling_client, message).map_err(|error| ScenegraphError::SignalError {
				error: error.to_string(),
			})
		}
	}
	pub fn execute_local_method(
		self: Arc<Self>,
		calling_client: Arc<Client>,
		method: &str,
		message: Message,
		response: MethodResponseSender,
	) {
		if let Some(alias) = self.alias.get() {
			if !alias.info.server_methods.iter().any(|e| e == &method) {
				response.send(Err(ScenegraphError::MethodNotFound));
				return;
			}
			let Some(alias) = alias.original.upgrade() else {
				response.send(Err(ScenegraphError::BrokenAlias));
				return;
			};
			alias.execute_local_method(
				calling_client,
				method,
				Message {
					data: message.data.clone(),
					fds: Vec::new(),
				},
				response,
			)
		} else {
			let Some(method) = self.local_methods.lock().get(method).cloned() else {
				response.send(Err(ScenegraphError::MethodNotFound));
				return;
			};
			method(self, calling_client, message, response);
		}
	}
	pub fn send_remote_signal(&self, method: &str, message: impl Into<Message>) -> Result<()> {
		let message = message.into();
		self.aliases
			.get_valid_contents()
			.iter()
			.filter(|alias| alias.info.client_signals.iter().any(|e| e == &method))
			.filter_map(|alias| alias.node.upgrade())
			.for_each(|node| {
				// Beware! file descriptors will not be sent to aliases!!!
				let _ = node.send_remote_signal(
					method,
					Message {
						data: message.data.clone(),
						fds: Vec::new(),
					},
				);
			});
		let path = self.path.clone();
		let method = method.to_string();
		if let Some(handle) = self.message_sender_handle.as_ref() {
			handle.signal(path.as_str(), method.as_str(), &message.data, message.fds)?;
		}
		Ok(())
	}
	pub async fn execute_remote_method_typed<S: Serialize, D: DeserializeOwned>(
		&self,
		method: &str,
		input: S,
		fds: Vec<OwnedFd>,
	) -> Result<(D, Vec<OwnedFd>)> {
		let message_sender_handle = self
			.message_sender_handle
			.as_ref()
			.ok_or(eyre!("Messenger does not exist for this node"))?;

		let serialized = serialize(input)?;
		let result = message_sender_handle
			.method(self.path.as_str(), method, &serialized, fds)?
			.await
			.map_err(|e| eyre!(e))?;

		let (message, fds) = result.into_components();
		let deserialized: D = deserialize(&message)?;
		Ok((deserialized, fds))
	}
}
impl Debug for Node {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Node")
			.field("uid", &self.uid)
			.field("path", &self.path)
			.finish()
	}
}
impl NodeAspect for Node {
	fn set_enabled(node: Arc<Node>, _calling_client: Arc<Client>, enabled: bool) -> Result<()> {
		node.enabled.store(enabled, Ordering::Relaxed);
		Ok(())
	}

	fn destroy(node: Arc<Node>, _calling_client: Arc<Client>) -> Result<()> {
		if node.destroyable {
			node.destroy();
		}
		Ok(())
	}
}
impl Drop for Node {
	fn drop(&mut self) {
		// Debug breakpoint
	}
}
