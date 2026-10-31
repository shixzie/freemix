//! SW-013: IANA `TZif` files parse into exact UTC transition tables.

use fm_clock::production::{MAX_TZIF_BYTES, TzifError, TzifZone};

fn header(version: u8, counts: [u32; 6]) -> Vec<u8> {
    let mut out = b"TZif".to_vec();
    out.push(version);
    out.extend([0_u8; 15]);
    for count in counts {
        out.extend_from_slice(&count.to_be_bytes());
    }
    out
}

/// A v2 file: no starts, then DST from 1000 and standard from 2000.
fn zone_bytes() -> Vec<u8> {
    let mut out = header(b'2', [0, 0, 0, 0, 1, 4]);
    out.extend_from_slice(&0_i32.to_be_bytes());
    out.push(0);
    out.push(0);
    out.extend_from_slice(b"UTC\0");
    out.extend_from_slice(&header(b'2', [0, 0, 0, 2, 2, 8]));
    out.extend_from_slice(&1000_i64.to_be_bytes());
    out.extend_from_slice(&2000_i64.to_be_bytes());
    out.push(1);
    out.push(0);
    out.extend_from_slice(&0_i32.to_be_bytes());
    out.push(0);
    out.push(0);
    out.extend_from_slice(&3600_i32.to_be_bytes());
    out.push(1);
    out.push(4);
    out.extend_from_slice(b"STD\0DST\0");
    out
}

#[test]
fn transitions_select_the_exact_type() {
    let zone = TzifZone::from_bytes(&zone_bytes()).unwrap();
    assert_eq!(zone.transition_count(), 2);
    assert_eq!(zone.offset_at(999).unwrap().offset_seconds, 0);
    assert_eq!(zone.offset_at(999).unwrap().abbreviation, "STD");
    assert_eq!(zone.offset_at(1000).unwrap().offset_seconds, 3_600);
    assert_eq!(zone.offset_at(1000).unwrap().abbreviation, "DST");
    assert_eq!(zone.offset_at(1999).unwrap().offset_seconds, 3_600);
    assert_eq!(zone.offset_at(2000).unwrap().offset_seconds, 0);
    assert!(!zone.offset_at(2000).unwrap().is_dst);
    assert_eq!(zone.local_at(1_500), Some(5_100));
    assert_eq!(zone.local_at(2_000), Some(2_000));
}

#[test]
fn malformed_files_are_refused() {
    assert_eq!(TzifZone::from_bytes(&[]), Err(TzifError::Header));
    assert_eq!(TzifZone::from_bytes(b"TZif"), Err(TzifError::Header));
    assert_eq!(
        TzifZone::from_bytes(b"NOPE____________"),
        Err(TzifError::Header)
    );

    let mut huge = zone_bytes();
    // The first header's charcnt is the sixth count.
    huge[20 + 5 * 4..20 + 6 * 4].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(TzifZone::from_bytes(&huge), Err(TzifError::Counts));

    let mut bad_type = zone_bytes();
    // Skip both headers and the v1 data to reach the second transition type.
    let transition_type = 44 + 10 + 44 + 16;
    bad_type[transition_type] = 9;
    assert_eq!(TzifZone::from_bytes(&bad_type), Err(TzifError::TypeIndex));

    let mut bad_abbreviation = zone_bytes();
    let types = 44 + 10 + 44 + 16 + 2;
    bad_abbreviation[types + 11] = 9;
    assert_eq!(
        TzifZone::from_bytes(&bad_abbreviation),
        Err(TzifError::AbbreviationIndex)
    );

    let oversized = vec![0_u8; usize::try_from(MAX_TZIF_BYTES).unwrap() + 1];
    assert!(matches!(
        TzifZone::from_bytes(&oversized),
        Err(TzifError::TooLarge { .. })
    ));
}
