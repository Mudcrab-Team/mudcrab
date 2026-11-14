//! Opt-in records-only differential driver; ordinary tests generate their own mod fixtures.
#[test]
#[ignore = "requires the read-only official-plugin corpus and a fresh external output path"]
fn handwritten_real80() {
    let data =
        std::path::PathBuf::from(std::env::var_os("FORMKEY_REAL80_DATA").expect("data root"));
    let list =
        std::path::PathBuf::from(std::env::var_os("FORMKEY_REAL80_LIST").expect("plugin list"));
    let output = std::path::PathBuf::from(
        std::env::var_os("FORMKEY_REAL80_OUTPUT").expect("fresh database"),
    );
    assert!(!output.exists(), "fresh output required");
    let paths = converter::esm::plugins_txt::read_plugins_txt(&list, &data).unwrap();
    assert_eq!(paths.len(), 80);
    converter::esm::EsmParser::convert_plugins(&paths, &output).unwrap();
    let db = rusqlite::Connection::open(&output).unwrap();
    let count: u64 = db
        .query_row("SELECT count(*) FROM records", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1_240_167);
    println!("handwritten real80: plugins=80 records={count}");
}
