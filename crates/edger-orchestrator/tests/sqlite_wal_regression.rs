use rusqlite::Connection;
use std::path::Path;

// RUSTSEC-2026-0459: exercise sqlite3WalReadFrame through ordinary SQL.
// Fixture shape: https://sqlite.org/bugs/info/2026-08-17T15:54:41Z
const BLOB_LEN: usize = 3000;
const CELL_DATA_OFFSET: usize = 38;

// SQLite's WAL checksum covers pairs of native-endian words. Magic 0x377f0682
// selects little-endian words; the stored checksum itself is big-endian.
fn checksum(bytes: &[u8], mut state: [u32; 2]) -> [u32; 2] {
    assert_eq!(bytes.len() % 8, 0);
    for pair in bytes.as_chunks::<8>().0 {
        let first = u32::from_le_bytes(pair[..4].try_into().unwrap());
        let second = u32::from_le_bytes(pair[4..].try_into().unwrap());
        state[0] = state[0].wrapping_add(first).wrapping_add(state[1]);
        state[1] = state[1].wrapping_add(second).wrapping_add(state[0]);
    }
    state
}

fn put_word(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

fn write_wal(path: &Path, page_size: usize, root_page: u32, page_count: u32) {
    let salt = [0x1234_5678, 0x9abc_def0];
    let mut header = [0_u8; 32];
    put_word(&mut header, 0, 0x377f_0682);
    put_word(&mut header, 4, 3_007_000);
    put_word(&mut header, 8, page_size.try_into().unwrap());
    put_word(&mut header, 16, salt[0]);
    put_word(&mut header, 20, salt[1]);
    let header_sum = checksum(&header[..24], [0, 0]);
    put_word(&mut header, 24, header_sum[0]);
    put_word(&mut header, 28, header_sum[1]);

    // One table-leaf cell: payload length 3003, rowid 1, record header
    // length 3, serial type 6012 (a 3000-byte BLOB). In a short WAL page,
    // only the prefix is present; the DB page still has room for the cell.
    let mut page = vec![0_u8; page_size];
    page[0] = 0x0d;
    page[3..5].copy_from_slice(&1_u16.to_be_bytes());
    page[5..7].copy_from_slice(&32_u16.to_be_bytes());
    page[8..10].copy_from_slice(&32_u16.to_be_bytes());
    page[32..CELL_DATA_OFFSET].copy_from_slice(&[0x97, 0x3b, 1, 3, 0xae, 0x7c]);
    let prefix_len = BLOB_LEN.min(page_size - CELL_DATA_OFFSET);
    page[CELL_DATA_OFFSET..CELL_DATA_OFFSET + prefix_len].fill(0x5a);

    let mut frame = [0_u8; 24];
    put_word(&mut frame, 0, root_page);
    put_word(&mut frame, 4, page_count);
    put_word(&mut frame, 8, salt[0]);
    put_word(&mut frame, 12, salt[1]);
    let frame_sum = checksum(&page, checksum(&frame[..8], header_sum));
    put_word(&mut frame, 16, frame_sum[0]);
    put_word(&mut frame, 20, frame_sum[1]);
    let wal = [header.as_slice(), frame.as_slice(), page.as_slice()].concat();
    std::fs::write(path.with_extension("db-wal"), wal).unwrap();
}

fn query_wal_blob(wal_page_size: usize) -> Vec<u8> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fixture.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "PRAGMA page_size=4096;
             PRAGMA journal_mode=WAL;
             CREATE TABLE target (b BLOB);
             CREATE TABLE primer (b BLOB);",
        )
        .unwrap();
    // Force cache-page recycling before reading the short WAL frame. The
    // nonzero marker makes an uninitialized tail observable on the old engine.
    for _ in 0..48 {
        connection
            .execute("INSERT INTO primer VALUES (?1)", [vec![0x43_u8; 3500]])
            .unwrap();
    }
    let root_page: u32 = connection
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name='target'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let page_count: u32 = connection
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .unwrap();
    connection.close().unwrap();
    write_wal(&path, wal_page_size, root_page, page_count);

    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("PRAGMA mmap_size=0; PRAGMA cache_size=12;")
        .unwrap();
    let mut statement = connection.prepare("SELECT b FROM primer").unwrap();
    let rows = statement
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap();
    for row in rows {
        assert_eq!(row.unwrap(), vec![0x43; 3500]);
    }
    drop(statement);
    connection
        .query_row("SELECT b FROM target", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn matching_wal_page_size_preserves_the_entire_blob() {
    assert_eq!(query_wal_blob(4096), vec![0x5a; BLOB_LEN]);
}

fn assert_zeroed_tail(wal_page_size: usize) {
    let blob = query_wal_blob(wal_page_size);
    assert_eq!(blob.len(), BLOB_LEN);
    let present = wal_page_size - CELL_DATA_OFFSET;
    assert!(blob[..present].iter().all(|byte| *byte == 0x5a));
    assert!(
        blob[present..].iter().all(|byte| *byte == 0),
        "WAL page size {wal_page_size}: unread tail exposed nonzero cache bytes"
    );
}

#[test]
fn short_512_byte_wal_frame_zeroes_the_unread_database_page_tail() {
    assert_zeroed_tail(512);
}

#[test]
fn short_1024_byte_wal_frame_zeroes_the_unread_database_page_tail() {
    assert_zeroed_tail(1024);
}
