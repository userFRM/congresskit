//! End-to-end: serve a manifest + a real parquet shard, then confirm the
//! client fetches, reads, and filters it.

use congresskit::{write_trades, Chamber, Congresskit, Owner, Trade, TxnType};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn trade(member: &str, chamber: Chamber, ticker: &str, ty: TxnType, date: i32) -> Trade {
    Trade {
        filing_date: date,
        doc_id: "20024277".into(),
        chamber,
        member_name: member.into(),
        party: "Republican".into(),
        bioguide_id: "A000372".into(),
        state: "GA".into(),
        district: "12".into(),
        txn_date: date,
        notification_date: date,
        ticker: ticker.into(),
        asset_description: format!("{ticker} [ST]"),
        asset_type: "stock".into(),
        txn_type: ty,
        amount_low: 1001,
        amount_high: 15000,
        owner: Owner::SelfFiler,
        source: "house_clerk".into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn client_reads_served_parquet() {
    let dir = tempfile::TempDir::new().unwrap();
    let shard_path = dir.path().join("congress-2024.parquet");
    let rows = vec![
        trade(
            "Richard Allen",
            Chamber::House,
            "NVDA",
            TxnType::Sale,
            20240201,
        ),
        trade(
            "Richard Allen",
            Chamber::House,
            "NVDA",
            TxnType::Purchase,
            20240105,
        ),
        trade(
            "Jane Senator",
            Chamber::Senate,
            "AAPL",
            TxnType::Sale,
            20240115,
        ),
    ];
    write_trades(&shard_path, &rows).unwrap();
    let parquet = std::fs::read(&shard_path).unwrap();
    let digest = sha256_hex(&parquet);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/manifest.json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!(r#"{{"congress-2024.parquet":"sha256:{digest}"}}"#)),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/congress-2024.parquet"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(parquet))
        .mount(&server)
        .await;

    let cache = tempfile::TempDir::new().unwrap();
    let client = Congresskit::new()
        .with_base_url(server.uri())
        .with_cache_dir(cache.path().to_path_buf())
        .with_mirror_url(None);

    let nvda = client.trades_for("nvda").await.unwrap();
    assert_eq!(nvda.len(), 2, "two NVDA rows");
    assert_eq!(nvda[0].txn_date, 20240201, "sorted most-recent first");

    let buys = client.buys("NVDA").await.unwrap();
    assert_eq!(buys.len(), 1);
    assert_eq!(buys[0].txn_type, TxnType::Purchase);

    let by_member = client.by_member("Allen").await.unwrap();
    assert_eq!(by_member.len(), 2);

    let senate = client.by_chamber(Chamber::Senate).await.unwrap();
    assert_eq!(senate.len(), 1);
    assert_eq!(senate[0].ticker, "AAPL");

    let latest = client.latest(2).await.unwrap();
    assert_eq!(latest.len(), 2);
}
