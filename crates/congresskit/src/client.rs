//! Stateful `Congresskit` client — async congressional-trade endpoints with
//! blocking wrappers.
//!
//! Fetches year-partitioned parquet shards from GitHub raw (or a configurable
//! origin) with ETag-aware caching, SHA-256 manifest verification, and CDN
//! mirror fallback. Falls back to stale cache on transient network failures.
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
//! # Client pattern (reuse across calls)
//!
//! ```no_run
//! use congresskit::Congresskit;
//!
//! #[tokio::main]
//! async fn main() -> congresskit::Result<()> {
//!     let client = Congresskit::new();
//!     let buys = client.buys("AAPL").await?;
//!     println!("{} disclosed purchases", buys.len());
//!     Ok(())
//! }
//! ```

use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::fetcher::{default_cache_dir, resolved_base_url, CachedFetcher};
use crate::parquet_io::read_trades;
use crate::record::{Chamber, Trade};

/// Stateful congresskit client.
///
/// Wraps an ETag-aware cached fetcher and exposes flat async query methods.
/// Create once and reuse; the internal reqwest client is kept alive for
/// connection pooling.
///
/// ```no_run
/// use congresskit::Congresskit;
/// use std::path::PathBuf;
///
/// let client = Congresskit::new()
///     .with_base_url("https://my-mirror.example.com/congresskit")
///     .with_cache_dir(PathBuf::from("/tmp/congresskit-test"));
/// ```
#[derive(Clone)]
pub struct Congresskit {
    fetcher: CachedFetcher,
}

impl Congresskit {
    /// Create a client with the default GitHub raw backend and XDG cache.
    ///
    /// Reads `CONGRESSKIT_BASE_URL` and `CONGRESSKIT_CACHE_DIR` from the
    /// environment if set. **This function never fails.** Errors are deferred
    /// to the first fetch.
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent("congresskit/0.1 (+https://github.com/userFRM/congresskit)")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            fetcher: CachedFetcher::new(http, resolved_base_url(), default_cache_dir()),
        }
    }

    /// Override the origin URL.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.fetcher.set_base_url(url.into());
        self
    }

    /// Override the on-disk cache directory.
    pub fn with_cache_dir(mut self, dir: PathBuf) -> Self {
        self.fetcher.set_cache_dir(dir);
        self
    }

    /// Override the CDN mirror URL. `None` disables mirror fallback.
    pub fn with_mirror_url(mut self, url: Option<String>) -> Self {
        self.fetcher.set_mirror_url(url);
        self
    }

    // ── Async query endpoints ───────────────────────────────────────────────

    /// All trades in a `ticker` (case-insensitive), most recent transaction
    /// date first.
    pub async fn trades_for(&self, ticker: &str) -> Result<Vec<Trade>> {
        let rows = self.load_all_rows().await?;
        Ok(sort_desc(
            rows.into_iter()
                .filter(|r| r.ticker.eq_ignore_ascii_case(ticker))
                .collect(),
        ))
    }

    /// All trades by a member, matched by exact `bioguide_id` if `query` looks
    /// like one (a letter followed by digits), otherwise by case-insensitive
    /// substring of the member name. Most recent first.
    pub async fn by_member(&self, query: &str) -> Result<Vec<Trade>> {
        let rows = self.load_all_rows().await?;
        let matched: Vec<Trade> = if looks_like_bioguide(query) {
            let id = query.to_uppercase();
            rows.into_iter().filter(|r| r.bioguide_id == id).collect()
        } else {
            let needle = query.to_lowercase();
            rows.into_iter()
                .filter(|r| r.member_name.to_lowercase().contains(&needle))
                .collect()
        };
        Ok(sort_desc(matched))
    }

    /// The `n` most recent trades across all members, by transaction date.
    pub async fn latest(&self, n: usize) -> Result<Vec<Trade>> {
        let mut rows = self.load_all_rows().await?;
        rows.sort_by_key(|r| std::cmp::Reverse(r.txn_date));
        rows.truncate(n);
        Ok(rows)
    }

    /// Purchases in a `ticker`, most recent first.
    pub async fn buys(&self, ticker: &str) -> Result<Vec<Trade>> {
        Ok(self
            .trades_for(ticker)
            .await?
            .into_iter()
            .filter(Trade::is_buy)
            .collect())
    }

    /// Full and partial sales in a `ticker`, most recent first.
    pub async fn sells(&self, ticker: &str) -> Result<Vec<Trade>> {
        Ok(self
            .trades_for(ticker)
            .await?
            .into_iter()
            .filter(Trade::is_sell)
            .collect())
    }

    /// All trades filed by one chamber, most recent transaction date first.
    pub async fn by_chamber(&self, chamber: Chamber) -> Result<Vec<Trade>> {
        let rows = self.load_all_rows().await?;
        Ok(sort_desc(
            rows.into_iter().filter(|r| r.chamber == chamber).collect(),
        ))
    }

    // ── Blocking wrappers ───────────────────────────────────────────────────

    /// Blocking variant of [`trades_for`](Self::trades_for).
    pub fn trades_for_blocking(&self, ticker: &str) -> Result<Vec<Trade>> {
        let c = self.clone();
        let t = ticker.to_owned();
        block(async move { c.trades_for(&t).await })
    }

    /// Blocking variant of [`by_member`](Self::by_member).
    pub fn by_member_blocking(&self, query: &str) -> Result<Vec<Trade>> {
        let c = self.clone();
        let q = query.to_owned();
        block(async move { c.by_member(&q).await })
    }

    /// Blocking variant of [`latest`](Self::latest).
    pub fn latest_blocking(&self, n: usize) -> Result<Vec<Trade>> {
        let c = self.clone();
        block(async move { c.latest(n).await })
    }

    // ── Internal ─────────────────────────────────────────────────────────────

    /// Fetch every `congress-YYYY.parquet` shard listed in the manifest and
    /// flat-concatenate the rows.
    pub(crate) async fn load_all_rows(&self) -> Result<Vec<Trade>> {
        let keys = self.discover_shards().await?;
        let mut all = Vec::new();
        for key in keys {
            let bytes = self.fetcher.fetch(&key).await?;
            all.extend(read_trades(&bytes)?);
        }
        Ok(all)
    }

    /// Fetch `manifest.json` and return sorted shard keys (without `.parquet`).
    async fn discover_shards(&self) -> Result<Vec<String>> {
        let url = format!("{}/manifest.json", self.fetcher.base_url);
        let resp = self
            .fetcher
            .http
            .get(&url)
            .send()
            .await
            .map_err(Error::Http)?;
        if !resp.status().is_success() {
            return Err(Error::Other(format!(
                "manifest.json: HTTP {} {}",
                resp.status().as_u16(),
                resp.status().canonical_reason().unwrap_or("")
            )));
        }
        let manifest: serde_json::Value = resp.json().await.map_err(Error::Http)?;
        let obj = manifest
            .as_object()
            .ok_or_else(|| Error::Other("manifest.json is not a JSON object".into()))?;
        let mut keys: Vec<String> = obj
            .keys()
            .filter(|k| is_congress_shard(k))
            .map(|k| k.trim_end_matches(".parquet").to_string())
            .collect();
        keys.sort();
        Ok(keys)
    }
}

impl Default for Congresskit {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sort_desc(mut rows: Vec<Trade>) -> Vec<Trade> {
    rows.sort_by_key(|r| std::cmp::Reverse(r.txn_date));
    rows
}

/// A bioguide id is one ASCII letter followed by digits (e.g. `A000372`).
fn looks_like_bioguide(s: &str) -> bool {
    let mut bytes = s.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_alphabetic())
        && s.len() > 1
        && bytes.all(|b| b.is_ascii_digit())
}

/// Return `true` for filenames matching `congress-YYYY.parquet`.
fn is_congress_shard(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("congress-") else {
        return false;
    };
    let Some(year) = rest.strip_suffix(".parquet") else {
        return false;
    };
    !year.is_empty() && year.bytes().all(|b| b.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// All trades in `ticker` using a temporary one-shot client.
pub async fn trades_for(ticker: &str) -> Result<Vec<Trade>> {
    Congresskit::new().trades_for(ticker).await
}

/// All trades by a member (name substring or bioguide id), one-shot client.
pub async fn by_member(query: &str) -> Result<Vec<Trade>> {
    Congresskit::new().by_member(query).await
}

/// The `n` most recent trades across all members, one-shot client.
pub async fn latest(n: usize) -> Result<Vec<Trade>> {
    Congresskit::new().latest(n).await
}

// ---------------------------------------------------------------------------
// Blocking helper
// ---------------------------------------------------------------------------

/// Drive a future to completion from any context (sync or async).
///
/// - Inside a tokio **multi-thread** runtime: `block_in_place` + `block_on`.
/// - Inside a **current-thread** runtime or no runtime: the future is driven on
///   a dedicated OS thread with its own runtime so the caller is not re-entered.
pub(crate) fn block<F, T>(fut: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| handle.block_on(fut))
        }
        _ => std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(Error::Io)
                .and_then(|rt| rt.block_on(fut))
        })
        .join()
        .expect("blocking thread panicked"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_matches_year_files_only() {
        assert!(is_congress_shard("congress-2024.parquet"));
        assert!(!is_congress_shard("manifest.json"));
        assert!(!is_congress_shard("congress-.parquet"));
        assert!(!is_congress_shard("insider-2024.parquet"));
    }

    #[test]
    fn bioguide_detection() {
        assert!(looks_like_bioguide("A000372"));
        assert!(looks_like_bioguide("P000197"));
        assert!(!looks_like_bioguide("Pelosi"));
        assert!(!looks_like_bioguide("123456"));
        assert!(!looks_like_bioguide("A"));
    }
}
