// Copyright 2018-2019 Parity Technologies (UK) Ltd.
// This file is part of Substrate.

// Substrate is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Substrate is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Substrate.  If not, see <http://www.gnu.org/licenses/>.

//! Chain api required for the transaction pool.

use std::{marker::PhantomData, pin::Pin, sync::Arc};
use codec::{Decode, Encode};
use futures::{channel::oneshot, executor::{ThreadPool, ThreadPoolBuilder}, future::{Future, FutureExt, ready}};

use sc_client_api::{
	blockchain::HeaderBackend,
	light::{Fetcher, RemoteCallRequest}
};
use sp_core::{H256, Blake2Hasher, Hasher};
use sp_runtime::{generic::BlockId, traits::{self, Block as BlockT}, transaction_validity::TransactionValidity};
use sp_transaction_pool::runtime_api::TaggedTransactionQueue;

use crate::error::{self, Error};

/// The transaction pool logic for full client.
pub struct FullChainApi<T, Block> {
	client: Arc<T>,
	_marker: PhantomData<Block>,
}

impl<T, Block> FullChainApi<T, Block> where
	Block: BlockT,
	T: traits::ProvideRuntimeApi + traits::BlockIdTo<Block> {
	/// Create new transaction pool logic.
	pub fn new(client: Arc<T>) -> Self {
		FullChainApi {
			client,
			_marker: Default::default()
		}
	}
}

impl<T, Block> sc_transaction_graph::ChainApi for FullChainApi<T, Block> where
	Block: BlockT<Hash = H256>,
	T: traits::ProvideRuntimeApi + traits::BlockIdTo<Block> + 'static + Send + Sync,
	T::Api: TaggedTransactionQueue<Block>,
	sp_api::ApiErrorFor<T, Block>: Send,
{
	type Block = Block;
	type Hash = H256;
	type Error = error::Error;
	type ValidationFuture = Pin<Box<dyn Future<Output = error::Result<TransactionValidity>> + Send>>;

	fn validate_transaction(
		&self,
		at: &BlockId<Self::Block>,
		uxt: sc_transaction_graph::ExtrinsicFor<Self>,
	) -> Self::ValidationFuture {
		let client = self.client.clone();
		let at = at.clone();

		let res = client.runtime_api().validate_transaction(&at, uxt)
			.map_err(|e| Error::RuntimeApi(format!("{:?}", e)));

		Box::pin(async move { res })
	}

	fn block_id_to_number(
		&self,
		at: &BlockId<Self::Block>,
	) -> error::Result<Option<sc_transaction_graph::NumberFor<Self>>> {
		self.client.to_number(at).map_err(|e| Error::BlockIdConversion(format!("{:?}", e)))
	}

	fn block_id_to_hash(
		&self,
		at: &BlockId<Self::Block>,
	) -> error::Result<Option<sc_transaction_graph::BlockHash<Self>>> {
		self.client.to_hash(at).map_err(|e| Error::BlockIdConversion(format!("{:?}", e)))
	}

	fn hash_and_length(&self, ex: &sc_transaction_graph::ExtrinsicFor<Self>) -> (Self::Hash, usize) {
		ex.using_encoded(|x| {
			(Blake2Hasher::hash(x), x.len())
		})
	}
}

/// The transaction pool logic for light client.
pub struct LightChainApi<T, F, Block> {
	client: Arc<T>,
	fetcher: Arc<F>,
	_phantom: PhantomData<Block>,
}

impl<T, F, Block> LightChainApi<T, F, Block> where
	Block: BlockT,
	T: HeaderBackend<Block>,
	F: Fetcher<Block>,
{
	/// Create new transaction pool logic.
	pub fn new(client: Arc<T>, fetcher: Arc<F>) -> Self {
		LightChainApi {
			client,
			fetcher,
			_phantom: Default::default(),
		}
	}
}

impl<T, F, Block> sc_transaction_graph::ChainApi for LightChainApi<T, F, Block> where
	Block: BlockT<Hash=H256>,
	T: HeaderBackend<Block> + 'static,
	F: Fetcher<Block> + 'static,
{
	type Block = Block;
	type Hash = H256;
	type Error = error::Error;
	type ValidationFuture = Box<dyn Future<Output = error::Result<TransactionValidity>> + Send + Unpin>;

	fn validate_transaction(
		&self,
		at: &BlockId<Self::Block>,
		uxt: sc_transaction_graph::ExtrinsicFor<Self>,
	) -> Self::ValidationFuture {
		let header_hash = self.client.expect_block_hash_from_id(at);
		let header_and_hash = header_hash
			.and_then(|header_hash| self.client.expect_header(BlockId::Hash(header_hash))
				.map(|header| (header_hash, header)));
		let (block, header) = match header_and_hash {
			Ok((header_hash, header)) => (header_hash, header),
			Err(err) => return Box::new(ready(Err(err.into()))),
		};
		let remote_validation_request = self.fetcher.remote_call(RemoteCallRequest {
			block,
			header,
			method: "TaggedTransactionQueue_validate_transaction".into(),
			call_data: uxt.encode(),
			retry_count: None,
		});
		let remote_validation_request = remote_validation_request.then(move |result| {
			let result: error::Result<TransactionValidity> = result
				.map_err(Into::into)
				.and_then(|result| Decode::decode(&mut &result[..])
					.map_err(|e| Error::RuntimeApi(
						format!("Error decoding tx validation result: {:?}", e)
					))
				);
			ready(result)
		});

		Box::new(remote_validation_request)
	}

	fn block_id_to_number(&self, at: &BlockId<Self::Block>) -> error::Result<Option<sc_transaction_graph::NumberFor<Self>>> {
		Ok(self.client.block_number_from_id(at)?)
	}

	fn block_id_to_hash(&self, at: &BlockId<Self::Block>) -> error::Result<Option<sc_transaction_graph::BlockHash<Self>>> {
		Ok(self.client.block_hash_from_id(at)?)
	}

	fn hash_and_length(&self, ex: &sc_transaction_graph::ExtrinsicFor<Self>) -> (Self::Hash, usize) {
		ex.using_encoded(|x| {
			(Blake2Hasher::hash(x), x.len())
		})
	}
}
