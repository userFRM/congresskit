//! The congressional-trade record and its small categorical enums.
//!
//! One [`Trade`] is one disclosed transaction line from a STOCK Act periodic
//! transaction report (PTR). Dates are stored as `i32` in `YYYYMMDD` form (e.g.
//! `20240401`) so comparisons are integer-cheap and need no calendar library on
//! the hot path. Amount ranges are stored as integer-dollar `amount_low` /
//! `amount_high` bounds parsed from the disclosed `"$min - $max"` band.
use serde::{Deserialize, Serialize};

/// Which chamber filed the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Chamber {
    House,
    Senate,
}

impl Chamber {
    pub fn as_str(self) -> &'static str {
        match self {
            Chamber::House => "house",
            Chamber::Senate => "senate",
        }
    }
    pub fn parse(s: &str) -> Option<Chamber> {
        match s {
            "house" => Some(Chamber::House),
            "senate" => Some(Chamber::Senate),
            _ => None,
        }
    }
}

/// Disclosed transaction kind. PTRs use single-letter codes (P/S/E) plus a
/// "S (partial)" partial-sale variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TxnType {
    Purchase,
    Sale,
    PartialSale,
    Exchange,
}

impl TxnType {
    /// Map a raw PTR transaction-type token to a [`TxnType`].
    ///
    /// Accepts `"P"`, `"S"`, `"S (partial)"` / `"S (Partial)"`, and `"E"`
    /// (case-insensitive). Returns `None` for anything else.
    pub fn from_code(raw: &str) -> Option<TxnType> {
        let t = raw.trim().to_ascii_lowercase();
        if t.starts_with("s (partial") || t == "sp" {
            Some(TxnType::PartialSale)
        } else if t.starts_with('p') {
            Some(TxnType::Purchase)
        } else if t.starts_with('e') {
            Some(TxnType::Exchange)
        } else if t.starts_with('s') {
            Some(TxnType::Sale)
        } else {
            None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TxnType::Purchase => "purchase",
            TxnType::Sale => "sale",
            TxnType::PartialSale => "partial_sale",
            TxnType::Exchange => "exchange",
        }
    }

    pub fn parse(s: &str) -> Option<TxnType> {
        match s {
            "purchase" => Some(TxnType::Purchase),
            "sale" => Some(TxnType::Sale),
            "partial_sale" => Some(TxnType::PartialSale),
            "exchange" => Some(TxnType::Exchange),
            _ => None,
        }
    }
}

/// Beneficial owner of the position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Owner {
    SelfFiler,
    Spouse,
    Child,
    Joint,
}

impl Owner {
    /// Map a PTR owner code to an [`Owner`]. `SP`=spouse, `DC`=dependent child,
    /// `JT`=joint; an empty or unknown code is the filer themselves.
    pub fn from_code(raw: &str) -> Owner {
        match raw.trim().to_ascii_uppercase().as_str() {
            "SP" => Owner::Spouse,
            "DC" => Owner::Child,
            "JT" => Owner::Joint,
            _ => Owner::SelfFiler,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Owner::SelfFiler => "self",
            Owner::Spouse => "spouse",
            Owner::Child => "child",
            Owner::Joint => "joint",
        }
    }

    pub fn parse(s: &str) -> Option<Owner> {
        match s {
            "self" => Some(Owner::SelfFiler),
            "spouse" => Some(Owner::Spouse),
            "child" => Some(Owner::Child),
            "joint" => Some(Owner::Joint),
            _ => None,
        }
    }
}

/// One disclosed congressional transaction (one row in the bundled parquet).
///
/// Dates are `i32` `YYYYMMDD` (0 when the filing omits the date). `ticker` is
/// empty when the disclosure names no symbol. `party` and `bioguide_id` are
/// empty when the member could not be matched to the public roster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trade {
    pub filing_date: i32,
    pub doc_id: String,
    pub chamber: Chamber,
    pub member_name: String,
    pub party: String,
    pub bioguide_id: String,
    pub state: String,
    /// House district (e.g. `"31"`); empty for the Senate.
    pub district: String,
    pub txn_date: i32,
    pub notification_date: i32,
    pub ticker: String,
    pub asset_description: String,
    /// One of `stock`, `option`, `other`.
    pub asset_type: String,
    pub txn_type: TxnType,
    pub amount_low: i64,
    pub amount_high: i64,
    pub owner: Owner,
    /// `house_clerk` or `senate_efd`.
    pub source: String,
}

impl Trade {
    /// `true` for a purchase.
    pub fn is_buy(&self) -> bool {
        self.txn_type == TxnType::Purchase
    }

    /// `true` for a full or partial sale.
    pub fn is_sell(&self) -> bool {
        matches!(self.txn_type, TxnType::Sale | TxnType::PartialSale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txn_type_from_codes() {
        assert_eq!(TxnType::from_code("P"), Some(TxnType::Purchase));
        assert_eq!(TxnType::from_code("S"), Some(TxnType::Sale));
        assert_eq!(
            TxnType::from_code("S (partial)"),
            Some(TxnType::PartialSale)
        );
        assert_eq!(TxnType::from_code("E"), Some(TxnType::Exchange));
        assert_eq!(TxnType::from_code("x"), None);
    }

    #[test]
    fn owner_from_codes() {
        assert_eq!(Owner::from_code("SP"), Owner::Spouse);
        assert_eq!(Owner::from_code("DC"), Owner::Child);
        assert_eq!(Owner::from_code("JT"), Owner::Joint);
        assert_eq!(Owner::from_code(""), Owner::SelfFiler);
    }

    #[test]
    fn round_trips() {
        for c in [Chamber::House, Chamber::Senate] {
            assert_eq!(Chamber::parse(c.as_str()), Some(c));
        }
        for t in [
            TxnType::Purchase,
            TxnType::Sale,
            TxnType::PartialSale,
            TxnType::Exchange,
        ] {
            assert_eq!(TxnType::parse(t.as_str()), Some(t));
        }
        for o in [Owner::SelfFiler, Owner::Spouse, Owner::Child, Owner::Joint] {
            assert_eq!(Owner::parse(o.as_str()), Some(o));
        }
    }
}
