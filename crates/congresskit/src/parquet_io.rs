//! Parquet reader/writer for congressional-trade rows.
//!
//! # File layout
//!
//! One row per transaction. Columns, in order:
//!
//! ```text
//! filing_date Int32(YYYYMMDD), doc_id Utf8, chamber Utf8(house|senate),
//! member_name Utf8, party Utf8, bioguide_id Utf8, state Utf8, district Utf8,
//! txn_date Int32(YYYYMMDD), notification_date Int32(YYYYMMDD), ticker Utf8,
//! asset_description Utf8, asset_type Utf8, txn_type Utf8, amount_low Int64,
//! amount_high Int64, owner Utf8, source Utf8
//! ```
//!
//! Dates are plain `i32` `YYYYMMDD` integers, not Arrow `Date32`, so a consumer
//! never needs a calendar library to compare or bucket them.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{Array, Int32Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use crate::error::{Error, Result};
use crate::record::{Chamber, Owner, Trade, TxnType};

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// The bundled-parquet schema, bound field by field. Every column non-null;
/// the writer fills empty strings rather than nulls so the read path can reject
/// any unexpected null as corruption.
fn trade_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("filing_date", DataType::Int32, false),
        Field::new("doc_id", DataType::Utf8, false),
        Field::new("chamber", DataType::Utf8, false),
        Field::new("member_name", DataType::Utf8, false),
        Field::new("party", DataType::Utf8, false),
        Field::new("bioguide_id", DataType::Utf8, false),
        Field::new("state", DataType::Utf8, false),
        Field::new("district", DataType::Utf8, false),
        Field::new("txn_date", DataType::Int32, false),
        Field::new("notification_date", DataType::Int32, false),
        Field::new("ticker", DataType::Utf8, false),
        Field::new("asset_description", DataType::Utf8, false),
        Field::new("asset_type", DataType::Utf8, false),
        Field::new("txn_type", DataType::Utf8, false),
        Field::new("amount_low", DataType::Int64, false),
        Field::new("amount_high", DataType::Int64, false),
        Field::new("owner", DataType::Utf8, false),
        Field::new("source", DataType::Utf8, false),
    ]))
}

fn writer_props() -> WriterProperties {
    WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(3).expect("valid zstd level"),
        ))
        .set_max_row_group_row_count(Some(50_000))
        .build()
}

// ---------------------------------------------------------------------------
// Write
// ---------------------------------------------------------------------------

/// Write `rows` to a parquet file at `path` (creates or overwrites).
pub fn write_trades(path: &Path, rows: &[Trade]) -> Result<()> {
    let schema = trade_schema();
    let file = fs::File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, schema.clone(), Some(writer_props()))?;
    for chunk in rows.chunks(50_000) {
        writer.write(&batch_of(&schema, chunk)?)?;
    }
    writer.close()?;
    Ok(())
}

fn batch_of(schema: &Arc<Schema>, rows: &[Trade]) -> Result<RecordBatch> {
    let filing_date: Int32Array = rows.iter().map(|r| Some(r.filing_date)).collect();
    let doc_id: StringArray = rows.iter().map(|r| Some(r.doc_id.as_str())).collect();
    let chamber: StringArray = rows.iter().map(|r| Some(r.chamber.as_str())).collect();
    let member_name: StringArray = rows.iter().map(|r| Some(r.member_name.as_str())).collect();
    let party: StringArray = rows.iter().map(|r| Some(r.party.as_str())).collect();
    let bioguide_id: StringArray = rows.iter().map(|r| Some(r.bioguide_id.as_str())).collect();
    let state: StringArray = rows.iter().map(|r| Some(r.state.as_str())).collect();
    let district: StringArray = rows.iter().map(|r| Some(r.district.as_str())).collect();
    let txn_date: Int32Array = rows.iter().map(|r| Some(r.txn_date)).collect();
    let notification_date: Int32Array = rows.iter().map(|r| Some(r.notification_date)).collect();
    let ticker: StringArray = rows.iter().map(|r| Some(r.ticker.as_str())).collect();
    let asset_description: StringArray = rows
        .iter()
        .map(|r| Some(r.asset_description.as_str()))
        .collect();
    let asset_type: StringArray = rows.iter().map(|r| Some(r.asset_type.as_str())).collect();
    let txn_type: StringArray = rows.iter().map(|r| Some(r.txn_type.as_str())).collect();
    let amount_low: Int64Array = rows.iter().map(|r| Some(r.amount_low)).collect();
    let amount_high: Int64Array = rows.iter().map(|r| Some(r.amount_high)).collect();
    let owner: StringArray = rows.iter().map(|r| Some(r.owner.as_str())).collect();
    let source: StringArray = rows.iter().map(|r| Some(r.source.as_str())).collect();

    RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(filing_date),
            Arc::new(doc_id),
            Arc::new(chamber),
            Arc::new(member_name),
            Arc::new(party),
            Arc::new(bioguide_id),
            Arc::new(state),
            Arc::new(district),
            Arc::new(txn_date),
            Arc::new(notification_date),
            Arc::new(ticker),
            Arc::new(asset_description),
            Arc::new(asset_type),
            Arc::new(txn_type),
            Arc::new(amount_low),
            Arc::new(amount_high),
            Arc::new(owner),
            Arc::new(source),
        ],
    )
    .map_err(Error::Arrow)
}

// ---------------------------------------------------------------------------
// Read
// ---------------------------------------------------------------------------

fn column_as<'a, A: Array + 'static>(batch: &'a RecordBatch, name: &str) -> Result<&'a A> {
    let idx = batch
        .schema()
        .index_of(name)
        .map_err(|_| Error::Parquet(format!("missing column: {name}")))?;
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<A>()
        .ok_or_else(|| Error::Parquet(format!("{name} column type mismatch")))
}

#[inline]
fn require_non_null(col: &dyn Array, field: &str, i: usize) -> Result<()> {
    if col.is_null(i) {
        Err(Error::Parquet(format!("null {field} at row {i}")))
    } else {
        Ok(())
    }
}

/// Parse a parquet file (in-memory bytes) into [`Trade`] records.
pub fn read_trades(bytes: &[u8]) -> Result<Vec<Trade>> {
    let owned: bytes::Bytes = bytes::Bytes::copy_from_slice(bytes);
    let reader = ParquetRecordBatchReaderBuilder::try_new(owned)?.build()?;

    let mut rows = Vec::new();
    for batch in reader {
        let batch = batch?;
        let filing_date = column_as::<Int32Array>(&batch, "filing_date")?;
        let doc_id = column_as::<StringArray>(&batch, "doc_id")?;
        let chamber = column_as::<StringArray>(&batch, "chamber")?;
        let member_name = column_as::<StringArray>(&batch, "member_name")?;
        let party = column_as::<StringArray>(&batch, "party")?;
        let bioguide_id = column_as::<StringArray>(&batch, "bioguide_id")?;
        let state = column_as::<StringArray>(&batch, "state")?;
        let district = column_as::<StringArray>(&batch, "district")?;
        let txn_date = column_as::<Int32Array>(&batch, "txn_date")?;
        let notification_date = column_as::<Int32Array>(&batch, "notification_date")?;
        let ticker = column_as::<StringArray>(&batch, "ticker")?;
        let asset_description = column_as::<StringArray>(&batch, "asset_description")?;
        let asset_type = column_as::<StringArray>(&batch, "asset_type")?;
        let txn_type = column_as::<StringArray>(&batch, "txn_type")?;
        let amount_low = column_as::<Int64Array>(&batch, "amount_low")?;
        let amount_high = column_as::<Int64Array>(&batch, "amount_high")?;
        let owner = column_as::<StringArray>(&batch, "owner")?;
        let source = column_as::<StringArray>(&batch, "source")?;

        for i in 0..batch.num_rows() {
            require_non_null(filing_date, "filing_date", i)?;
            require_non_null(chamber, "chamber", i)?;
            require_non_null(txn_date, "txn_date", i)?;
            require_non_null(txn_type, "txn_type", i)?;
            require_non_null(owner, "owner", i)?;

            let chamber_val = Chamber::parse(chamber.value(i)).ok_or_else(|| {
                Error::Parquet(format!("unknown chamber {:?} at row {i}", chamber.value(i)))
            })?;
            let txn_type_val = TxnType::parse(txn_type.value(i)).ok_or_else(|| {
                Error::Parquet(format!(
                    "unknown txn_type {:?} at row {i}",
                    txn_type.value(i)
                ))
            })?;
            let owner_val = Owner::parse(owner.value(i)).ok_or_else(|| {
                Error::Parquet(format!("unknown owner {:?} at row {i}", owner.value(i)))
            })?;

            rows.push(Trade {
                filing_date: filing_date.value(i),
                doc_id: doc_id.value(i).to_owned(),
                chamber: chamber_val,
                member_name: member_name.value(i).to_owned(),
                party: party.value(i).to_owned(),
                bioguide_id: bioguide_id.value(i).to_owned(),
                state: state.value(i).to_owned(),
                district: district.value(i).to_owned(),
                txn_date: txn_date.value(i),
                notification_date: notification_date.value(i),
                ticker: ticker.value(i).to_owned(),
                asset_description: asset_description.value(i).to_owned(),
                asset_type: asset_type.value(i).to_owned(),
                txn_type: txn_type_val,
                amount_low: amount_low.value(i),
                amount_high: amount_high.value(i),
                owner: owner_val,
                source: source.value(i).to_owned(),
            });
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Trade {
        Trade {
            filing_date: 20240108,
            doc_id: "20024277".into(),
            chamber: Chamber::House,
            member_name: "Richard W. Allen".into(),
            party: "Republican".into(),
            bioguide_id: "A000372".into(),
            state: "GA".into(),
            district: "12".into(),
            txn_date: 20231214,
            notification_date: 20240108,
            ticker: "SCHW".into(),
            asset_description: "Charles Schwab Corporation (SCHW) [ST]".into(),
            asset_type: "stock".into(),
            txn_type: TxnType::Purchase,
            amount_low: 50001,
            amount_high: 100000,
            owner: Owner::Spouse,
            source: "house_clerk".into(),
        }
    }

    #[test]
    fn round_trips_rows() {
        let dir = std::env::temp_dir().join("congresskit_pq_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("congress-2024.parquet");
        let rows = vec![sample()];
        write_trades(&path, &rows).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let back = read_trades(&bytes).unwrap();
        assert_eq!(back, rows);
    }

    #[test]
    fn rejects_null_in_non_nullable_txn_date() {
        // A nullable txn_date column with a null value must be rejected on read.
        let schema = Arc::new(Schema::new(vec![
            Field::new("filing_date", DataType::Int32, false),
            Field::new("doc_id", DataType::Utf8, false),
            Field::new("chamber", DataType::Utf8, false),
            Field::new("member_name", DataType::Utf8, false),
            Field::new("party", DataType::Utf8, false),
            Field::new("bioguide_id", DataType::Utf8, false),
            Field::new("state", DataType::Utf8, false),
            Field::new("district", DataType::Utf8, false),
            Field::new("txn_date", DataType::Int32, true), // nullable — the bad case
            Field::new("notification_date", DataType::Int32, false),
            Field::new("ticker", DataType::Utf8, false),
            Field::new("asset_description", DataType::Utf8, false),
            Field::new("asset_type", DataType::Utf8, false),
            Field::new("txn_type", DataType::Utf8, false),
            Field::new("amount_low", DataType::Int64, false),
            Field::new("amount_high", DataType::Int64, false),
            Field::new("owner", DataType::Utf8, false),
            Field::new("source", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int32Array::from(vec![20240108])),
                Arc::new(StringArray::from(vec!["d"])),
                Arc::new(StringArray::from(vec!["house"])),
                Arc::new(StringArray::from(vec!["m"])),
                Arc::new(StringArray::from(vec![""])),
                Arc::new(StringArray::from(vec![""])),
                Arc::new(StringArray::from(vec!["GA"])),
                Arc::new(StringArray::from(vec!["12"])),
                Arc::new(Int32Array::from(vec![None])),
                Arc::new(Int32Array::from(vec![20240108])),
                Arc::new(StringArray::from(vec!["SCHW"])),
                Arc::new(StringArray::from(vec!["x"])),
                Arc::new(StringArray::from(vec!["stock"])),
                Arc::new(StringArray::from(vec!["purchase"])),
                Arc::new(Int64Array::from(vec![1i64])),
                Arc::new(Int64Array::from(vec![2i64])),
                Arc::new(StringArray::from(vec!["self"])),
                Arc::new(StringArray::from(vec!["house_clerk"])),
            ],
        )
        .unwrap();
        let mut buf = Vec::new();
        {
            let mut w = ArrowWriter::try_new(&mut buf, schema, None).unwrap();
            w.write(&batch).unwrap();
            w.close().unwrap();
        }
        let err = read_trades(&buf).unwrap_err().to_string();
        assert!(err.contains("null txn_date"), "got: {err}");
    }
}
