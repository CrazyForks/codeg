use std::collections::BTreeMap;

use crate::app_error::{AppCommandError, AppErrorCode};

/// Errors raised by the loop engine and its services. `Conflict` is the
/// compare-and-swap miss (concurrent state change) that the frontend retries.
#[derive(Debug, thiserror::Error)]
pub enum LoopError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("illegal loop state transition")]
    IllegalTransition,
    #[error("conflicting concurrent update")]
    Conflict,
    #[error("loop space is detached from its folder")]
    Detached,
    #[error("folder is not a git repository")]
    NotGitRepo,
    #[error("merge conflict")]
    MergeConflict,
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("acp error: {0}")]
    Acp(String),
    #[error(transparent)]
    Db(#[from] sea_orm::DbErr),
}

impl From<LoopError> for AppCommandError {
    fn from(e: LoopError) -> Self {
        match e {
            LoopError::NotFound(m) => AppCommandError::not_found(m),
            LoopError::IllegalTransition => {
                AppCommandError::new(AppErrorCode::InvalidInput, "Illegal loop state transition")
            }
            // Surfaced as a retryable conflict (HTTP 409 via TurnInProgress); the
            // frontend renders the localized `Loops.conflictRetry` toast.
            LoopError::Conflict => AppCommandError::new(
                AppErrorCode::TurnInProgress,
                "Loop state changed concurrently; retry",
            )
            .with_i18n("Loops.conflictRetry", BTreeMap::new()),
            LoopError::Detached => AppCommandError::new(
                AppErrorCode::InvalidInput,
                "Loop space is detached from its folder",
            ),
            LoopError::NotGitRepo => {
                AppCommandError::not_a_git_repository("Loop space folder is not a git repository")
            }
            LoopError::MergeConflict => AppCommandError::new(
                AppErrorCode::ExternalCommandFailed,
                "Merge conflict while integrating the issue branch",
            ),
            LoopError::InvalidInput(m) => AppCommandError::invalid_input(m),
            LoopError::Acp(m) => AppCommandError::task_execution_failed(m),
            LoopError::Db(err) => {
                AppCommandError::database_error("Database operation failed").with_detail(err.to_string())
            }
        }
    }
}
