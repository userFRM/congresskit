//! `congresskit` — US congressional stock trades (STOCK Act periodic
//! transaction reports) for Rust.
//!
//! Fetches year-partitioned parquet files on demand from GitHub raw, caches
//! them locally with ETag revalidation, and falls back to stale cache on
//! network errors. No API keys. Offline after the first successful fetch of
//! each year file.
//!
//! Data comes from US public-record disclosures: House trades from the US House
//! Clerk, Senate trades from the US Senate EFD, and member party + identity
//! from the public-domain unitedstates/congress-legislators dataset. Each row
//! is one disclosed transaction; dates are `i32` `YYYYMMDD`.
//!
//! # Quick start — free functions
//!
//! ```no_run
//! use congresskit::trades_for;
//!
//! #[tokio::main]
//! async fn main() -> congresskit::Result<()> {
//!     for t in trades_for("NVDA").await?.iter().take(5) {
//!         println!("{} {} {} {} ${}-{}", t.txn_date, t.member_name, t.txn_type.as_str(), t.ticker, t.amount_low, t.amount_high);
//!     }
//!     Ok(())
//! }
//! ```
//!
//! For connection-pool reuse across many lookups, create a [`Congresskit`]
//! client once and call its methods instead of the free functions.
//!
//! # Environment overrides
//!
//! | Variable | Effect |
//! |---|---|
//! | `CONGRESSKIT_BASE_URL` | Replace the GitHub raw origin URL |
//! | `CONGRESSKIT_CACHE_DIR` | Override `~/.cache/congresskit/` |
//! | `CONGRESSKIT_MIRROR_URL` | Override the jsDelivr CDN mirror |
#![forbid(unsafe_code)]

mod error;
pub use error::{Error, Result};

mod record;
pub use record::{Chamber, Owner, Trade, TxnType};

pub mod parquet_io;
pub use parquet_io::{read_trades, write_trades};

mod fetcher;

mod client;
pub use client::{by_member, latest, trades_for, Congresskit};
