//! Contains the core derivation pipeline.

use super::{
    NextAttributes, OriginAdvancer, OriginProvider, Pipeline, PipelineError, PipelineResult,
    ResettableStage, StepResult,
};
use crate::{
    errors::PipelineErrorKind,
    traits::{FlushableStage, Signal},
};
use alloc::{boxed::Box, collections::VecDeque, string::ToString, sync::Arc};
use async_trait::async_trait;
use core::fmt::Debug;
use kona_providers::L2ChainProvider;
use op_alloy_genesis::RollupConfig;
use op_alloy_protocol::{BlockInfo, L2BlockInfo};
use op_alloy_rpc_types_engine::OptimismAttributesWithParent;
use tracing::{error, trace, warn};

/// The derivation pipeline is responsible for deriving L2 inputs from L1 data.
#[derive(Debug)]
pub struct DerivationPipeline<S, P>
where
    S: NextAttributes
        + ResettableStage
        + FlushableStage
        + OriginProvider
        + OriginAdvancer
        + Debug
        + Send,
    P: L2ChainProvider + Send + Sync + Debug,
{
    /// A handle to the next attributes.
    pub attributes: S,
    /// Reset provider for the pipeline.
    /// A list of prepared [OptimismAttributesWithParent] to be used by the derivation pipeline
    /// consumer.
    pub prepared: VecDeque<OptimismAttributesWithParent>,
    /// The rollup config.
    pub rollup_config: Arc<RollupConfig>,
    /// The L2 Chain Provider used to fetch the system config on reset.
    pub l2_chain_provider: P,
}

impl<S, P> DerivationPipeline<S, P>
where
    S: NextAttributes
        + ResettableStage
        + FlushableStage
        + OriginProvider
        + OriginAdvancer
        + Debug
        + Send,
    P: L2ChainProvider + Send + Sync + Debug,
{
    /// Creates a new instance of the [DerivationPipeline].
    pub const fn new(
        attributes: S,
        rollup_config: Arc<RollupConfig>,
        l2_chain_provider: P,
    ) -> Self {
        Self { attributes, prepared: VecDeque::new(), rollup_config, l2_chain_provider }
    }
}

impl<S, P> OriginProvider for DerivationPipeline<S, P>
where
    S: NextAttributes
        + ResettableStage
        + FlushableStage
        + OriginProvider
        + OriginAdvancer
        + Debug
        + Send,
    P: L2ChainProvider + Send + Sync + Debug,
{
    fn origin(&self) -> Option<BlockInfo> {
        self.attributes.origin()
    }
}

impl<S, P> Iterator for DerivationPipeline<S, P>
where
    S: NextAttributes
        + ResettableStage
        + FlushableStage
        + OriginProvider
        + OriginAdvancer
        + Debug
        + Send
        + Sync,
    P: L2ChainProvider + Send + Sync + Debug,
{
    type Item = OptimismAttributesWithParent;

    fn next(&mut self) -> Option<Self::Item> {
        self.prepared.pop_front()
    }
}

#[async_trait]
impl<S, P> Pipeline for DerivationPipeline<S, P>
where
    S: NextAttributes
        + ResettableStage
        + FlushableStage
        + OriginProvider
        + OriginAdvancer
        + Debug
        + Send
        + Sync,
    P: L2ChainProvider + Send + Sync + Debug,
{
    /// Peeks at the next prepared [OptimismAttributesWithParent] from the pipeline.
    fn peek(&self) -> Option<&OptimismAttributesWithParent> {
        self.prepared.front()
    }

    /// Resets the pipeline by calling the [`ResettableStage::reset`] method.
    ///
    /// During a reset, each stage is recursively called from the top-level
    /// [crate::stages::AttributesQueue] to the bottom [crate::stages::L1Traversal]
    /// with a head-recursion pattern. This effectively clears the internal state
    /// of each stage in the pipeline from bottom on up.
    ///
    /// ### Parameters
    ///
    /// The `l2_block_info` is the new L2 cursor to step on. It is needed during
    /// reset to fetch the system config at that block height.
    ///
    /// The `l1_block_info` is the new L1 origin set in the [crate::stages::L1Traversal]
    /// stage.
    async fn signal(&mut self, signal: Signal) -> PipelineResult<()> {
        match signal {
            Signal::Reset { l2_safe_head, l1_origin } => {
                let system_config = self
                    .l2_chain_provider
                    .system_config_by_number(
                        l2_safe_head.block_info.number,
                        Arc::clone(&self.rollup_config),
                    )
                    .await
                    .map_err(|e| PipelineError::Provider(e.to_string()).temp())?;
                match self.attributes.reset(l1_origin, &system_config).await {
                    Ok(()) => trace!(target: "pipeline", "Stages reset"),
                    Err(err) => {
                        if let PipelineErrorKind::Temporary(PipelineError::Eof) = err {
                            trace!(target: "pipeline", "Stages reset with EOF");
                        } else {
                            error!(target: "pipeline", "Stage reset errored: {:?}", err);
                            return Err(err);
                        }
                    }
                }
            }
            Signal::FlushChannel => {
                self.attributes.flush_channel().await?;
            }
        }
        Ok(())
    }

    /// Attempts to progress the pipeline.
    ///
    /// ## Returns
    ///
    /// A [PipelineError::Eof] is returned if the pipeline is blocked by waiting for new L1 data.
    /// Any other error is critical and the derivation pipeline should be reset.
    /// An error is expected when the underlying source closes.
    ///
    /// When [DerivationPipeline::step] returns [Ok(())], it should be called again, to continue the
    /// derivation process.
    ///
    /// [PipelineError]: crate::errors::PipelineError
    async fn step(&mut self, cursor: L2BlockInfo) -> StepResult {
        match self.attributes.next_attributes(cursor).await {
            Ok(a) => {
                trace!(target: "pipeline", "Prepared L2 attributes: {:?}", a);
                self.prepared.push_back(a);
                StepResult::PreparedAttributes
            }
            Err(err) => match err {
                PipelineErrorKind::Temporary(PipelineError::Eof) => {
                    trace!(target: "pipeline", "Pipeline advancing origin");
                    if let Err(e) = self.attributes.advance_origin().await {
                        return StepResult::OriginAdvanceErr(e);
                    }
                    StepResult::AdvancedOrigin
                }
                _ => {
                    warn!(target: "pipeline", "Attributes queue step failed: {:?}", err);
                    StepResult::StepFailed(err)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::*;
    use alloy_rpc_types_engine::PayloadAttributes;
    use op_alloy_genesis::SystemConfig;
    use op_alloy_rpc_types_engine::OptimismPayloadAttributes;

    fn default_test_payload_attributes() -> OptimismAttributesWithParent {
        OptimismAttributesWithParent {
            attributes: OptimismPayloadAttributes {
                payload_attributes: PayloadAttributes {
                    timestamp: 0,
                    prev_randao: Default::default(),
                    suggested_fee_recipient: Default::default(),
                    withdrawals: None,
                    parent_beacon_block_root: None,
                },
                transactions: None,
                no_tx_pool: None,
                gas_limit: None,
                eip_1559_params: None,
            },
            parent: Default::default(),
            is_last_in_span: false,
        }
    }

    #[test]
    fn test_pipeline_next_attributes_empty() {
        let mut pipeline = new_test_pipeline();
        let result = pipeline.next();
        assert_eq!(result, None);
    }

    #[test]
    fn test_pipeline_next_attributes_with_peek() {
        let mut pipeline = new_test_pipeline();
        let expected = default_test_payload_attributes();
        pipeline.prepared.push_back(expected.clone());

        let result = pipeline.peek();
        assert_eq!(result, Some(&expected));

        let result = pipeline.next();
        assert_eq!(result, Some(expected));
    }

    #[tokio::test]
    async fn test_derivation_pipeline_missing_block() {
        let mut pipeline = new_test_pipeline();
        let cursor = L2BlockInfo::default();
        let result = pipeline.step(cursor).await;
        assert_eq!(
            result,
            StepResult::OriginAdvanceErr(
                PipelineError::Provider("Block not found".to_string()).temp()
            )
        );
    }

    #[tokio::test]
    async fn test_derivation_pipeline_prepared_attributes() {
        let rollup_config = Arc::new(RollupConfig::default());
        let l2_chain_provider = TestL2ChainProvider::default();
        let expected = default_test_payload_attributes();
        let attributes = TestNextAttributes { next_attributes: Some(expected) };
        let mut pipeline = DerivationPipeline::new(attributes, rollup_config, l2_chain_provider);

        // Step on the pipeline and expect the result.
        let cursor = L2BlockInfo::default();
        let result = pipeline.step(cursor).await;
        assert_eq!(result, StepResult::PreparedAttributes);
    }

    #[tokio::test]
    async fn test_derivation_pipeline_advance_origin() {
        let rollup_config = Arc::new(RollupConfig::default());
        let l2_chain_provider = TestL2ChainProvider::default();
        let attributes = TestNextAttributes::default();
        let mut pipeline = DerivationPipeline::new(attributes, rollup_config, l2_chain_provider);

        // Step on the pipeline and expect the result.
        let cursor = L2BlockInfo::default();
        let result = pipeline.step(cursor).await;
        assert_eq!(result, StepResult::AdvancedOrigin);
    }

    #[tokio::test]
    async fn test_derivation_pipeline_signal_reset_missing_sys_config() {
        let rollup_config = Arc::new(RollupConfig::default());
        let l2_chain_provider = TestL2ChainProvider::default();
        let attributes = TestNextAttributes::default();
        let mut pipeline = DerivationPipeline::new(attributes, rollup_config, l2_chain_provider);

        // Signal the pipeline to reset.
        let l2_safe_head = L2BlockInfo::default();
        let l1_origin = BlockInfo::default();
        let result = pipeline.signal(Signal::Reset { l2_safe_head, l1_origin }).await.unwrap_err();
        assert_eq!(result, PipelineError::Provider("System config not found".to_string()).temp());
    }

    #[tokio::test]
    async fn test_derivation_pipeline_signal_reset_ok() {
        let rollup_config = Arc::new(RollupConfig::default());
        let mut l2_chain_provider = TestL2ChainProvider::default();
        l2_chain_provider.system_configs.insert(0, SystemConfig::default());
        let attributes = TestNextAttributes::default();
        let mut pipeline = DerivationPipeline::new(attributes, rollup_config, l2_chain_provider);

        // Signal the pipeline to reset.
        let l2_safe_head = L2BlockInfo::default();
        let l1_origin = BlockInfo::default();
        let result = pipeline.signal(Signal::Reset { l2_safe_head, l1_origin }).await;
        assert!(result.is_ok());
    }
}
