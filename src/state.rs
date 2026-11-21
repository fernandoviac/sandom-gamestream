use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// What Moonshine remembers about a paired device, keyed by its certificate fingerprint.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceRecord {
	/// Name the user gave the device. Empty until the user names it.
	pub name: String,

	/// When the device paired, in seconds since the Unix epoch. Zero when unknown.
	pub paired_at: u64,

	/// When the device last made a request, in seconds since the Unix epoch. Zero when unknown.
	pub last_seen: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StateData {
	unique_id: String,
	#[serde(default)]
	clients: HashSet<String>,
	#[serde(default)]
	paired_certs: HashSet<String>,
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	devices: BTreeMap<String, DeviceRecord>,
}

impl StateData {
	fn new() -> Self {
		Self {
			unique_id: uuid::Uuid::new_v4().to_string(),
			clients: Default::default(),
			paired_certs: Default::default(),
			devices: Default::default(),
		}
	}
}

#[derive(Clone)]
pub struct PersistentState {
	data: Arc<RwLock<StateData>>,
	path: PathBuf,
}

impl PersistentState {
	pub(crate) fn new(path: PathBuf) -> Result<Self, ()> {

		let data = if path.exists() {
			let serialized =
				std::fs::read_to_string(&path).map_err(|e| tracing::error!("Failed to read state file: {e}"))?;
			let data: StateData = toml::from_str(&serialized)
				.map_err(|e| tracing::error!("Failed to parse state file at '{}': {e}", path.display()))?;

			tracing::debug!("Successfully loaded state from {:?}", path);
			tracing::trace!("State: {data:?}");

			data
		} else {
			StateData::new()
		};

		let state = Self {
			data: Arc::new(RwLock::new(data)),
			path,
		};
		state.save()?;

		Ok(state)
	}

	pub fn get_uuid(&self) -> Result<String, ()> {
		let data = self
			.data
			.read()
			.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
		Ok(data.unique_id.clone())
	}

	pub(crate) fn save(&self) -> Result<(), ()> {
		let data = self
			.data
			.read()
			.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
		let parent_dir = self
			.path
			.parent()
			.ok_or_else(|| tracing::warn!("Failed to get state dir for file {:?}", self.path))?;
		std::fs::create_dir_all(parent_dir).map_err(|e| tracing::warn!("Failed to create state dir: {e}"))?;

		std::fs::write(
			&self.path,
			toml::to_string_pretty(&*data).map_err(|e| tracing::warn!("Failed to serialize state: {e}"))?,
		)
		.map_err(|e| tracing::warn!("Failed to save state file: {e}"))
	}

	pub(crate) fn has_client(&self, client: String) -> Result<bool, ()> {
		let data = self
			.data
			.read()
			.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
		Ok(data.clients.contains(&client))
	}

	pub(crate) fn add_client(&self, client: String) -> Result<bool, ()> {
		let has_client = {
			let mut data = self
				.data
				.write()
				.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
			if data.clients.contains(&client) {
				tracing::warn!("Failed to add client ('{client}'), client already exists.");
				false
			} else {
				data.clients.insert(client);
				true
			}
		};
		if has_client {
			self.save()?;
		}
		Ok(has_client)
	}

	pub(crate) fn has_paired_cert(&self, fingerprint: String) -> Result<bool, ()> {
		let data = self
			.data
			.read()
			.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
		Ok(data.paired_certs.contains(&fingerprint))
	}

	pub(crate) fn add_paired_cert(&self, fingerprint: String) -> Result<bool, ()> {
		let inserted = {
			let mut data = self
				.data
				.write()
				.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
			data.paired_certs.insert(fingerprint)
		};
		if inserted {
			self.save()?;
		} else {
			tracing::warn!("Failed to add paired cert, already exists.");
		}
		Ok(inserted)
	}

	/// Change what is known about a paired certificate, then save when anything changed and
	/// `persist` returns true for the old and new record.
	pub(crate) fn update_device(
		&self,
		fingerprint: &str,
		update: impl FnOnce(&mut DeviceRecord),
		persist: impl FnOnce(&DeviceRecord, &DeviceRecord) -> bool,
	) -> Result<bool, ()> {
		let save = {
			let mut data = self
				.data
				.write()
				.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
			if !data.paired_certs.contains(fingerprint) {
				return Ok(false);
			}
			let record = data.devices.entry(fingerprint.to_string()).or_default();
			let old = record.clone();
			update(record);
			*record != old && persist(&old, record)
		};
		if save {
			self.save()?;
		}
		Ok(true)
	}

	// pub fn remove_client(&self, client: String) -> Result<bool, ()> {
	// 	let mut data = self.data.write().map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
	// 	if !data.clients.contains(&client) {
	// 		tracing::error!("Failed to remove client ('{client}'), client doesn't exist.");
	// 		return Ok(false);
	// 	}
	// 	data.clients.shift_remove(&client);
	// 	self.save()?;
	// 	Ok(true)
	// }
}
