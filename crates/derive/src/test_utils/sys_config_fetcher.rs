//! Implements a mock [L2SystemConfigFetcher] for testing.

use crate::traits::L2ChainProvider;
use alloc::{boxed::Box, sync::Arc};
use alloy_primitives::map::HashMap;
use async_trait::async_trait;
use op_alloy_consensus::OpBlock;
use op_alloy_genesis::{RollupConfig, SystemConfig};
use op_alloy_protocol::L2BlockInfo;

/// A mock implementation of the `SystemConfigL2Fetcher` for testing.
#[derive(Debug, Default)]
pub struct TestSystemConfigL2Fetcher {
    /// A map from [u64] block number to a [SystemConfig].
    pub system_configs: HashMap<u64, SystemConfig>,
}

impl TestSystemConfigL2Fetcher {
    /// Inserts a new system config into the mock fetcher with the given block number.
    pub fn insert(&mut self, number: u64, config: SystemConfig) {
        self.system_configs.insert(number, config);
    }

    /// Clears all system configs from the mock fetcher.
    pub fn clear(&mut self) {
        self.system_configs.clear();
    }
}

/// An error returned by the [TestSystemConfigL2Fetcher].
#[derive(derive_more::Display, Debug, PartialEq, Eq)]
pub enum TestSystemConfigL2FetcherError {
    /// The system config was not found.
    #[display("system config not found: {_0}")]
    NotFound(u64),
}

impl core::error::Error for TestSystemConfigL2FetcherError {}

#[async_trait]
impl L2ChainProvider for TestSystemConfigL2Fetcher {
    type Error = TestSystemConfigL2FetcherError;

    async fn system_config_by_number(
        &mut self,
        number: u64,
        _: Arc<RollupConfig>,
    ) -> Result<SystemConfig, Self::Error> {
        self.system_configs
            .get(&number)
            .cloned()
            .ok_or_else(|| TestSystemConfigL2FetcherError::NotFound(number))
    }

    async fn l2_block_info_by_number(&mut self, _: u64) -> Result<L2BlockInfo, Self::Error> {
        unimplemented!()
    }

    async fn block_by_number(&mut self, _: u64) -> Result<OpBlock, Self::Error> {
        unimplemented!()
    }
}
