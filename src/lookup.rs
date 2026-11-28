//! One-transaction lookup, shared by `recon inspect` and
//! `GET /api/v1/records/{transaction}`.
//!
//! A support ticket names a transaction hash, not a block range. Both surfaces
//! therefore need the same two things: a canonical form for the hash a person
//! pasted, and the ledger record that claims it. Keeping that here means the
//! CLI and the API cannot drift on what counts as a valid hash or on how a
//! mixed-case hash is compared.

use crate::model::LedgerRecord;

/// Canonicalise a transaction hash: `0x` (either case) plus exactly 64 hex
/// digits, surrounding whitespace ignored, output lower-case.
///
/// Lower-cased because the ledger stores hashes exactly as the gateway saw
/// them; a client pasting an upper-case hash should still address the same
/// record, and the canonical form makes that comparison trivial.
pub fn normalize_transaction(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("0x{}", hex.to_ascii_lowercase()))
}

/// The first ledger record whose `tx_hash` is `hash`, compared case
/// insensitively and ignoring stray whitespace.
///
/// A record with no hash, or one whose hash is `"settled"` rather than a real
/// transaction id, never matches: it cannot be addressed by a transaction.
pub fn find_record<'a>(records: &'a [LedgerRecord], hash: &str) -> Option<&'a LedgerRecord> {
    let needle = hash.trim();
    records.iter().find(|record| {
        record
            .tx_hash
            .as_deref()
            .is_some_and(|candidate| candidate.trim().eq_ignore_ascii_case(needle))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, hash: Option<&str>) -> LedgerRecord {
        LedgerRecord {
            id: id.to_owned(),
            pay_to: "0x2222222222222222222222222222222222222222".to_owned(),
            payer: None,
            amount_atomic: "10000".to_owned(),
            tx_hash: hash.map(str::to_owned),
            observed_at: "2026-11-22T10:00:00Z".to_owned(),
            network: None,
        }
    }

    #[test]
    fn hashes_normalise_to_lower_case_with_the_0x_prefix() {
        let hash = "0xDEADBEEFdeadbeefDEADBEEFdeadbeefDEADBEEFdeadbeefDEADBEEFdeadbeef";
        assert_eq!(
            normalize_transaction(hash).as_deref(),
            Some("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
        );
        assert_eq!(
            normalize_transaction(
                "   0Xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef "
            )
            .as_deref(),
            Some("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
        );
    }

    #[test]
    fn anything_but_a_full_32_byte_hash_is_rejected() {
        for rejected in [
            "0x",
            "0x123",
            "1111",
            "settled",
            "",
            "  ",
            "0xdeadbeef",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeeg",
        ] {
            assert!(
                normalize_transaction(rejected).is_none(),
                "accepted {rejected:?}"
            );
        }
    }

    #[test]
    fn lookup_matches_any_case_and_ignores_records_without_a_hash() {
        let records = vec![
            record("inv_ok", Some("0x1111")),
            record("inv_hashless", None),
            record(
                "inv_real",
                Some("0xDEADBEEFdeadbeefDEADBEEFdeadbeefDEADBEEFdeadbeefDEADBEEFdeadbeef"),
            ),
        ];
        let canonical = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        assert_eq!(
            find_record(&records, canonical).map(|record| record.id.as_str()),
            Some("inv_real")
        );
        assert_eq!(
            find_record(
                &records,
                " 0XDEADBEEFdeadbeefDEADBEEFdeadbeefDEADBEEFdeadbeefDEADBEEFdeadbeef "
            )
            .map(|record| record.id.as_str()),
            Some("inv_real")
        );
        assert!(find_record(&records, "0x9999").is_none());
    }
}
