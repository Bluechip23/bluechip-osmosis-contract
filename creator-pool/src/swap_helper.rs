//! Swap-math re-exports. The pure AMM math that used to live in this file
//! (`compute_swap`, `compute_offer_amount`, `assert_max_spread`,
//! `update_price_accumulator`, `DEFAULT_SLIPPAGE`) lives in
//! `pool_core::swap` and is re-exported below so existing imports like
//! `use crate::swap_helper::compute_swap;` keep resolving.
pub use pool_core::swap::*;
