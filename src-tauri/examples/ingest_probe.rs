//! The URL-ingest probe — the 2026-09-15 spike, re-runnable through the
//! SHIPPED `ingest::youtube` and `ingest::web` code (spec "Probe").
//!
//! ```text
//! cargo run --example ingest_probe -- \
//!   "https://www.youtube.com/watch?v=jXtnhyro-QE" \
//!   "https://www.youtube.com/watch?v=ve4f7oz-UPs" \
//!   "https://github.com/BBM-Co-ORG/Jodd"
//! ```
//!
//! The URLs must be quoted under zsh — an unquoted `?v=...` query string
//! glob-expands and aborts with "no matches found" before the binary runs.
//!
//! The first thing to run when YouTube changes behaviour: it prints the
//! playability and caption tracks the IOS client gets, then what a real
//! ingest fetch would store. Needs the network; no account.

use jodd_lib::ingest::{self, urls, youtube, FetchPolicy, FetchedSource, SourceFetcher, SourceKind};
use tokio_util::sync::CancellationToken;

fn report(s: &FetchedSource) {
    println!("  status:  {}", s.status.label());
    println!("  title:   {:?}", s.title);
    println!("  chars:   {}", s.text.chars().count());
    println!("  thai word-gap ratio: {:.3} (per-word spacing is ~0.2)", youtube::thai_word_gap_ratio(&s.text));
    println!("  sample:  {}", s.text.chars().take(160).collect::<String>().replace('\n', " / "));
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: cargo run --example ingest_probe -- <url> [<url>…]");
        std::process::exit(2);
    }
    let policy = FetchPolicy::default();
    let cancel = CancellationToken::new();
    let fetcher = ingest::HttpFetcher::default();
    println!("IOS client: clientVersion {} · {} · iOS {}", youtube::IOS.version, youtube::IOS.device_model, youtube::IOS.os_version);
    for url in args {
        println!("\n== {}", urls::log_form(&url));
        let started = std::time::Instant::now();
        match urls::classify(&url) {
            urls::UrlKind::YouTube { id } => {
                match youtube::request_player(&id, &youtube::YoutubeEndpoints::default(), policy, &cancel).await {
                    Ok(info) => {
                        println!("  playability: {:?}", info.playability);
                        println!("  tracks: {:?}", info.tracks.iter().map(|t| (t.language_code.as_str(), if t.is_asr { "asr" } else { "manual" })).collect::<Vec<_>>());
                    }
                    Err(e) => println!("  player request failed: {e}"),
                }
                report(&fetcher.fetch(&url, SourceKind::YouTube, cancel.clone()).await);
            }
            urls::UrlKind::Web => report(&fetcher.fetch(&url, SourceKind::Web, cancel.clone()).await),
            urls::UrlKind::Unsupported(reason) => println!("  unsupported: {reason}"),
        }
        println!("  elapsed: {} ms", started.elapsed().as_millis());
    }
}
