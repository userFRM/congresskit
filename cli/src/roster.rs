//! Member roster enrichment from the public-domain unitedstates/congress-legislators
//! dataset.
//!
//! Joins each disclosed trade's (last, first, state, district, chamber) to a
//! stable politician identity so a member's trades link across years and
//! chambers. Adds `party`, `bioguide_id`, and the roster's official full name.
//!
//! Matching is conservative: last name + state (+ district for the House),
//! case-insensitive, with first-name disambiguation. An unmatched trade keeps
//! its filing name and gets empty `party` / `bioguide_id`; the caller reports
//! the unmatched rate rather than guessing party.

use std::collections::HashMap;

use anyhow::{Context, Result};
use congresskit::{Chamber, Trade};
use serde::Deserialize;

const CURRENT_URL: &str =
    "https://unitedstates.github.io/congress-legislators/legislators-current.json";
const HISTORICAL_URL: &str =
    "https://unitedstates.github.io/congress-legislators/legislators-historical.json";

#[derive(Deserialize)]
struct Legislator {
    id: Ids,
    name: Name,
    terms: Vec<Term>,
}

#[derive(Deserialize)]
struct Ids {
    bioguide: Option<String>,
}

#[derive(Deserialize)]
struct Name {
    first: Option<String>,
    last: Option<String>,
    official_full: Option<String>,
}

#[derive(Deserialize)]
struct Term {
    #[serde(rename = "type")]
    kind: Option<String>, // "sen" | "rep"
    state: Option<String>,
    district: Option<i64>,
    party: Option<String>,
}

/// A roster entry: one chamber-specific service record for a legislator.
struct Entry {
    bioguide: String,
    official_full: String,
    first: String,
    party: String,
    chamber: Chamber,
    district: Option<i64>,
}

/// Index of roster entries keyed by `(lastname_lower, state_upper)`.
#[derive(Default)]
pub struct Roster {
    by_last_state: HashMap<(String, String), Vec<Entry>>,
}

impl Roster {
    /// Fetch both the current and historical rosters and build the index.
    pub async fn fetch(client: &reqwest::Client) -> Result<Roster> {
        let mut roster = Roster::default();
        for url in [CURRENT_URL, HISTORICAL_URL] {
            let bytes = client
                .get(url)
                .send()
                .await
                .with_context(|| format!("fetch roster {url}"))?
                .error_for_status()
                .with_context(|| format!("roster {url} status"))?
                .bytes()
                .await
                .context("read roster body")?;
            let legislators: Vec<Legislator> =
                serde_json::from_slice(&bytes).with_context(|| format!("parse roster {url}"))?;
            roster.ingest(legislators);
        }
        Ok(roster)
    }

    fn ingest(&mut self, legislators: Vec<Legislator>) {
        for leg in legislators {
            let bioguide = leg.id.bioguide.unwrap_or_default();
            if bioguide.is_empty() {
                continue;
            }
            let last = leg.name.last.clone().unwrap_or_default();
            let first = leg.name.first.clone().unwrap_or_default();
            let official_full = leg
                .name
                .official_full
                .clone()
                .unwrap_or_else(|| format!("{first} {last}").trim().to_string());
            // One entry per distinct (state, chamber, district, party) the
            // member served, so a join in any year/chamber resolves.
            let mut seen = std::collections::HashSet::new();
            for term in &leg.terms {
                let Some(state) = term.state.as_deref() else {
                    continue;
                };
                let chamber = match term.kind.as_deref() {
                    Some("sen") => Chamber::Senate,
                    Some("rep") => Chamber::House,
                    _ => continue,
                };
                let key = (
                    state.to_uppercase(),
                    chamber,
                    term.district,
                    term.party.clone().unwrap_or_default(),
                );
                if !seen.insert(key) {
                    continue;
                }
                self.by_last_state
                    .entry((last.to_lowercase(), state.to_uppercase()))
                    .or_default()
                    .push(Entry {
                        bioguide: bioguide.clone(),
                        official_full: official_full.clone(),
                        first: first.clone(),
                        party: canonical_party(term.party.as_deref().unwrap_or("")),
                        chamber,
                        district: term.district,
                    });
            }
        }
    }

    /// Match one trade. Returns `(official_full, party, bioguide_id)` on a hit.
    fn lookup(
        &self,
        last: &str,
        first: &str,
        state: &str,
        district: &str,
        chamber: Chamber,
    ) -> Option<(String, String, String)> {
        let candidates = self
            .by_last_state
            .get(&(last.to_lowercase(), state.to_uppercase()))?;

        // Filter to the right chamber, then by district (House), then by first
        // name. Each filter only narrows if it leaves at least one candidate.
        let chamber_hits: Vec<&Entry> =
            candidates.iter().filter(|e| e.chamber == chamber).collect();
        let pool = if chamber_hits.is_empty() {
            candidates.iter().collect::<Vec<_>>()
        } else {
            chamber_hits
        };

        let pool = narrow_by_district(pool, district, chamber);
        let pool = narrow_by_first(pool, first);

        // Unique party + bioguide across the surviving pool means a confident
        // match even if multiple term-entries remain.
        let first_entry = pool.first()?;
        let unique = pool
            .iter()
            .all(|e| e.bioguide == first_entry.bioguide && e.party == first_entry.party);
        if !unique {
            return None;
        }
        Some((
            first_entry.official_full.clone(),
            first_entry.party.clone(),
            first_entry.bioguide.clone(),
        ))
    }

    /// Enrich `trades` in place. Returns the count of unmatched trades.
    pub fn enrich(
        &self,
        trades: &mut [Trade],
        house_filing_names: &HashMap<String, (String, String)>,
    ) -> usize {
        let mut unmatched = 0;
        for t in trades.iter_mut() {
            // House trades carry first/last via the filing-name map keyed by
            // doc_id; Senate trades carry only member_name + state.
            let (last, first) = house_filing_names
                .get(&t.doc_id)
                .cloned()
                .unwrap_or_else(|| split_name(&t.member_name));
            match self.lookup(&last, &first, &t.state, &t.district, t.chamber) {
                Some((official, party, bioguide)) => {
                    t.member_name = official;
                    t.party = party;
                    t.bioguide_id = bioguide;
                }
                None => unmatched += 1,
            }
        }
        unmatched
    }
}

fn narrow_by_district<'a>(
    pool: Vec<&'a Entry>,
    district: &str,
    chamber: Chamber,
) -> Vec<&'a Entry> {
    if chamber != Chamber::House || district.is_empty() {
        return pool;
    }
    let want: i64 = district.parse().unwrap_or(-1);
    let hits: Vec<&Entry> = pool
        .iter()
        .copied()
        .filter(|e| e.district == Some(want) || (want == 0 && e.district.is_none()))
        .collect();
    if hits.is_empty() {
        pool
    } else {
        hits
    }
}

fn narrow_by_first<'a>(pool: Vec<&'a Entry>, first: &str) -> Vec<&'a Entry> {
    let needle = first_token(first).to_lowercase();
    if needle.is_empty() {
        return pool;
    }
    let hits: Vec<&Entry> = pool
        .iter()
        .copied()
        .filter(|e| first_token(&e.first).to_lowercase() == needle)
        .collect();
    if hits.is_empty() {
        pool
    } else {
        hits
    }
}

/// Map roster party text to the canonical `Democrat | Republican | Independent`.
fn canonical_party(p: &str) -> String {
    match p.trim() {
        "Democrat" | "Democratic" => "Democrat",
        "Republican" => "Republican",
        "" => "",
        // Independents and minor caucuses (Independent, Libertarian, etc.).
        _ => "Independent",
    }
    .to_string()
}

/// First whitespace token of a name, ignoring a leading honorific.
fn first_token(name: &str) -> String {
    name.split_whitespace()
        .find(|t| !is_honorific(t))
        .unwrap_or("")
        .trim_end_matches('.')
        .to_string()
}

fn is_honorific(t: &str) -> bool {
    matches!(
        t.trim_end_matches('.').to_ascii_lowercase().as_str(),
        "hon" | "mr" | "mrs" | "ms" | "dr"
    )
}

/// Split a "First Last" display name into (last, first) by best effort.
fn split_name(name: &str) -> (String, String) {
    let toks: Vec<&str> = name
        .split_whitespace()
        .filter(|t| !is_honorific(t))
        .collect();
    match toks.as_slice() {
        [] => (String::new(), String::new()),
        [one] => (one.to_string(), String::new()),
        [first, .., last] => (last.to_string(), first.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn party_canonicalises() {
        assert_eq!(canonical_party("Democrat"), "Democrat");
        assert_eq!(canonical_party("Republican"), "Republican");
        assert_eq!(canonical_party("Independent"), "Independent");
        assert_eq!(canonical_party("Libertarian"), "Independent");
        assert_eq!(canonical_party(""), "");
    }

    #[test]
    fn first_token_skips_honorific() {
        assert_eq!(first_token("Hon. Richard W. Allen"), "Richard");
        assert_eq!(first_token("Nancy"), "Nancy");
    }

    #[test]
    fn splits_display_name() {
        assert_eq!(
            split_name("Nancy Pelosi"),
            ("Pelosi".into(), "Nancy".into())
        );
        assert_eq!(
            split_name("Hon. Richard W. Allen"),
            ("Allen".into(), "Richard".into())
        );
    }
}
