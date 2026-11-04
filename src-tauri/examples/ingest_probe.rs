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
//!
//! ```text
//! cargo run --example ingest_probe -- --links "https://example.com/post"
//! ```
//!
//! `--links` runs "Find links" (spec 2026-10-28 §5) through the SHIPPED
//! `links::discover`: every candidate with its score, reasons and whether it
//! would arrive ticked, then what the junk rules skipped. It fetches the page
//! and a few short links — never a candidate. The tool for tuning the rules
//! against real pages (§9 item 9).
//!
//! ```text
//! cargo run --example ingest_probe -- --comments "https://bsky.app/profile/x/post/3k…"
//! YOUTUBE_API_KEY=AIza… cargo run --example ingest_probe -- --comments "https://youtu.be/…"
//! ```
//!
//! `--comments` reads a link's discussion through the SHIPPED readers (spec
//! 2026-10-28 §6): Bluesky, Mastodon, Hacker News, a page's JSON-LD / comment
//! feed / Discourse topic, or YouTube when `YOUTUBE_API_KEY` is set (the probe
//! never opens the keychain). Prints how many comments were read and kept,
//! the author's links, and the rendered section — never a commenter's name.

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

/// `--links`: the ranked candidates and what was skipped. Candidate URLs are
/// printed host + path only (`log_form`) — a query can carry a token.
async fn links(url: &str, policy: FetchPolicy, cancel: &CancellationToken) {
    // The probe never opens the keychain, so YouTube comments are not read here.
    match ingest::links::discover(url, &[], &ingest::links::Readers::default(), policy, cancel).await {
        Ok(d) => {
            println!("  title: {:?}", d.title);
            for c in &d.candidates {
                let tick = if c.preticked { "[x]" } else { "[ ]" };
                println!("  {tick} {:>4}  {:<9} {}", c.score, c.kind, urls::log_form(&c.url));
                println!("              anchor: {}", c.anchor.chars().take(80).collect::<String>());
                println!("              why:    {}", c.reasons.join(" · "));
            }
            println!("  not shown: {}", d.not_shown);
            for s in &d.skipped {
                println!("  skipped {:>3} × {}", s.count, s.reason);
            }
        }
        Err(e) => println!("  failed: {e}"),
    }
}

/// `--comments`: what a link's discussion reader returns.
async fn comments(url: &str, policy: FetchPolicy, cancel: &CancellationToken) {
    use ingest::discuss;
    let show = |d: &discuss::Discussion| {
        let kept = discuss::select(&d.comments).len();
        let op = d.comments.iter().filter(|c| c.is_op).count();
        println!("  comments: read {} (platform says {:?}), kept {kept}, by the author {op}", d.comments.len(), d.total);
        for l in d.author_links() {
            println!("  author link: {}", urls::log_form(&l));
        }
        if let Some(r) = d.render() {
            println!("{}", r.lines().take(12).map(|l| format!("  | {}", l.chars().take(140).collect::<String>())).collect::<Vec<_>>().join("\n"));
        }
    };
    match urls::classify(url) {
        urls::UrlKind::Bluesky { .. } | urls::UrlKind::Mastodon { .. } | urls::UrlKind::HackerNews { .. } => {
            match discuss::read_thread(url, &discuss::ThreadEndpoints::default(), policy, cancel).await {
                Ok(t) => {
                    println!("  title: {:?} · shared: {:?} · partial: {:?}", t.title, t.shared.iter().map(|u| urls::log_form(u)).collect::<Vec<_>>(), t.partial);
                    show(&t.discussion);
                }
                Err(e) => println!("  failed: {e}"),
            }
        }
        urls::UrlKind::YouTube { id } => match std::env::var("YOUTUBE_API_KEY") {
            Ok(key) => match discuss::youtube::read(&discuss::youtube::CommentsApi::with_key(key), &id, policy, cancel).await {
                Ok(d) => show(&d),
                Err(e) => println!("  failed: {e}"),
            },
            Err(_) => println!("  set YOUTUBE_API_KEY to read YouTube comments"),
        },
        urls::UrlKind::Web => {
            let resp = match ingest::net::send_guarded(GuardedRequest::get(url), policy, cancel).await {
                Ok(r) => r,
                Err(e) => return println!("  failed: {e}"),
            };
            let final_url = resp.url().to_string();
            let body = String::from_utf8_lossy(&ingest::net::read_capped(resp, ingest::web::MAX_BODY_BYTES, cancel).await.unwrap_or_default()).into_owned();
            let readable = ingest::web::extract_readable(&body).text;
            let h = discuss::generic::hints(&body, &final_url, &readable);
            println!("  hints: author {:?} · json-ld {} · feed {:?} · discourse {:?}", h.author.is_some(), h.json_ld.is_some(), h.feed.as_deref().map(urls::log_form), h.discourse.as_deref().map(urls::log_form));
            match discuss::generic::read(&body, &final_url, &readable, policy, cancel).await {
                Some(pc) => show(&pc.discussion),
                None => println!("  no comments the page publishes in a standard shape"),
            }
        }
        other => println!("  comments are not readable for this link ({})", urls::kind_name(&other)),
    }
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
    let is_links = flag(&mut args, "--links");
    let is_comments = flag(&mut args, "--comments");
    if args.is_empty() || (download && !is_social) || [is_links, is_social, is_comments].iter().filter(|x| **x).count() > 1 {
        eprintln!("usage: cargo run --example ingest_probe -- [--social [--download] | --links | --comments] <url> [<url>…]");
        std::process::exit(2);
    }
    let policy = FetchPolicy::default();
    let cancel = CancellationToken::new();
    let fetcher = ingest::HttpFetcher::default();
    if is_comments {
        for url in args {
            println!("\n== {}", urls::log_form(&url));
            let started = std::time::Instant::now();
            comments(&url, policy, &cancel).await;
            println!("  elapsed: {} ms", started.elapsed().as_millis());
        }
        return;
    }
    if is_links {
        for url in args {
            println!("\n== {}", urls::log_form(&url));
            let started = std::time::Instant::now();
            links(&url, policy, &cancel).await;
            println!("  elapsed: {} ms", started.elapsed().as_millis());
        }
        return;
    }
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
            ref thread @ (urls::UrlKind::Bluesky { .. } | urls::UrlKind::Mastodon { .. } | urls::UrlKind::HackerNews { .. }) => {
                let kind = urls::thread_kind(thread).unwrap_or(SourceKind::Web);
                report(&fetcher.fetch(&url, kind, cancel.clone()).await)
            }
            urls::UrlKind::Unsupported(reason) => println!("  unsupported: {reason}"),
        }
        println!("  elapsed: {} ms", started.elapsed().as_millis());
    }
}
