use cosmwasm_std::{OverflowError, StdError, Timestamp};
use semver::Error as SemVerError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),
    #[error("Query error: {msg}")]
    QueryError { msg: String },
    #[error("Unauthorized")]
    Unauthorized {},

    #[error("You are missing important times and prices")]
    InsufficientData {},

    #[error("Contract Address Can Not Be Found")]
    ContractAddressNotFound {},

    #[error("Contract Failed Creating {}", id)]
    UnknownReplyId { id: u64 },

    #[error("SemVer parse error: {0}")]
    SemVer(#[from] SemVerError),
    #[error("Update is not yet effective. Can be applied after {effective_after}")]
    TimelockNotExpired { effective_after: Timestamp },

    // ---------------------------------------------------------------------
    // Pool-creation reply chain errors.
    // ---------------------------------------------------------------------
    #[error("Pool reply '{step}' missing address: {kind}")]
    ReplyMissingAddress {
        step: &'static str,
        kind: &'static str,
    },

    #[error("Threshold payout corruption detected: components do not match factory config")]
    ThresholdPayoutCorruption,

    #[error("Reply for SubMsg id={id} returned an error in a reply_on_success path: {msg}")]
    ReplyOnSuccessSawError { id: u64, msg: String },

    #[error("Invalid pair shape: {reason}")]
    InvalidPairShape { reason: String },

    #[error(
        "Duplicate pair: pool_id {existing_pool_id} is already registered for ({asset_a}, {asset_b})"
    )]
    DuplicatePair {
        existing_pool_id: u64,
        asset_a: String,
        asset_b: String,
    },

    // ---------------------------------------------------------------------
    // Migration / config errors.
    // ---------------------------------------------------------------------
    #[error("Downgrade refused: stored {stored}, current {current}")]
    DowngradeRefused { stored: String, current: String },
}

impl From<OverflowError> for ContractError {
    fn from(o: OverflowError) -> Self {
        StdError::from(o).into()
    }
}
