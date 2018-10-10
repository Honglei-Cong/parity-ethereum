// Copyright 2015-2018 Parity Technologies (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

//! A provider for the PIP protocol. This is typically a full node, who can
//! give as much data as necessary to its peers.

use std::sync::Arc;

use ethcore::blockchain_info::BlockChainInfo;
use ethcore::client::{BlockChainClient, ProvingBlockChainClient, ChainInfo, BlockInfo as ClientBlockInfo};
use ethcore::ids::BlockId;
use ethcore::encoded;
use ethereum_types::H256;
use parking_lot::RwLock;
use transaction::PendingTransaction;

use cht::{self, BlockInfo};
use client::{LightChainClient, AsLightClient};
use transaction_queue::TransactionQueue;

use request;

/// Maximum allowed size of a headers request.
pub const MAX_HEADERS_PER_REQUEST: u64 = 512;

/// Defines the operations that a provider for the light subprotocol must fulfill.
pub trait Provider: Send + Sync {
	/// Provide current blockchain info.
	fn chain_info(&self) -> BlockChainInfo;

	/// Find the depth of a common ancestor between two blocks.
	/// If either block is unknown or an ancestor can't be found
	/// then return `None`.
	fn reorg_depth(&self, a: &H256, b: &H256) -> Option<u64>;

	/// Earliest block where state queries are available.
	/// If `None`, no state queries are servable.
	fn earliest_state(&self) -> Option<u64>;

	/// Provide a list of headers starting at the requested block,
	/// possibly in reverse and skipping `skip` at a time.
	///
	/// The returned vector may have any length in the range [0, `max`], but the
	/// results within must adhere to the `skip` and `reverse` parameters.
	fn block_headers(&self, req: request::CompleteHeadersRequest) -> request::HeadersResponse {
		use request::HashOrNumber;

		if req.max == 0 { 
			return Default::default();
		};

		let best_num = self.chain_info().best_block_number;
		let start_num = match req.start {
			HashOrNumber::Number(start_num) => start_num,
			HashOrNumber::Hash(hash) => {
				let header = self.block_header(BlockId::Hash(hash));
				if header.is_empty() {
					trace!(target: "pip_provider", "Unknown block hash {} requested", hash);
					return Default::default();
				}
				else {
					let num = header.number();
					let _canon_hash = self.block_header(BlockId::Number(num)).hash();

					if req.max <= 1 {
						// Non-canonical header or single header requested.
						return request::HeadersResponse { headers: vec![header] };
					}

					num
				}
			}
		};

		let max = ::std::cmp::min(MAX_HEADERS_PER_REQUEST, req.max);

		let headers: Vec<_> = (0_u64..max)
			.map(|x: u64| x.saturating_mul(req.skip.saturating_add(1)))
			.take_while(|&x| if req.reverse { x < start_num } else { best_num.saturating_sub(start_num) >= x })
			.map(|x| if req.reverse { start_num.saturating_sub(x) } else { start_num.saturating_add(x) })
			.map(|x| self.block_header(BlockId::Number(x)))
			.take_while(|x| !x.is_empty())
			.collect();

		request::HeadersResponse { headers }
	}

	/// Get a block header by id.
	fn block_header(&self, id: BlockId) -> encoded::Header;

	/// Get a transaction index by hash.
	fn transaction_index(&self, req: request::CompleteTransactionIndexRequest)
		-> request::TransactionIndexResponse;

	/// Fulfill a block body request.
	fn block_body(&self, req: request::CompleteBodyRequest) -> request::BodyResponse;

	/// Fulfill a request for block receipts.
	fn block_receipts(&self, req: request::CompleteReceiptsRequest) -> request::ReceiptsResponse;

	/// Get an account proof.
	fn account_proof(&self, req: request::CompleteAccountRequest) -> request::AccountResponse;

	/// Get a storage proof.
	fn storage_proof(&self, req: request::CompleteStorageRequest) -> request::StorageResponse;

	/// Provide contract code for the specified (block_hash, code_hash) pair.
	fn contract_code(&self, req: request::CompleteCodeRequest) -> request::CodeResponse;

	/// Provide a header proof from a given Canonical Hash Trie as well as the
	/// corresponding header.
	fn header_proof(&self, req: request::CompleteHeaderProofRequest) -> request::HeaderProofResponse;

	/// Provide pending transactions.
	fn transactions_to_propagate(&self) -> Vec<PendingTransaction>;

	/// Provide a proof-of-execution for the given transaction proof request.
	/// Returns a vector of all state items necessary to execute the transaction.
	fn transaction_proof(&self, req: request::CompleteExecutionRequest) -> request::ExecutionResponse;

	/// Provide epoch signal data at given block hash. This should be just the
	fn epoch_signal(&self, req: request::CompleteSignalRequest) -> request::SignalResponse;
}

// Implementation of a light client data provider for a client.
impl<T: ProvingBlockChainClient + ?Sized> Provider for T {
	fn chain_info(&self) -> BlockChainInfo {
		let chain_info = ChainInfo::chain_info(self);
		trace!(target: "pip_provider", " chain_info {:?}", chain_info);
		chain_info
	}

	fn reorg_depth(&self, a: &H256, b: &H256) -> Option<u64> {
		let reorg = self.tree_route(a, b).map(|route| route.index as u64);
		trace!(target: "pip_provider", " reorg_depth {:?}", reorg);
		reorg
	}

	fn earliest_state(&self) -> Option<u64> {
		let earliest_state = self.pruning_info().earliest_state;
		trace!(target: "pip_provider", "earliest_state {:?}", earliest_state);
		Some(earliest_state)
	}

	fn block_header(&self, id: BlockId) -> encoded::Header {
                let mut is_success = true;
                let block_header = ClientBlockInfo::block_header(self, id).unwrap_or_else(|| {
                    use ethcore::header;
                    use rlp;
                        
                    let empty = rlp::EMPTY_LIST_RLP.to_vec();
                    let mut rlp = rlp::RlpStream::new_list(1);
                    rlp.append_raw(&empty, 1);

                    is_success = false;
                    encoded::Header::new(rlp.out())
                });
		debug!(target: "pip_provider", "block_header response: block_id: {:?} was succesful {}", id, !block_header.is_empty());
		trace!(target: "pip_provider", "block_header {:?} ", block_header);
		block_header
	}

	fn transaction_index(&self, req: request::CompleteTransactionIndexRequest) -> request::TransactionIndexResponse {
		use ethcore::ids::TransactionId;

		let transaction_receipt = self.transaction_receipt(TransactionId::Hash(req.hash)).map(|receipt| request::TransactionIndexResponse {
			inner: Some(request::TransactionIndexResponseInner { 
				num: receipt.block_number,
				hash: receipt.block_hash,
				index: receipt.transaction_index as u64,
			}),
		}).unwrap_or_default();
		debug!(target: "pip_provider", "TransactionReceipt response: req.hash {} was succesful: {}", req.hash, transaction_receipt.inner.is_some());
		trace!(target: "pip_provider", "transaction_receipt: {:?} ", transaction_receipt);
		transaction_receipt
	}

	fn block_body(&self, req: request::CompleteBodyRequest) -> request::BodyResponse {
		let mut is_success = true;
		let block_body = BlockChainClient::block_body(self, BlockId::Hash(req.hash))
			.map_or_else(|| {
				use ethcore;
				use rlp;
				is_success = false;
				
				let empty = rlp::EMPTY_LIST_RLP.to_vec();

				let mut body = rlp::RlpStream::new_list(2);
				body.append_raw(&empty, 1);
				body.append_raw(&empty, 1);
				
				request::BodyResponse { body: ethcore::encoded::Body::new(body.out()) }
			}, |body| request::BodyResponse { body });
		debug!(target: "pip_provider", "block_body response: request_hash {}: was succesful: {}", req.hash, is_success);
		trace!(target: "pip_provider", "block_body {:?}", block_body);
		block_body
	}

	fn block_receipts(&self, req: request::CompleteReceiptsRequest) -> request::ReceiptsResponse {
		let block_receipt = BlockChainClient::encoded_block_receipts(self, &req.hash)
			.map_or_else(Default::default, |receipt| request::ReceiptsResponse { receipts: ::rlp::decode_list(&receipt) });
		debug!(target: "pip_provider", "block_receipts response: request_hash {}: was succesful: {}", req.hash, !block_receipt.receipts.is_empty());
		trace!(target: "pip_provider", "block_receipts {:?}", block_receipt);
		block_receipt
	}

	fn account_proof(&self, req: request::CompleteAccountRequest) -> request::AccountResponse {
		let account_proof = self.prove_account(req.address_hash, BlockId::Hash(req.block_hash))
			.map_or_else(Default::default, |(proof, acc)| {
				request::AccountResponse {
					proof,
					nonce: acc.nonce,
					balance: acc.balance,
					code_hash: acc.code_hash,
					storage_root: acc.storage_root,
				}
		});
		debug!(target: "pip_provider", "account_proof response for account: {} was succesful: {}", req.address_hash, !account_proof.proof.is_empty());
		trace!(target: "pip_provider", "account: {:?}", account_proof);
		account_proof
	}

	fn storage_proof(&self, req: request::CompleteStorageRequest) -> request::StorageResponse {
		let storage_proof = self.prove_storage(req.address_hash, req.key_hash, BlockId::Hash(req.block_hash))
			.map_or_else(Default::default, |(proof, item) | {
				request::StorageResponse {
					proof,
					value: item,
				}
		});
		debug!(target: "pip_provider", "storage_proof response for (block_hash: {} account: {}) was succesful: {}", 
			req.block_hash, req.address_hash, !storage_proof.proof.is_empty());
		trace!(target: "pip_provider", "storage: {:?}", storage_proof);
		storage_proof
	}

	fn contract_code(&self, req: request::CompleteCodeRequest) -> request::CodeResponse {
		let contract_code = self.state_data(&req.code_hash)
			.map_or_else(Default::default, |code| request::CodeResponse { code });
		debug!(target: "pip_provider", "contract_code response for (block_hash {} code_hash {}) was successful: {}", 
				req.block_hash, req.code_hash, !contract_code.code.is_empty());
		trace!(target: "pip_provider", "contrace_code{:?}", contract_code);
		contract_code
	}

	fn header_proof(&self, req: request::CompleteHeaderProofRequest) -> request::HeaderProofResponse {
		let cht_number = match cht::block_to_cht_number(req.num) {
			Some(cht_num) => cht_num,
			None => {
				debug!(target: "pip_provider", "Requested CHT proof with invalid block number");
				return Default::default()
			}
		};

		let mut needed = None;

		// build the CHT, caching the requested header as we pass through it.
		let cht = {
			let block_info = |id| {
				let hdr = self.block_header(id);
				let td = self.block_total_difficulty(id);

				match (hdr, td) {
					(Some(hdr), Some(td)) => {
						let info = BlockInfo {
							hash: hdr.hash(),
							parent_hash: hdr.parent_hash(),
							total_difficulty: td,
						};

						if hdr.number() == req.num {
							needed = Some((hdr, td));
						}

						Some(info)
					}
					_ => None,
				}
			};

			match cht::build(cht_number, block_info) {
				Some(cht) => cht,
				None => {
					debug!(target: "pip_provider", "Couldn't build CHT with cht_number: {}", cht_number);
					return Default::default()
				}
			}
		};

		let (needed_hdr, needed_td) = needed.expect("`needed` always set in loop, number checked before; qed");

		// prove our result.
		let cht_proof = match cht.prove(req.num, 0) {
			Ok(Some(proof)) => request::HeaderProofResponse {
				proof,
				hash: needed_hdr.hash(),
				td: needed_td,
			},
			Ok(None) => Default::default(),
			Err(e) => {
				debug!(target: "pip_provider", "Error looking up number in freshly-created CHT: {}", e);
				Default::default()
			}
		};
		debug!(target: "pip_provider", "CHT proof is success: {}", !cht_proof.proof.is_empty());
		trace!(target: "pip_provider", "CHT proof {:?}", cht_proof.proof);
		cht_proof
	}

	fn transaction_proof(&self, req: request::CompleteExecutionRequest) -> request::ExecutionResponse {
		use transaction::Transaction;

		let id = BlockId::Hash(req.block_hash);
		let nonce = match self.nonce(&req.from, id) {
			Some(nonce) => nonce,
			None => {
				debug!(target: "pip_provider", "Couldn't find nonce in the execution proof");
				return Default::default();
			}
		};
		let transaction = Transaction {
			nonce,
			gas: req.gas,
			gas_price: req.gas_price,
			action: req.action,
			value: req.value,
			data: req.data,
		}.fake_sign(req.from);

		let transaction_proof = self.prove_transaction(transaction, id)
			.map_or_else(Default::default, |(_, proof)| request::ExecutionResponse { items: proof });
		debug!(target: "pip_provider", "transaction_proof response (tx_hash {} from: {} value: {}) was successful: {}", 
                       req.block_hash, req.from, req.value, !transaction_proof.items.is_empty());
		trace!(target: "pip_provider", "transaction_proof: {:?}", transaction_proof);
		transaction_proof
	}

	fn transactions_to_propagate(&self) -> Vec<PendingTransaction> {
		let transactions_to_propagate = BlockChainClient::transactions_to_propagate(self)
			.into_iter()
			.map(|tx| tx.pending().clone())
			.collect();
		trace!(target: "pip_provider", "transactions_to_propagate: {:?}", transactions_to_propagate);
		transactions_to_propagate
	}

	fn epoch_signal(&self, req: request::CompleteSignalRequest) -> request::SignalResponse {
		let epoch_signal = self.epoch_signal(req.block_hash)
			.map_or_else(Default::default, |signal| request::SignalResponse { signal });
		trace!(target: "pip_provider", "epoch_signal response was succesful =  {}", !epoch_signal.signal.is_empty());
		epoch_signal
	}
}

/// The light client "provider" implementation. This wraps a `LightClient` and
/// a light transaction queue.
pub struct LightProvider<L> {
	client: Arc<L>,
	tx_queue: Arc<RwLock<TransactionQueue>>,
}

impl<L> LightProvider<L> {
	/// Create a new `LightProvider` from the given client and transaction queue.
	pub fn new(client: Arc<L>, tx_queue: Arc<RwLock<TransactionQueue>>) -> Self {
		LightProvider {
			client,
			tx_queue,
		}
	}
}

// TODO: draw from cache (shared between this and the RPC layer)
impl<L: AsLightClient + Send + Sync> Provider for LightProvider<L> {
	fn chain_info(&self) -> BlockChainInfo {
		self.client.as_light_client().chain_info()
	}

	fn reorg_depth(&self, _a: &H256, _b: &H256) -> Option<u64> {
		None
	}

	fn earliest_state(&self) -> Option<u64> {
		None
	}

	fn block_header(&self, _id: BlockId) -> encoded::Header {
		Default::default()
	}

	fn transaction_index(&self, _req: request::CompleteTransactionIndexRequest)
		-> request::TransactionIndexResponse
	{
		Default::default()
	}

	fn block_body(&self, _req: request::CompleteBodyRequest) -> request::BodyResponse {
		Default::default()
	}

	fn block_receipts(&self, _req: request::CompleteReceiptsRequest) -> request::ReceiptsResponse {
		Default::default()
	}

	fn account_proof(&self, _req: request::CompleteAccountRequest) -> request::AccountResponse {
		Default::default()
	}

	fn storage_proof(&self, _req: request::CompleteStorageRequest) -> request::StorageResponse {
		Default::default()
	}

	fn contract_code(&self, _req: request::CompleteCodeRequest) -> request::CodeResponse {
		Default::default()
	}

	fn header_proof(&self, _req: request::CompleteHeaderProofRequest) -> request::HeaderProofResponse {
		Default::default()
	}

	fn transaction_proof(&self, _req: request::CompleteExecutionRequest) -> request::ExecutionResponse {
		Default::default()
	}

	fn epoch_signal(&self, _req: request::CompleteSignalRequest) -> request::SignalResponse {
		Default::default()
	}

	fn transactions_to_propagate(&self) -> Vec<PendingTransaction> {
		let chain_info = self.chain_info();
		self.tx_queue.read()
			.ready_transactions(chain_info.best_block_number, chain_info.best_block_timestamp)
	}
}

impl<L: AsLightClient> AsLightClient for LightProvider<L> {
	type Client = L::Client;

	fn as_light_client(&self) -> &L::Client {
		self.client.as_light_client()
	}
}

#[cfg(test)]
mod tests {
	use ethcore::client::{EachBlockWith, TestBlockChainClient};
	use super::Provider;

	#[test]
	fn cht_proof() {
		let client = TestBlockChainClient::new();
		client.add_blocks(2000, EachBlockWith::Nothing);

		let req = ::request::CompleteHeaderProofRequest {
			num: 1500,
		};

		assert!(client.header_proof(req.clone()).is_none());

		client.add_blocks(48, EachBlockWith::Nothing);

		assert!(client.header_proof(req.clone()).is_some());
	}
}
