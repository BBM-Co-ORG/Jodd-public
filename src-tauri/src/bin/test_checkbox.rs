// One-shot test: insert a note into Gmail with three flavors of checkbox markup
// to see what Apple Notes preserves vs. strips on round-trip.
//
// Run from src-tauri/:
//   cargo run --bin test_checkbox             # uses first account in accounts.json
//   cargo run --bin test_checkbox -- 1        # uses account at index 1
//
// After it succeeds, watch your iPhone Notes for "Jodd checkbox test <date>"
// in the "Notes" folder. Open it, then look at the same note here in Jodd
// (or directly in Gmail) to see what survived Apple's HTML pipeline.

use jodd_lib::{accounts, auth, gmail};

#[tokio::main]
async fn main() -> Result<(), String> {
    // Load .env so GOOGLE_CLIENT_ID is visible to auth::client_id().
    let _ = dotenv::from_path("../.env");
    let _ = dotenv::dotenv();

    let all = accounts::load_accounts();
    if all.is_empty() {
        return Err("no signed-in accounts found — run Jodd and sign in first".into());
    }

    let idx: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let account = all.get(idx).ok_or_else(|| {
        format!("account index {} out of range (have {})", idx, all.len())
    })?;
    eprintln!("[test] using account [{}] {}", idx, account.email);

    let rt = accounts::load_refresh_token(&account.id)
        .ok_or_else(|| format!("no refresh token in keychain for {}", account.id))?;
    let token = auth::refresh_access_token(&rt).await?;
    eprintln!("[test] access token acquired ({:?}s lifetime)", token.expires_in);

    let label_map = gmail::get_label_map(&token.access_token).await?;
    eprintln!("[test] loaded {} Gmail labels", label_map.len());

    let today = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let title = format!("Jodd checkbox test {}", today);

    // Three flavors mixed in one note so a single round-trip is conclusive.
    let body = r#"<div>SECTION A — raw input checkboxes (paragraph form):</div>
<div><input type="checkbox" checked> done item A1</div>
<div><input type="checkbox"> open item A2</div>

<div>SECTION B — input checkboxes inside a list:</div>
<ul>
  <li><input type="checkbox" checked> done item B1</li>
  <li><input type="checkbox"> open item B2
    <ul>
      <li><input type="checkbox"> nested open item B2a</li>
    </ul>
  </li>
</ul>

<div>SECTION C — visible-glyph fallback (control):</div>
<ul class="jodd-tasks">
  <li class="jodd-task done">&#9745; done item C1</li>
  <li class="jodd-task">&#9744; open item C2</li>
</ul>

<div>SECTION D — link round-trip control:</div>
<div><a href="https://example.com/jodd-test">example.com/jodd-test</a></div>
"#;

    let saved = gmail::save_note(
        &token.access_token,
        &title,
        body,
        None,                      // existing_gmail_id
        None,                      // existing_uuid
        None,                      // existing_x_mail_created_date
        "Notes",                   // root Notes label
        &account.email,
        &label_map,
    )
    .await?;

    eprintln!("[test] INSERTED ok — gmail message id: {}", saved.id);
    eprintln!("[test] uuid: {}", saved.uuid);
    eprintln!("[test] title: {}", title);
    eprintln!();
    eprintln!("Next: wait ~30s for Apple to sync, open the note on iPhone,");
    eprintln!("then re-open it in Jodd (or Gmail web) and compare body HTML.");
    Ok(())
}
