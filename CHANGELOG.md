<!-- Canonical CHANGELOG header for every *kit. The body keeps each kit's real
release history; only this top block is standardized. -->
# Changelog

All notable changes to congresskit are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0]

Initial release.

- Async `Congresskit` client plus blocking siblings and one-shot free functions.
- Query surface: `trades_for`, `by_member`, `latest`, `buys`, `sells`, `by_chamber`.
- Each trade carries `party` and a stable `bioguide_id` joined from the public-domain unitedstates/congress-legislators roster, so a member's trades link across years and chambers.
- Bundled per-year parquet (`data/year=YYYY/congress-YYYY.parquet`) served from GitHub raw with on-demand fetch, ETag revalidation, SHA-256 manifest verification, and a CDN mirror plus stale-cache fallback.
- `congresskit-cli` with `backfill`, `nightly-append`, `manifest`, and `query`.
- House Clerk ingest is fully automated. Senate EFD ingest is implemented but requires a non-datacenter IP; it degrades gracefully when the host blocks the request.
