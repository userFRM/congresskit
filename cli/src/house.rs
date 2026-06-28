//! House Clerk ingest: annual financial-disclosure index ZIP -> per-member PTR
//! PDFs -> [`Trade`] rows.
//!
//! Flow:
//! 1. Download `{year}FD.zip` and read the flat `{year}FD.xml` member index.
//! 2. Keep `FilingType == "P"` (periodic transaction report) records.
//! 3. Download each PTR PDF and extract its text.
//! 4. Parse transaction lines out of the text into [`Trade`] rows.
//!
//! Some older PTR PDFs are scanned images with no extractable text. Those are
//! detected (empty / no transaction lines), counted, and skipped — never
//! invented. The caller reports the skipped count.

use std::io::Read;

use anyhow::{Context, Result};
use congresskit::{Owner, Trade, TxnType};

/// One PTR filing referenced by the annual index.
pub struct PtrFiling {
    pub doc_id: String,
    pub last: String,
    pub first: String,
    pub state: String,    // e.g. "GA"
    pub district: String, // e.g. "12" (House)
    pub filing_date: i32,
}

/// Outcome of ingesting one year of House filings.
#[derive(Default)]
pub struct HouseYear {
    pub trades: Vec<Trade>,
    /// PTRs whose PDF had no extractable transaction rows (scanned images).
    pub skipped_scanned: usize,
    /// PTRs whose PDF could not be fetched.
    pub fetch_failed: usize,
    /// Total PTR filings the index listed for the year.
    pub ptr_filings: usize,
}

/// Parse the annual `{year}FD.xml` index out of the downloaded ZIP bytes and
/// return its `FilingType == "P"` records.
pub fn parse_index_zip(zip_bytes: &[u8], year: i32) -> Result<Vec<PtrFiling>> {
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut zip = zip::ZipArchive::new(cursor).context("open annual FD zip")?;
    let mut xml = String::new();
    zip.by_name(&format!("{year}FD.xml"))
        .with_context(|| format!("read {year}FD.xml from zip"))?
        .read_to_string(&mut xml)
        .context("decode FD xml")?;
    Ok(parse_index_xml(&xml))
}

/// Parse the flat `<Member>` list, keeping only periodic transaction reports.
///
/// The XML is a flat sequence of `<Member>` blocks with simple leaf elements,
/// so a tag-scan is enough and avoids an XML-parser dependency.
pub fn parse_index_xml(xml: &str) -> Vec<PtrFiling> {
    let mut out = Vec::new();
    for block in xml.split("<Member>").skip(1) {
        let Some(block) = block.split("</Member>").next() else {
            continue;
        };
        if tag(block, "FilingType") != "P" {
            continue;
        }
        let state_dst = tag(block, "StateDst");
        let (state, district) = split_state_district(&state_dst);
        out.push(PtrFiling {
            doc_id: tag(block, "DocID"),
            last: tag(block, "Last"),
            first: tag(block, "First"),
            state,
            district,
            filing_date: parse_us_date(&tag(block, "FilingDate")),
        });
    }
    out
}

fn tag(block: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    if let Some(start) = block.find(&open) {
        let rest = &block[start + open.len()..];
        if let Some(end) = rest.find(&close) {
            return rest[..end].trim().to_string();
        }
    }
    String::new()
}

/// Split a `StateDst` like `"TX31"` into `("TX", "31")`. An at-large or
/// senate-style entry with no trailing digits yields an empty district.
fn split_state_district(s: &str) -> (String, String) {
    let split = s.find(|c: char| c.is_ascii_digit()).unwrap_or(s.len());
    (s[..split].to_string(), s[split..].to_string())
}

/// The PTR PDF URL for a House doc id.
pub fn ptr_pdf_url(year: i32, doc_id: &str) -> String {
    format!("https://disclosures-clerk.house.gov/public_disc/ptr-pdfs/{year}/{doc_id}.pdf")
}

/// Parse a single PTR PDF's extracted `text` into trade rows for `filing`.
///
/// Returns an empty vec for a scanned image (no transaction lines), which the
/// caller counts as skipped rather than treating as an error.
pub fn parse_ptr_text(text: &str, filing: &PtrFiling) -> Vec<Trade> {
    let member_name = format!("{} {}", filing.first, filing.last)
        .trim()
        .to_string();
    let mut out = Vec::new();
    for block in transaction_blocks(text) {
        if let Some(t) = parse_transaction_block(&block, filing, &member_name) {
            out.push(t);
        }
    }
    out
}

/// Split the extracted text into per-transaction text blocks. A transaction
/// block is a line carrying two `MM/DD/YYYY` dates, plus any immediately
/// following continuation lines that hold a wrapped amount (e.g. `$50,001 -`
/// then `$100,000`).
fn transaction_blocks(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if count_dates(line) >= 2 {
            let mut block = line.to_string();
            // Pull in a wrapped amount continuation: a following line starting
            // with `$` and not itself a new transaction line.
            if let Some(next) = lines.get(i + 1) {
                let n = next.trim();
                if n.starts_with('$') && count_dates(next) < 2 {
                    block.push(' ');
                    block.push_str(n);
                }
            }
            // Pull in a wrapped asset prefix: the previous line if it had no
            // dates and the current line begins with the type/date columns
            // (i.e. it did not start with an owner code or asset text).
            if let Some(prev) = lines.get(i.wrapping_sub(1)) {
                let p = prev.trim();
                if i > 0
                    && count_dates(prev) == 0
                    && starts_with_owner_or_asset(p)
                    && !looks_like_data_start(line)
                {
                    block = format!("{p} {block}");
                }
            }
            blocks.push(block);
        }
    }
    blocks
}

/// `true` if the line begins with an owner code or an asset name (heuristic for
/// a wrapped asset prefix). Owner codes are SP/DC/JT; otherwise any alphabetic
/// start that is not a section label.
fn starts_with_owner_or_asset(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

/// `true` if the dated line itself starts with the data columns rather than an
/// asset name (so no wrapped prefix is needed).
fn looks_like_data_start(line: &str) -> bool {
    let first = line.split_whitespace().next().unwrap_or("");
    matches!(first, "SP" | "DC" | "JT")
}

fn count_dates(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i + 9 < bytes.len() + 1 && i + 10 <= bytes.len() {
        if is_us_date_at(&bytes[i..i + 10]) {
            count += 1;
            i += 10;
        } else {
            i += 1;
        }
    }
    count
}

/// `MM/DD/YYYY` shape check on a 10-byte window.
fn is_us_date_at(w: &[u8]) -> bool {
    w.len() == 10
        && w[0].is_ascii_digit()
        && w[1].is_ascii_digit()
        && w[2] == b'/'
        && w[3].is_ascii_digit()
        && w[4].is_ascii_digit()
        && w[5] == b'/'
        && w[6..10].iter().all(u8::is_ascii_digit)
}

/// Parse one transaction text block into a [`Trade`].
fn parse_transaction_block(block: &str, filing: &PtrFiling, member_name: &str) -> Option<Trade> {
    let dates = extract_dates(block);
    if dates.len() < 2 {
        return None;
    }
    let txn_date = dates[0];
    let notification_date = dates[1];

    // Everything before the first date is "<owner?> <asset> <type>".
    let date_pos = block.find(&us_date_string(txn_date))?;
    let head = block[..date_pos].trim();
    let tail = block[date_pos..].trim(); // dates + amount

    let (owner, asset_and_type) = split_owner(head);
    let (asset_description, txn_type) = split_asset_type(asset_and_type)?;

    let (amount_low, amount_high) = parse_amount(tail);
    let ticker = extract_ticker(&asset_description);
    let asset_type = classify_asset(&asset_description);

    Some(Trade {
        filing_date: filing.filing_date,
        doc_id: filing.doc_id.clone(),
        chamber: congresskit::Chamber::House,
        member_name: member_name.to_string(),
        party: String::new(),
        bioguide_id: String::new(),
        state: filing.state.clone(),
        district: filing.district.clone(),
        txn_date,
        notification_date,
        ticker,
        asset_description,
        asset_type,
        txn_type,
        amount_low,
        amount_high,
        owner,
        source: "house_clerk".to_string(),
    })
}

/// Split a leading owner code (SP/DC/JT) off the head; default to self.
fn split_owner(head: &str) -> (Owner, &str) {
    let mut parts = head.splitn(2, char::is_whitespace);
    match parts.next() {
        Some(tok @ ("SP" | "DC" | "JT")) => {
            (Owner::from_code(tok), parts.next().unwrap_or("").trim())
        }
        _ => (Owner::SelfFiler, head),
    }
}

/// Split the asset+type segment into `(asset_description, txn_type)`.
///
/// The transaction type is the last whitespace token of the segment and is one
/// of `P`, `S`, `S (partial)`, or `E`. A `(partial)` qualifier may trail the
/// `S`. Returns `None` if the trailing token is not a known type.
fn split_asset_type(seg: &str) -> Option<(String, TxnType)> {
    let seg = seg.trim();
    // Handle a trailing "S (partial)".
    let lower = seg.to_ascii_lowercase();
    if let Some(idx) = lower.rfind("s (partial") {
        let asset = seg[..idx].trim().to_string();
        return Some((asset, TxnType::PartialSale));
    }
    let (asset, last) = seg.rsplit_once(char::is_whitespace)?;
    let ty = TxnType::from_code(last)?;
    Some((asset.trim().to_string(), ty))
}

/// Extract the up-to-two `MM/DD/YYYY` dates in order, as `YYYYMMDD` integers.
fn extract_dates(s: &str) -> Vec<i32> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 10 <= bytes.len() {
        if is_us_date_at(&bytes[i..i + 10]) {
            out.push(parse_us_date(&s[i..i + 10]));
            i += 10;
        } else {
            i += 1;
        }
    }
    out
}

fn us_date_string(yyyymmdd: i32) -> String {
    format!(
        "{:02}/{:02}/{:04}",
        (yyyymmdd / 100) % 100,
        yyyymmdd % 100,
        yyyymmdd / 10000
    )
}

/// Parse `M/D/YYYY` or `MM/DD/YYYY` to `i32` `YYYYMMDD`; 0 on failure.
pub fn parse_us_date(s: &str) -> i32 {
    let s = s.trim();
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 3 {
        return 0;
    }
    match (
        parts[0].parse::<i32>(),
        parts[1].parse::<i32>(),
        parts[2].parse::<i32>(),
    ) {
        (Ok(m), Ok(d), Ok(y)) if (1..=12).contains(&m) && (1..=31).contains(&d) => {
            y * 10000 + m * 100 + d
        }
        _ => 0,
    }
}

/// Parse the amount band out of the tail (after the dates), e.g.
/// `"... $1,001 - $15,000"` -> `(1001, 15000)`. A single value yields equal
/// low/high. Returns `(0, 0)` when no `$` amount is present.
fn parse_amount(tail: &str) -> (i64, i64) {
    let dollars: Vec<i64> = tail
        .split('$')
        .skip(1)
        .filter_map(|chunk| {
            let digits: String = chunk
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == ',')
                .collect();
            let cleaned: String = digits.chars().filter(|c| *c != ',').collect();
            cleaned.parse::<i64>().ok()
        })
        .collect();
    match dollars.as_slice() {
        [] => (0, 0),
        [v] => (*v, *v),
        [lo, hi, ..] => (*lo, *hi),
    }
}

/// Extract a ticker from an asset description's trailing `(SYM)`, if any.
fn extract_ticker(asset: &str) -> String {
    // Take the last parenthesised group that looks like a ticker (1-6 uppercase
    // letters, optionally with a dot). Skips qualifiers like "(partial)".
    let mut best = String::new();
    let bytes = asset.as_bytes();
    let mut i = 0;
    while let Some(rel) = asset[i..].find('(') {
        let start = i + rel + 1;
        let Some(rel_end) = asset[start..].find(')') else {
            break;
        };
        let end = start + rel_end;
        let inner = &asset[start..end];
        if is_ticker(inner) {
            best = inner.to_string();
        }
        i = end + 1;
        let _ = bytes; // keep i progressing within bounds
    }
    best
}

fn is_ticker(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty()
        && s.len() <= 6
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '.')
        && s.chars().any(|c| c.is_ascii_uppercase())
}

/// Map the asset-type code in brackets (e.g. `[ST]` stock, `[OP]` option) to
/// `stock` / `option` / `other`.
fn classify_asset(asset: &str) -> String {
    let upper = asset.to_ascii_uppercase();
    if upper.contains("[ST]") || upper.contains("[CS]") {
        "stock"
    } else if upper.contains("[OP]") || upper.contains("OPTION") {
        "option"
    } else {
        "other"
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_state_district() {
        assert_eq!(split_state_district("TX31"), ("TX".into(), "31".into()));
        assert_eq!(split_state_district("GA12"), ("GA".into(), "12".into()));
        assert_eq!(split_state_district("AK00"), ("AK".into(), "00".into()));
    }

    #[test]
    fn parses_amount_band() {
        assert_eq!(
            parse_amount("S 12/21/2023 01/08/2024 $1,001 - $15,000"),
            (1001, 15000)
        );
        assert_eq!(parse_amount("$50,001 - $100,000"), (50001, 100000));
        assert_eq!(parse_amount("no dollars here"), (0, 0));
    }

    #[test]
    fn extracts_ticker() {
        assert_eq!(extract_ticker("Albemarle Corporation (ALB) [ST]"), "ALB");
        assert_eq!(
            extract_ticker("Charles Schwab Corporation (SCHW) [ST]"),
            "SCHW"
        );
        assert_eq!(extract_ticker("Some Muni Bond"), "");
    }

    #[test]
    fn parses_real_ptr_pdf_fixture() {
        // The bundled fixture is a real e-filed House PTR (Hon. Richard W. Allen,
        // GA12). It must yield concrete transaction rows with tickers + amounts.
        let bytes = std::fs::read("../crates/congresskit/tests/fixtures/sample_ptr.pdf").unwrap();
        let text = pdf_extract::extract_text_from_mem(&bytes).unwrap();
        let filing = PtrFiling {
            doc_id: "20024277".into(),
            last: "Allen".into(),
            first: "Richard".into(),
            state: "GA".into(),
            district: "12".into(),
            filing_date: 20240111,
        };
        let trades = parse_ptr_text(&text, &filing);
        assert!(
            trades.len() >= 4,
            "expected >=4 transactions, got {}",
            trades.len()
        );

        let schw = trades
            .iter()
            .find(|t| t.ticker == "SCHW")
            .expect("SCHW row");
        assert_eq!(schw.txn_type, TxnType::Purchase);
        assert_eq!(schw.amount_low, 50001);
        assert_eq!(schw.amount_high, 100000);
        assert_eq!(schw.owner, Owner::Spouse);
        assert_eq!(schw.txn_date, 20231214);
        assert_eq!(schw.notification_date, 20240108);
        assert_eq!(schw.asset_type, "stock");

        let alb = trades.iter().find(|t| t.ticker == "ALB").expect("ALB row");
        assert_eq!(alb.txn_type, TxnType::Sale);
        assert_eq!(alb.amount_low, 1001);
        assert_eq!(alb.amount_high, 15000);
    }

    #[test]
    fn empty_text_yields_no_trades() {
        let filing = PtrFiling {
            doc_id: "x".into(),
            last: "Doe".into(),
            first: "Jane".into(),
            state: "CA".into(),
            district: "1".into(),
            filing_date: 0,
        };
        assert!(parse_ptr_text("", &filing).is_empty());
        assert!(parse_ptr_text("scanned image with no dates", &filing).is_empty());
    }

    #[test]
    fn splits_asset_and_type() {
        let (asset, ty) = split_asset_type("Albemarle Corporation (ALB) [ST] S").unwrap();
        assert_eq!(asset, "Albemarle Corporation (ALB) [ST]");
        assert_eq!(ty, TxnType::Sale);
        let (_, ty) = split_asset_type("Foo (BAR) [ST] P").unwrap();
        assert_eq!(ty, TxnType::Purchase);
        let (_, ty) = split_asset_type("Foo (BAR) [ST] S (partial)").unwrap();
        assert_eq!(ty, TxnType::PartialSale);
    }
}
