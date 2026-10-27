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
//!
//! ```text
//! cargo run --example ingest_probe -- --social "https://www.tiktok.com/@u/video/123"
//! cargo run --example ingest_probe -- --social --download "https://www.instagram.com/reel/XXXX/"
//! ```
//!
//! `--social` reads a TikTok / Instagram / Facebook / X link through the
//! SHIPPED `fetch_meta` and prints, per stage, what came back — lengths, the
//! caption's first 60 characters, the media HOST (never the signed URL). It
//! is the tool the next platform breakage is diagnosed with. `--download`
//! additionally fetches the media into the scratch cache folder, remuxes its
//! audio track, prints the sizes and deletes both files. The probe never
//! transcribes, and opens neither the Jodd database nor the keychain.

use jodd_lib::ingest::audio::{remux, MAX_MEDIA_BYTES, MEDIA_IDLE_TIMEOUT};
use jodd_lib::ingest::net::{download_to_file, host_matches_suffixes, GuardedRequest, MediaPolicy};
use jodd_lib::ingest::social::PlatformMeta;
use jodd_lib::ingest::{self, facebook, instagram, tiktok, urls, x, youtube, FetchPolicy, FetchedSource, SourceFetcher, SourceKind};
use tokio_util::sync::CancellationToken;

fn report(s: &FetchedSource) {
    println!("  status:  {}", s.status.label());
    println!("  title:   {:?}", s.title);
    println!("  chars:   {}", s.text.chars().count());
    println!("  thai word-gap ratio: {:.3} (per-word spacing is ~0.2)", youtube::thai_word_gap_ratio(&s.text));
    println!("  sample:  {}", s.text.chars().take(160).collect::<String>().replace('\n', " / "));
}

/// `--social`: the platform fetcher's output, stage by stage. Prints lengths,
/// the caption's first 60 characters and the media host — never a signed URL,
/// a cookie, or the transcript's text.
async fn social(url: &str, download: bool, policy: FetchPolicy, cancel: &CancellationToken) {
    let meta: PlatformMeta = match urls::classify(url) {
        urls::UrlKind::TikTok { .. } => tiktok::fetch_meta(url, policy, cancel).await,
        urls::UrlKind::Facebook { .. } => facebook::fetch_meta(url, policy, cancel).await,
        urls::UrlKind::Instagram { shortcode } => instagram::fetch_meta(&shortcode, policy, cancel).await,
        urls::UrlKind::X { id } => x::fetch_meta(&id, policy, cancel).await,
        urls::UrlKind::Unsupported(reason) => return println!("  unsupported: {reason}"),
        _ => return println!("  not a TikTok / Instagram / Facebook / X link"),
    };
    println!("  [meta]");
    println!("    kind:           {:?}", meta.platform);
    println!("    canonical url:  {}", meta.canonical_url.as_deref().map(urls::log_form).unwrap_or_else(|| "(none)".into()));
    println!("    author:         {}", meta.author.as_deref().unwrap_or("(none)"));
    match meta.caption.as_deref() {
        Some(c) => println!(
            "    caption:        {} chars, starts \"{}\"",
            c.chars().count(),
            c.chars().take(60).collect::<String>().replace('\n', " ")
        ),
        None => println!("    caption:        (none)"),
    }
    println!("    caption maybe cut: {}", meta.caption_maybe_cut);
    println!("    duration:       {}", meta.duration_secs.map_or("(none)".into(), |d| format!("{d:.1} s")));
    println!("    is_post:        {}", meta.is_post);
    println!("    has_video:      {}", meta.has_video);
    println!("    unavailable:    {}", meta.unavailable.as_deref().unwrap_or("(no)"));
    println!("  [transcript]");
    match &meta.transcript {
        Some(t) => println!(
            "    source:         {} ({} chars, language {})",
            t.source,
            t.text.chars().count(),
            t.language.as_deref().unwrap_or("?")
        ),
        None => println!("    source:         none from the platform (the probe never transcribes)"),
    }
    println!("  [media]");
    let Some(media) = meta.media.as_ref() else {
        return println!("    (the platform gave no media URL)");
    };
    let host = reqwest::Url::parse(&media.url).ok().and_then(|u| u.host_str().map(str::to_string));
    let host_ok = host.as_deref().is_some_and(|h| host_matches_suffixes(h, media.allowed_host_suffixes));
    println!("    host:           {}", host.as_deref().unwrap_or("(unparseable)"));
    println!("    allowed:        {:?} -> {}", media.allowed_host_suffixes, if host_ok { "match" } else { "NO MATCH" });
    println!("    cookies/headers: {} / {}", media.cookies.len(), media.headers.len());
    if !download {
        return println!("    (pass --download to fetch and remux it)");
    }
    if !host_ok {
        return println!("    not downloading: host is off the allowlist");
    }
    println!("  [download + remux]");
    let Some(dir) = jodd_lib::paths::cache_base().map(|b| b.join("jodd")) else {
        return println!("    no cache folder on this device");
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return println!("    cannot create {}: {e}", dir.display());
    }
    let stem = format!("{}probe-{}", jodd_lib::paths::INGEST_TEMP_PREFIX, std::process::id());
    let (video, audio) = (dir.join(format!("{stem}.mp4")), dir.join(format!("{stem}.m4a")));
    let req = GuardedRequest { headers: media.headers.clone(), cookies: media.cookies.clone(), ..GuardedRequest::get(&media.url) };
    let limits = MediaPolicy { allowed_host_suffixes: media.allowed_host_suffixes, max_bytes: MAX_MEDIA_BYTES, idle_timeout: MEDIA_IDLE_TIMEOUT };
    match download_to_file(req, &limits, policy, cancel, &video).await {
        Ok(bytes) => {
            println!("    source bytes:   {bytes}");
            match remux::remux_audio_only(&video, &audio) {
                Ok(a) => println!("    remuxed bytes:  {} ({:.1} s of audio)", a.bytes, a.duration_secs),
                Err(e) => println!("    remux failed:   {e}"),
            }
        }
        Err(e) => println!("    download failed: {e}"),
    }
    let _ = std::fs::remove_file(&video);
    let _ = std::fs::remove_file(&audio);
}

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |args: &mut Vec<String>, name: &str| args.iter().position(|a| a == name).map(|i| args.remove(i)).is_some();
    let is_social = flag(&mut args, "--social");
    let download = flag(&mut args, "--download");
    if args.is_empty() || (download && !is_social) {
        eprintln!("usage: cargo run --example ingest_probe -- [--social [--download]] <url> [<url>…]");
        std::process::exit(2);
    }
    let policy = FetchPolicy::default();
    let cancel = CancellationToken::new();
    let fetcher = ingest::HttpFetcher::default();
    if is_social {
        for url in args {
            println!("\n== {}", urls::log_form(&url));
            let started = std::time::Instant::now();
            social(&url, download, policy, &cancel).await;
            println!("  elapsed: {} ms", started.elapsed().as_millis());
        }
        return;
    }
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
                        let original = info.tracks.iter().find(|t| t.is_original).map(|t| t.language_code.as_str());
                        let chosen = youtube::choose_track(&info.tracks).map(|t| t.language_code.as_str());
                        println!("  original audio's track: {original:?} — chosen: {chosen:?}");
                    }
                    Err(e) => println!("  player request failed: {e}"),
                }
                report(&fetcher.fetch(&url, SourceKind::YouTube, cancel.clone()).await);
            }
            urls::UrlKind::Web
            | urls::UrlKind::TikTok { .. }
            | urls::UrlKind::Instagram { .. }
            | urls::UrlKind::Facebook { .. }
            | urls::UrlKind::X { .. } => report(&fetcher.fetch(&url, SourceKind::Web, cancel.clone()).await),
            urls::UrlKind::Unsupported(reason) => println!("  unsupported: {reason}"),
        }
        println!("  elapsed: {} ms", started.elapsed().as_millis());
    }
}
