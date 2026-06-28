# congresskit

US congressional stock trades (STOCK Act periodic transaction reports) for Rust.

```toml
[dependencies]
congresskit = "0.1.0"
```

```rust,no_run
#[tokio::main]
async fn main() -> congresskit::Result<()> {
    for t in congresskit::trades_for("NVDA").await?.iter().take(5) {
        println!("{} {} {} {} ${}-{}", t.txn_date, t.member_name, t.txn_type, t.ticker, t.amount_low, t.amount_high);
    }
    Ok(())
}
```

Full documentation: <https://github.com/userFRM/congresskit>

Licensed under MIT OR Apache-2.0.
