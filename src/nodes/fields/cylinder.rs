use super::{Field, FieldTrait, Node};
use crate::core::client::Client;
use crate::nodes::spatial::{find_spatial_parent, parse_transform, Spatial, Transform};
use crate::nodes::Message;
use color_eyre::eyre::{ensure, Result};
use glam::{swizzles::*, vec2, Vec3A};
use portable_atomic::AtomicF32;
use serde::Deserialize;
use stardust_xr::schemas::flex::deserialize;

use std::sync::atomic::Ordering;
use std::sync::Arc;

pub struct CylinderField {
	space: Arc<Spatial>,
	length: AtomicF32,
	radius: AtomicF32,
}

impl CylinderField {
	pub fn add_to(node: &Arc<Node>, length: f32, radius: f32) -> Result<()> {
		ensure!(
			node.spatial.get().is_some(),
			"Internal: Node does not have a spatial attached!"
		);
		ensure!(
			node.field.get().is_none(),
			"Internal: Node already has a field attached!"
		);
		let cylinder_field = CylinderField {
			space: node.spatial.get().unwrap().clone(),
			length: AtomicF32::new(length.abs()),
			radius: AtomicF32::new(radius.abs()),
		};
		cylinder_field.add_field_methods(node);
		node.add_local_signal("set_size", CylinderField::set_size_flex);
		let _ = node.field.set(Arc::new(Field::Cylinder(cylinder_field)));
		Ok(())
	}

	pub fn set_size(&self, length: f32, radius: f32) {
		self.length.store(length.abs(), Ordering::Relaxed);
		self.radius.store(radius.abs(), Ordering::Relaxed);
	}

	pub fn set_size_flex(
		node: Arc<Node>,
		_calling_client: Arc<Client>,
		message: Message,
	) -> Result<()> {
		let Field::Cylinder(cylinder_field) = node.field.get().unwrap().as_ref() else {
			return Ok(());
		};
		let (length, radius) = deserialize(message.as_ref())?;
		cylinder_field.set_size(length, radius);
		Ok(())
	}
}

impl FieldTrait for CylinderField {
	fn local_distance(&self, p: Vec3A) -> f32 {
		let radius = self.radius.load(Ordering::Relaxed);
		let length = self.length.load(Ordering::Relaxed);
		let d = vec2(p.xy().length().abs() - radius, p.z.abs() - (length * 0.5));

		d.x.max(d.y).min(0.0) + d.max(vec2(0.0, 0.0)).length()
	}
	fn spatial_ref(&self) -> &Spatial {
		self.space.as_ref()
	}
}

pub fn create_cylinder_field_flex(
	_node: Arc<Node>,
	calling_client: Arc<Client>,
	message: Message,
) -> Result<()> {
	#[derive(Deserialize)]
	struct CreateFieldInfo<'a> {
		name: &'a str,
		parent_path: &'a str,
		transform: Transform,
		length: f32,
		radius: f32,
	}
	let info: CreateFieldInfo = deserialize(message.as_ref())?;
	let node = Node::create_parent_name(&calling_client, "/field", info.name, true);
	let parent = find_spatial_parent(&calling_client, info.parent_path)?;
	let transform = parse_transform(info.transform, true, true, false);
	let node = node.add_to_scenegraph()?;
	Spatial::add_to(&node, Some(parent), transform, false)?;
	CylinderField::add_to(&node, info.length, info.radius)?;
	Ok(())
}
