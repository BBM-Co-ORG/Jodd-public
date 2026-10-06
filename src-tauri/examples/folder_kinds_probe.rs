//! Read-only: print one account's folders with their kind and sync state,
//! from the real (encrypted) cache. `cargo run --example folder_kinds_probe -- <account_id> [prefix]`
fn main() {
    jodd_lib::secrets::init().expect("secrets");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = dirs::data_dir().unwrap().join("jodd");
    let db = jodd_lib::db::Db::open(&dir).expect("open db");
    let prefix = args.get(1).cloned().unwrap_or_default();
    for f in db.list_folders(&args[0]).expect("folders") {
        if f.path.starts_with(&prefix) {
            println!("{:?}", f);
        }
    }
}
