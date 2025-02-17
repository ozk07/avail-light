use color_eyre::{eyre::WrapErr, Result};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info};

use crate::{
	network::p2p::Client as P2pClient,
	shutdown::Controller,
	telemetry::{MetricValue, Metrics},
	types::BlockVerified,
};

#[derive(Clone, Copy)]
pub struct StaticConfigParams {
	pub block_confidence_treshold: f64,
	pub replication_factor: u16,
	pub query_timeout: u32,
	pub pruning_interval: u32,
	pub telemetry_flush_interval: u32,
}

pub async fn process_block(
	block_number: u32,
	p2p_client: &P2pClient,
	static_config_params: StaticConfigParams,
	metrics: &Arc<impl Metrics>,
) -> Result<()> {
	#[cfg(not(feature = "kademlia-rocksdb"))]
	if block_number % static_config_params.pruning_interval == 0 {
		info!(block_number, "Pruning...");
		match p2p_client.prune_expired_records().await {
			Ok(pruned) => info!(block_number, pruned, "Pruning finished"),
			Err(error) => error!(block_number, "Pruning failed: {error:#}"),
		}
	}

	if block_number % static_config_params.telemetry_flush_interval == 0 {
		info!(block_number, "Flushing metrics...");
		match metrics.flush().await {
			Ok(()) => info!(block_number, "Flushing metrics finished"),
			Err(error) => error!(block_number, "Flushing metrics failed: {error:#}"),
		}
	}

	p2p_client
		.shrink_kademlia_map()
		.await
		.wrap_err("Unable to perform Kademlia map shrink")?;

	let map_size = p2p_client
		.get_kademlia_map_size()
		.await
		.wrap_err("Unable to get Kademlia map size")?;

	let (peers_num, pub_peers_num) = p2p_client.count_dht_entries().await?;
	info!("Number of peers in the routing table: {peers_num}. Number of peers with public IPs: {pub_peers_num}.");

	let connected_peers = p2p_client.list_connected_peers().await?;
	debug!("Connected peers: {:?}", connected_peers);

	let peers_num_metric = MetricValue::DHTConnectedPeers(peers_num);
	metrics.record(peers_num_metric).await;

	metrics
		.record(MetricValue::BlockConfidenceThreshold(
			static_config_params.block_confidence_treshold,
		))
		.await;
	metrics
		.record(MetricValue::DHTReplicationFactor(
			static_config_params.replication_factor,
		))
		.await;
	metrics
		.record(MetricValue::DHTQueryTimeout(
			static_config_params.query_timeout,
		))
		.await;
	metrics.record(MetricValue::Up()).await;

	info!(block_number, map_size, "Maintenance completed");
	Ok(())
}

pub async fn run(
	p2p_client: P2pClient,
	metrics: Arc<impl Metrics>,
	mut block_receiver: broadcast::Receiver<BlockVerified>,
	static_config_params: StaticConfigParams,
	shutdown: Controller<String>,
) {
	info!("Starting maintenance...");

	loop {
		let result = match block_receiver.recv().await {
			Ok(block) => {
				process_block(block.block_num, &p2p_client, static_config_params, &metrics).await
			},
			Err(error) => Err(error.into()),
		};

		if let Err(error) = result {
			let _ = shutdown.trigger_shutdown(format!("{error:#}"));
			break;
		}
	}
}
