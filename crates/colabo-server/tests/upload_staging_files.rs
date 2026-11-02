use std::io::Write;

use colabo_storage::Store;
use uuid::Uuid;

const PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

fn payload(seed: u8) -> Vec<u8> {
    (0..PAYLOAD_BYTES)
        .map(|index| seed.wrapping_add((index.wrapping_mul(31) ^ (index >> 8)) as u8))
        .collect()
}

fn staging_file(directory: &std::path::Path, bytes: &[u8]) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new_in(directory).unwrap();
    file.write_all(bytes).unwrap();
    file.as_file().sync_all().unwrap();
    file
}

#[test]
fn staged_binary_upload_creates_nested_file_and_replaces_it_exactly() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("store")).unwrap();
    let workspace = store
        .create_workspace(Uuid::now_v7(), "staged upload".into())
        .unwrap();
    let target = "one/two/three/four/five/payload.bin";
    let initial_payload = payload(17);
    let initial_staging = staging_file(directory.path(), &initial_payload);

    store
        .create_file_from_path(workspace.id, target, initial_staging.path())
        .unwrap();
    assert_eq!(
        store.read_file(workspace.id, target).unwrap(),
        initial_payload
    );

    let replacement_payload = payload(113);
    let replacement_staging = staging_file(directory.path(), &replacement_payload);
    store
        .replace_file_from_path(workspace.id, target, replacement_staging.path())
        .unwrap();
    assert_eq!(
        store.read_file(workspace.id, target).unwrap(),
        replacement_payload
    );
}
