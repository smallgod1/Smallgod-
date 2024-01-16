use color_eyre::{eyre::WrapErr, Result};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, info};

use crate::{
	network::p2p::Client as P2pClient,
	shutdown::Controller,
	telemetry::{MetricValue, Metrics},
	types::BlockVerified,
};

pub async fn process_block(
	block_number: u32,
	p2p_client: &P2pClient,
	metrics: &Arc<impl Metrics>,
) -> Result<()> {
	p2p_client
		.shrink_kademlia_map()
		.await
		.wrap_err("Unable to perform Kademlia map shrink")?;

	if let Ok((multiaddr, ip)) = p2p_client.get_multiaddress_and_ip().await {
		metrics.set_multiaddress(multiaddr).await;
		metrics.set_ip(ip).await;
	}

	let peers_num = p2p_client.count_dht_entries().await?;
	let peers_num_metric = MetricValue::KadRoutingPeerNum(peers_num);

	metrics.record(peers_num_metric).await?;
	metrics.record(MetricValue::HealthCheck()).await?;

	debug!(block_number, "Maintenance completed");
	Ok(())
}

pub async fn run(
	p2p_client: P2pClient,
	metrics: Arc<impl Metrics>,
	mut block_receiver: broadcast::Receiver<BlockVerified>,
	shutdown: Controller<String>,
) {
	info!("Starting maintenance...");

	loop {
		let result = match block_receiver.recv().await {
			Ok(block) => process_block(block.block_num, &p2p_client, &metrics).await,
			Err(error) => Err(error.into()),
		};

		if let Err(error) = result {
			let _ = shutdown.trigger_shutdown(format!("{error:#}"));
			break;
		}
	}
}
