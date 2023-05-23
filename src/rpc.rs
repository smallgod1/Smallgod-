//! RPC communication with avail node.

use std::{collections::HashSet, fmt::Display, ops::Deref};

use anyhow::{anyhow, Result};
use avail_subxt::{build_client, primitives::Header as DaHeader, AvailConfig};
use kate_recovery::{
	data::Cell,
	matrix::{Dimensions, Position},
};
use rand::{seq::SliceRandom, thread_rng, Rng};
use subxt::{
	rpc::{types::BlockNumber, RpcParams},
	utils::H256,
	OnlineClient,
};
use tracing::{debug, info, instrument, warn};

use crate::types::*;

async fn get_block_hash(client: &OnlineClient<AvailConfig>, block: u32) -> Result<H256> {
	client
		.rpc()
		.block_hash(Some(BlockNumber::from(block)))
		.await?
		.ok_or(anyhow!("Block with number {block} not found"))
}

async fn get_header_by_hash(client: &OnlineClient<AvailConfig>, hash: H256) -> Result<DaHeader> {
	client
		.rpc()
		.header(Some(hash))
		.await?
		.ok_or(anyhow!("Header with hash {hash:?} not found"))
}

/// RPC for obtaining header of latest block mined by network
// I'm writing this function so that I can check what's latest block number of chain
// and start syncer to fetch block headers for block range [0, LATEST]
pub async fn get_chain_header(client: &OnlineClient<AvailConfig>) -> Result<DaHeader> {
	client
		.rpc()
		.header(None)
		.await?
		.ok_or(anyhow!("Latest header not found"))
}

/// Gets header by block number
pub async fn get_header_by_block_number(
	client: &OnlineClient<AvailConfig>,
	block: u32,
) -> Result<(DaHeader, H256)> {
	let hash = get_block_hash(client, block).await?;
	get_header_by_hash(client, hash).await.map(|e| (e, hash))
}

/// Generates random cell positions for sampling
pub fn generate_random_cells(dimensions: &Dimensions, cell_count: u32) -> Vec<Position> {
	let max_cells = dimensions.extended_size();
	let count = if max_cells < cell_count.into() {
		debug!("Max cells count {max_cells} is lesser than cell_count {cell_count}");
		max_cells
	} else {
		cell_count.into()
	};
	let mut rng = thread_rng();
	let mut indices = HashSet::new();
	while (indices.len() as u16) < count as u16 {
		let col = rng.gen_range(0..dimensions.cols());
		let row = rng.gen_range(0..dimensions.extended_rows());
		indices.insert(Position { row, col });
	}

	indices.into_iter().collect::<Vec<_>>()
}

#[instrument(skip_all, level = "trace")]
pub async fn get_kate_rows(
	client: &OnlineClient<AvailConfig>,
	rows: Vec<u32>,
	block_hash: H256,
) -> Result<Vec<Option<Vec<u8>>>> {
	let mut params = RpcParams::new();
	params.push(rows)?;
	params.push(block_hash)?;
	let t = client.rpc().deref();
	t.request("kate_queryRows", params)
		.await
		.map_err(|e| anyhow!("RPC failed: {e}"))
}

/// RPC to get proofs for given positions of block
pub async fn get_kate_proof(
	client: &OnlineClient<AvailConfig>,
	block_hash: H256,
	positions: &[Position],
) -> Result<Vec<Cell>> {
	let mut params = RpcParams::new();
	params.push(positions)?;
	params.push(block_hash)?;
	let t = client.rpc().deref();
	let proofs: Vec<u8> = t
		.request("kate_queryProof", params)
		.await
		.map_err(|e| anyhow!("Error fetching proof: {e}"))?;

	let i = proofs
		.chunks_exact(CELL_WITH_PROOF_SIZE)
		.map(|chunk| chunk.try_into().expect("chunks of 80 bytes size"));
	Ok(positions
		.iter()
		.zip(i)
		.map(|(position, &content)| Cell {
			position: position.clone(),
			content,
		})
		.collect::<Vec<_>>())
}

// RPC to check connection to substrate node
async fn get_system_version(client: &OnlineClient<AvailConfig>) -> Result<String> {
	client
		.rpc()
		.system_version()
		.await
		.map_err(|e| anyhow!("Version couldn't be retrieved, error: {e}"))
}

async fn get_runtime_version(client: &OnlineClient<AvailConfig>) -> Result<RuntimeVersionResult> {
	let t = client.rpc().deref();
	t.request("state_getRuntimeVersion", RpcParams::new())
		.await
		.map_err(|e| anyhow!("Version couldn't be retrieved, error: {e}"))
}

/// Shuffles full nodes to randomize access,
/// and pushes last full node to the end of a list
/// so we can try it if connection to other node fails
fn shuffle_full_nodes(full_nodes: &[String], last_full_node: Option<String>) -> Vec<String> {
	let mut candidates = full_nodes.to_owned();
	candidates.retain(|node| Some(node) != last_full_node.as_ref());
	candidates.shuffle(&mut thread_rng());

	// Pushing last full node to the end of a list, if it's only one left to try
	if let (Some(node), true) = (last_full_node, full_nodes.len() != candidates.len()) {
		candidates.push(node);
	}
	candidates
}

pub struct Version {
	pub version: String,
	pub spec_version: u32,
	pub spec_name: String,
}

impl Version {
	pub fn matches(&self, other: &Version) -> bool {
		(self.version.starts_with(&other.version) || other.version.starts_with(&self.version))
			&& self.spec_name == other.spec_name
			&& self.spec_version == other.spec_version
	}
}

impl Display for Version {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(
			f,
			"v{}/{}/{}",
			self.version, self.spec_name, self.spec_version
		)
	}
}

/// Connects to the random full node from the list,
/// trying to connect to the last connected full node as least priority.
pub async fn connect_to_the_full_node(
	full_nodes: &[String],
	last_full_node: Option<String>,
	expected_version: Version,
) -> Result<(OnlineClient<AvailConfig>, String)> {
	for full_node_ws in shuffle_full_nodes(full_nodes, last_full_node).iter() {
		let log_warn = |error| {
			warn!("Skipping connection to {full_node_ws}: {error}");
			error
		};

		let Ok(client) = build_client(full_node_ws.clone()).await.map_err(log_warn) else { continue };
		let Ok(system_version) = get_system_version(&client).await.map_err(log_warn) else { continue; };
		let Ok(runtime_version) = get_runtime_version(&client).await.map_err(log_warn) else { continue; };

		let version = Version {
			version: system_version,
			spec_name: runtime_version.spec_name,
			spec_version: runtime_version.spec_version,
		};

		if !expected_version.matches(&version) {
			log_warn(anyhow!("expected {expected_version}, found {version}"));
			continue;
		}

		info!("Connection established to the node: {full_node_ws} <{version}>");
		return Ok((client, full_node_ws.clone()));
	}
	Err(anyhow!("No working nodes"))
}

/* @note: fn to take the number of cells needs to get equal to or greater than
the percentage of confidence mentioned in config file */

/// Callculates number of cells required to achieve given confidence
pub fn cell_count_for_confidence(confidence: f64) -> u32 {
	let mut cell_count: u32;
	if !(50.0..100f64).contains(&confidence) {
		//in this default of 8 cells will be taken
		debug!(
			"confidence is {} invalid so taking default confidence of 99",
			confidence
		);
		cell_count = (-((1f64 - (99f64 / 100f64)).log2())).ceil() as u32;
	} else {
		cell_count = (-((1f64 - (confidence / 100f64)).log2())).ceil() as u32;
	}
	if cell_count == 0 || cell_count > 10 {
		debug!(
			"confidence is {} invalid so taking default confidence of 99",
			confidence
		);
		cell_count = (-((1f64 - (99f64 / 100f64)).log2())).ceil() as u32;
	}
	cell_count
}

#[cfg(test)]
mod tests {
	use proptest::{
		prelude::any_with,
		prop_assert, prop_assert_eq, proptest,
		sample::size_range,
		strategy::{BoxedStrategy, Strategy},
	};
	use rand::{seq::SliceRandom, thread_rng};

	use crate::rpc::shuffle_full_nodes;

	fn full_nodes() -> BoxedStrategy<(Vec<String>, Option<String>)> {
		any_with::<Vec<String>>(size_range(10).lift())
			.prop_map(|nodes| {
				let last_node = nodes.choose(&mut thread_rng()).cloned();
				(nodes, last_node)
			})
			.boxed()
	}

	proptest! {
		#[test]
		fn shuffle_without_last((full_nodes, _) in full_nodes()) {
			let shuffled = shuffle_full_nodes(&full_nodes, None);
			prop_assert!(shuffled.len() == full_nodes.len());
			prop_assert!(shuffled.iter().all(|node| full_nodes.contains(node)));

			if !full_nodes.contains(&"invalid_node".to_string()) {
				let shuffled = shuffle_full_nodes(&full_nodes, Some("invalid_node".to_string()));
				prop_assert!(shuffled.len() == full_nodes.len());
				prop_assert!(shuffled.iter().all(|node| full_nodes.contains(node)))
			}
		}

		#[test]
		fn shuffle_with_last((full_nodes, last_full_node) in full_nodes()) {
			let last_full_node_count = full_nodes.iter().filter(|&n| Some(n) == last_full_node.as_ref()).count();

			let mut shuffled = shuffle_full_nodes(&full_nodes, last_full_node.clone());
			prop_assert_eq!(shuffled.pop(), last_full_node);

			// Assuming case when last full node occuring more than once in full nodes list
			prop_assert!(shuffled.len() == full_nodes.len() - last_full_node_count);
		}
	}
}
