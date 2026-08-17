//! Run the Microsoft vertical's real read path outside the app and print what
//! it actually returns.
//!
//! `list_notes` reaches the Microsoft branch through `vertical_for(...).await?`
//! and `list_all_notes(...).map_err(...)?`. Both propagate with `?` and neither
//! logs on the way out, and the frontend renders a rejected `invoke` as an
//! empty folder — so a failure here is indistinguishable from "this mailbox has
//! no notes" from the outside. That is what this probe exists to break open: it
//! calls the same code the app calls and prints the error string instead of
//! swallowing it.
//!
//! Lives in `examples/`, never `src/bin/` — see gotcha #3 in CLAUDE.md: Tauri's
//! bundler copies every `[[bin]]` into `Contents/MacOS/`, and macOS Tahoe
//! refuses to launch a bundle with more than one binary there.
//!
//! Usage:
//!     cargo run --example ms_read_probe -- kaiwan.h@live.com
//!
//! The refresh token is read from the keychain by the app's own code and is
//! never printed.

use std::collections::HashMap;

use jodd_lib::backend::microsoft::MicrosoftVertical;
use jodd_lib::backend::NoteStore;
use jodd_lib::backend::Transport;

#[tokio::main]
async fn main() {
    let account_id = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: cargo run --example ms_read_probe -- <account-email>");
        std::process::exit(2);
    });

    // Android initialises this from the Activity; on desktop it is a no-op
    // beyond wiring the platform credential store, but the keychain read below
    // depends on it having happened.
    let _ = jodd_lib::secrets::init();

    println!("account: {account_id}");

    let rt = match jodd_lib::accounts::load_refresh_token(&account_id) {
        Some(rt) => rt,
        None => {
            println!("FAIL: no refresh token in the keychain for this account");
            std::process::exit(1);
        }
    };
    println!("refresh token: present ({} chars)", rt.len());

    let client_id = jodd_lib::auth_ms::client_id();
    if client_id.is_empty() {
        println!("FAIL: MS_CLIENT_ID is empty — is .env loaded for this binary?");
        std::process::exit(1);
    }
    println!("client_id: {}…", &client_id[..8.min(client_id.len())]);

    let token = match jodd_lib::auth_ms::refresh_access_token(&rt).await {
        Ok(t) => t.access_token,
        Err(e) => {
            println!("FAIL at refresh_access_token:\n  {e}");
            std::process::exit(1);
        }
    };
    println!("access token: obtained ({} chars)\n", token.len());

    // Empty folder-id map on purpose: a cold account has no `folders` rows yet,
    // which is exactly the state the app was in when it showed zero notes.
    let v = MicrosoftVertical::new(token, account_id.clone(), HashMap::new());

    println!("--- list_all_notes ---");
    let cache: HashMap<String, jodd_lib::backend::Note> = HashMap::new();
    match v.list_all_notes(&cache).await {
        Ok((notes, _dedup)) => {
            println!("OK: {} note(s)", notes.len());
            for n in notes.iter().take(8) {
                println!(
                    "  [{}] {:?}  body {} chars",
                    n.label,
                    n.title.chars().take(40).collect::<String>(),
                    n.body_html.len()
                );
            }
        }
        Err(e) => println!("FAIL: {e:?}"),
    }

    println!("\n--- list_folders ---");
    match v.list_folders().await {
        Ok(folders) => {
            println!("OK: {} folder(s)", folders.len());
            for f in &folders {
                println!("  {:?} -> id {}…", f.path, &f.id[..12.min(f.id.len())]);
            }
        }
        Err(e) => println!("FAIL: {e:?}"),
    }
}
