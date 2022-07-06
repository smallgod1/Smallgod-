extern crate threadpool;

use std::sync::{mpsc::channel, Arc};

use kate_recovery::com::Cell;

// Just a wrapper function, to be used when spawning threads for verifying proofs
// for a certain block
fn kc_verify_proof_wrapper(
	block_num: u64,
	row: u16,
	col: u16,
	total_rows: usize,
	total_cols: usize,
	proof: &[u8],
	commitment: &[u8],
) -> bool {
	let pp = kate_proof::testnet::public_params(total_cols);
	match kate_proof::kc_verify_proof(col as u32, proof, commitment, total_rows, total_cols, &pp) {
		// Ok(verification) => {
		// 	let public_params_hash =
		// 		hex::encode(sp_core::blake2_128(verification.public_params.as_slice()));
		// 	let public_params_len = hex::encode(verification.public_params.as_slice()).len();
		// 	log::trace!("Public params ({public_params_len}): hash: {public_params_hash}");
		// 	match &verification.status {
		// 		Ok(()) => {
		// 			log::trace!("Verified cell ({row}, {col}) of block {block_num}");
		// 		},
		// 		Err(verification_err) => {
		// 			log::error!("Verification for cell ({row}, {col}) of block {block_num} failed: {verification_err}");
		// 		},
		// 	}
		// 	verification.status.is_ok()
		// },
		Ok(_) => {
			let raw_pp = pp.to_raw_var_bytes();
			let public_params_hash = hex::encode(sp_core::blake2_128(&raw_pp));
			let public_params_len = hex::encode(raw_pp).len();
			log::trace!("Public params ({public_params_len}): hash: {public_params_hash}");
			log::trace!("Verified cell ({row}, {col}) of block {block_num}");
			true
		},
		Err(error) => {
			log::error!("Verify failed for cell ({row}, {col}) of block {block_num}: {error}");
			false
		},
	}
}

pub fn verify_proof(
	block_num: u64,
	total_rows: u16,
	total_cols: u16,
	cells: &[Cell],
	commitment: Vec<u8>,
) -> usize {
	let cpus = num_cpus::get();
	let pool = threadpool::ThreadPool::new(cpus);
	let (tx, rx) = channel::<bool>();
	let jobs = cells.len();
	let commitment = Arc::new(commitment);

	for cell in cells.iter().cloned() {
		let row = cell.position.row;
		let col = cell.position.col;
		let tx = tx.clone();
		let commitment = commitment.clone();

		pool.execute(move || {
			if let Err(error) = tx.send(kc_verify_proof_wrapper(
				block_num,
				row,
				col,
				total_rows as usize,
				total_cols as usize,
				&cell.content,
				&commitment[row as usize * 48..(row as usize + 1) * 48],
			)) {
				log::error!("Failed to send proof verified message: {error}");
			};
		});
	}

	rx.iter().take(jobs).filter(|&v| v).count()
}
