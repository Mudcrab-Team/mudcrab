//! Offline conversion of Skyrim assets into runtime-ready OpenSkyrim assets.

pub mod archive;
pub mod asset_path;
pub mod cache;
pub mod check;
pub mod config;
pub mod esm;
pub mod integration;
pub mod lip;
pub mod material;
pub mod mesh;
pub mod pipeline;
pub mod progress;
pub mod script;
pub mod texture;

#[cfg(test)]
mod test_strategies;
pub use check::{
    CheckCancelled, CheckMode, CheckProblem, CheckReport, check_output, check_output_with_cancel,
};
pub use config::{PipelineConfig, find_resumable_staging};
pub use esm::EsmParser;
pub use integration::IntegrationReport;
pub use pipeline::{AssetPipeline, Cancellation, PipelineFailure, PipelineReport};
pub use progress::{ProgressEstimate, ProgressEvent, ProgressStage};
