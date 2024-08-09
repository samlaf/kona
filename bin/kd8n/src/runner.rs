//! Pipeline runner.
//!
//! The runner that executes the pipeline and validates the output given the test fixtures.

use anyhow::{anyhow, Result};
use op_test_vectors::derivation::DerivationFixture;
use kona_derive::pipeline::StepResult;
use kona_derive::types::StageError;
use kona_derive::traits::{L2ChainProvider, Pipeline};
use tracing::{error, debug, info, warn, trace};

use crate::providers::FixtureL2Provider;
use crate::pipeline::RunnerPipeline;

const LOG_TARGET: &str = "runner";

/// Runs the pipeline.
pub async fn run(mut pipeline: RunnerPipeline, fixture: DerivationFixture) -> Result<()> {
    let cursor_num = fixture.l2_block_infos.keys().min().ok_or_else(|| anyhow!("No blocks found"))?;
    let mut cursor = *fixture.l2_block_infos.get(cursor_num).ok_or_else(|| anyhow!("No block info found"))?;
    let mut l2_provider = FixtureL2Provider::from(fixture.clone());
    let mut advance_cursor_flag = false;
    let end = fixture.l2_block_infos.keys().max().ok_or_else(|| anyhow!("No blocks found"))?;
    loop {
        if advance_cursor_flag {
            match l2_provider.l2_block_info_by_number(cursor.block_info.number + 1).await {
                Ok(bi) => {
                    cursor = bi;
                    advance_cursor_flag = false;
                }
                Err(e) => {
                    error!(target: LOG_TARGET, "Failed to fetch next pending l2 safe head: {}, err: {:?}", cursor.block_info.number + 1, e);
                    // We don't need to step on the pipeline if we failed to fetch the next pending
                    // l2 safe head.
                    continue;
                }
            }
        }
        trace!(target: LOG_TARGET, "Stepping on cursor block number: {}", cursor.block_info.number);
        match pipeline.step(cursor).await {
            StepResult::PreparedAttributes => trace!(target: "loop", "Prepared attributes"),
            StepResult::AdvancedOrigin => trace!(target: "loop", "Advanced origin"),
            StepResult::OriginAdvanceErr(e) => warn!(target: "loop", "Could not advance origin: {:?}", e),
            StepResult::StepFailed(e) => match e {
                StageError::NotEnoughData => {
                    debug!(target: "loop", "Not enough data to step derivation pipeline");
                }
                _ => {
                    error!(target: "loop", "Error stepping derivation pipeline: {:?}", e);
                }
            },
        }

        // Take the next attributes from the pipeline.
        let Some(attributes) = pipeline.next() else {
            error!(target: LOG_TARGET, "Must have valid attributes");
            continue;
        };

        // Validate the attributes against the reference.
        let Some(expected) = fixture.l2_payloads.get(&cursor.block_info.number) else {
            return Err(anyhow!("No expected payload found"));
        };
        if attributes.attributes != *expected {
            error!(target: LOG_TARGET, "Attributes do not match expected");
            debug!(target: LOG_TARGET, "Expected: {:?}", expected);
            debug!(target: LOG_TARGET, "Actual: {:?}", attributes);
            return Err(anyhow!("Attributes do not match expected"));
        }
        if cursor.block_info.number == *end {
            info!(target: LOG_TARGET, "All payload attributes successfully validated");
            break;
        }
    }
    Ok(())
}
