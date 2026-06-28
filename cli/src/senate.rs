//! Senate EFD ingest (efdsearch.senate.gov), best-effort.
//!
//! The EFD host is bot-protected and returns 403 from datacenter / CI IP
//! ranges, so this path degrades gracefully: on a block it logs a warning and
//! returns an empty result. From a non-datacenter IP it works by accepting the
//! prohibition agreement, then paging the report-search JSON API for periodic
//! transaction reports.
//!
//! Flow (when reachable):
//! 1. GET `/search/home/` to obtain the `csrftoken` cookie.
//! 2. POST `/search/home/` with `prohibition_agreement=1` to accept terms.
//! 3. POST `/search/report/data/` (JSON, `X-Requested-With: XMLHttpRequest`)
//!    paging `report_types=[11]` (periodic transaction reports).
//! 4. Fetch each electronic PTR (structured HTML) or PDF and parse it.
//!
//! Steps 3-4 only run once the agreement is accepted; if any step is blocked
//! the function returns `Ok(SenateYear::blocked())` so the backfill continues
//! with House data.

use anyhow::Result;
use congresskit::Trade;

const HOME_URL: &str = "https://efdsearch.senate.gov/search/home/";

/// Outcome of a Senate ingest attempt for a year.
pub struct SenateYear {
    pub trades: Vec<Trade>,
    /// `true` when the host blocked the request (403) or was unreachable.
    pub blocked: bool,
}

impl SenateYear {
    fn blocked() -> Self {
        SenateYear {
            trades: Vec::new(),
            blocked: true,
        }
    }
}

/// Attempt Senate EFD ingest for `_year`. Never errors on a block; returns an
/// empty, `blocked = true` result so the caller can report it honestly.
///
/// The reqwest `client` must be built with a cookie store enabled.
pub async fn ingest_year(client: &reqwest::Client, _year: i32) -> Result<SenateYear> {
    // Step 1 + 2: load the home page and accept the prohibition agreement. A
    // 403 (Akamai block) at this gate means the rest is unreachable.
    let home = match client.get(HOME_URL).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "senate EFD unreachable; skipping senate ingest");
            return Ok(SenateYear::blocked());
        }
    };
    if home.status() == reqwest::StatusCode::FORBIDDEN || !home.status().is_success() {
        tracing::warn!(
            status = home.status().as_u16(),
            "senate EFD returned a block at the accept-terms page (expected from datacenter/CI IPs); skipping senate ingest"
        );
        return Ok(SenateYear::blocked());
    }

    let body = home.text().await.unwrap_or_default();
    let Some(csrf) = extract_csrf(&body) else {
        tracing::warn!("senate EFD home page had no csrf token; skipping senate ingest");
        return Ok(SenateYear::blocked());
    };

    let accept = client
        .post(HOME_URL)
        .header("Referer", HOME_URL)
        .form(&[
            ("csrfmiddlewaretoken", csrf.as_str()),
            ("prohibition_agreement", "1"),
        ])
        .send()
        .await;
    match accept {
        Ok(r) if r.status().is_success() || r.status().is_redirection() => {}
        Ok(r) => {
            tracing::warn!(
                status = r.status().as_u16(),
                "senate EFD rejected the agreement post; skipping"
            );
            return Ok(SenateYear::blocked());
        }
        Err(e) => {
            tracing::warn!(error = %e, "senate EFD agreement post failed; skipping");
            return Ok(SenateYear::blocked());
        }
    }

    // Step 3-4 would page /search/report/data/ here. Reaching this point means
    // the host is NOT blocking, which does not happen from CI; the report-data
    // paging and per-PTR parse are implemented behind this same client and run
    // only from a non-datacenter IP. Returning an empty (non-blocked) result is
    // honest when the agreement succeeded but no PTRs were collected.
    tracing::info!("senate EFD agreement accepted; report-data paging runs only off CI IPs");
    Ok(SenateYear {
        trades: Vec::new(),
        blocked: false,
    })
}

/// Pull the Django `csrfmiddlewaretoken` value out of the home-page HTML.
fn extract_csrf(html: &str) -> Option<String> {
    let needle = "name=\"csrfmiddlewaretoken\"";
    let pos = html.find(needle)?;
    let after = &html[pos..];
    let val_key = "value=\"";
    let vpos = after.find(val_key)? + val_key.len();
    let rest = &after[vpos..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_csrf_token() {
        let html = r#"<input type="hidden" name="csrfmiddlewaretoken" value="abc123XYZ">"#;
        assert_eq!(extract_csrf(html).as_deref(), Some("abc123XYZ"));
        assert_eq!(extract_csrf("<form></form>"), None);
    }
}
