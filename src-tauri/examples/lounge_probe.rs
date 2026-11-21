//! Diagnostic for "What's on my TV" (spec 2026-11-02-tv-now-playing-ingest):
//! can Jodd still learn which YouTube video a TV is playing, through YouTube's
//! unofficial Lounge API — the "Link with TV code" pairing that pyytlounge /
//! iSponsorBlockTV use? Run it the day YouTube changes the protocol, like
//! `ingest_probe` for InnerTube.
//!
//! ```text
//! cargo run --example lounge_probe -- pair 123456789012
//! cargo run --example lounge_probe -- now <screen_id> [--raw]
//! ```
//!
//! It calls the SHIPPED client, `jodd_lib::ingest::tv_lounge`, so a pass here
//! is a pass of the code the app runs.
//!
//! `pair` takes the code from the TV's YouTube app → Settings → Link with TV
//! code, and prints the screen ID once. That ID is a secret — it controls the
//! TV's linking — so keep it private. Only the screen ID is needed afterwards:
//! `now` mints a fresh lounge token from it, binds a remote-control session,
//! asks `getNowPlaying`, and prints the video ID, the time and `PASS` or
//! `FAIL`. `--raw` additionally prints every event the TV sends, each led by
//! the milliseconds since `now` began (`+1234ms  ...`), and the total at the
//! end; the screen ID is masked if an event happens to carry it. Those
//! timings are what tunes the app's head start and after-video window.
//! `LISTEN_SECS` sets the overall deadline (default 6, the app's own).
//!
//! The device ID it sends is the app's own (`TvLinks::app()?.device_id()`: it
//! reads, or on a fresh install mints, the flag file `app_tv_links.json` and
//! nothing else), so a probe run reuses the app's single "Jodd" remote on the
//! TV instead of adding a new one every time. `LOUNGE_DEVICE_ID` overrides it.
//!
//! Opens neither the Jodd database nor the keychain (it builds the app's TV
//! list but never reads a secret from it). Never prints the lounge token (this
//! program never sees it) and never prints the screen ID in `now`; `pair`
//! prints it once, by design, marked private.

use std::time::{Duration, Instant};

use jodd_lib::ingest::tv_lounge::{self, Deadlines, LoungeEndpoints, LoungeEvent, PairError, TvError};
use jodd_lib::ingest::FetchPolicy;
use jodd_lib::tv_link::TvLinks;
use tokio_util::sync::CancellationToken;

const USAGE: &str = "usage: lounge_probe pair <code> | now <screen_id> [--raw]";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Cmd<'a> {
    Pair(&'a str),
    Now { screen_id: &'a str, raw: bool },
}

/// `None` is a usage error. A `now` screen ID that starts with `--` is a flag
/// in the wrong place (`now --raw`), never a screen: it must not reach the network.
fn parse(args: &[String]) -> Option<Cmd<'_>> {
    match args.first().map(String::as_str) {
        Some("pair") if args.len() == 2 => Some(Cmd::Pair(&args[1])),
        Some("now") if args.len() >= 2 && !args[1].starts_with("--") && args[2..].iter().all(|a| a == "--raw") => {
            Some(Cmd::Now { screen_id: &args[1], raw: args.len() > 2 })
        }
        _ => None,
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match parse(&args) {
        Some(Cmd::Pair(code)) => pair(code).await,
        Some(Cmd::Now { screen_id, raw }) => now(screen_id, raw).await,
        None => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("FAIL — {e}");
        std::process::exit(1);
    }
}

async fn pair(code: &str) -> Result<(), String> {
    let cancel = CancellationToken::new();
    let p = tv_lounge::pair(&LoungeEndpoints::default(), FetchPolicy::default(), code, &cancel)
        .await
        .map_err(|e| match e {
            PairError::BadCode => "code not recognised — wrong, or expired (the TV shows a new one every few minutes)".to_owned(),
            PairError::Network(s) => format!("network: {s}"),
        })?;
    println!("paired with: {}", p.screen_name);
    println!("screen ID:   {}", p.screen_id);
    println!("             ^ keep this private — it controls the TV's linking");
    println!("\nnext: cargo run --example lounge_probe -- now <screen ID>");
    Ok(())
}

/// Mask `secret` wherever it appears in an event's printed arguments.
fn masked(ev: &LoungeEvent, secret: &str) -> String {
    let args = serde_json::to_string(&ev.args).unwrap_or_default().replace(secret, "<screen>");
    args.chars().take(240).collect()
}

/// One `--raw` line: the time since `now` began, then the event.
fn event_line(elapsed: Duration, ev: &LoungeEvent, secret: &str) -> String {
    format!("+{}ms  [{:>3}] {:<24} {}", elapsed.as_millis(), ev.id, ev.name, masked(ev, secret))
}

fn summary_line(seen: usize, secs: u64, elapsed: Duration) -> String {
    format!("({seen} events, overall deadline {secs}s, total {}ms)", elapsed.as_millis())
}

/// `LOUNGE_DEVICE_ID` wins when set and non-empty; otherwise the app's own.
fn pick_device_id(env: Option<String>, app: impl FnOnce() -> Result<String, String>) -> Result<String, String> {
    match env.filter(|v| !v.is_empty()) {
        Some(v) => Ok(v),
        None => app(),
    }
}

async fn now(screen_id: &str, raw: bool) -> Result<(), String> {
    let secs = std::env::var("LISTEN_SECS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(6);
    let deadlines = Deadlines { overall: Duration::from_secs(secs), ..Deadlines::default() };
    let device_id = pick_device_id(std::env::var("LOUNGE_DEVICE_ID").ok(), || TvLinks::app()?.device_id())?;
    let cancel = CancellationToken::new();
    let started = Instant::now();
    let mut seen = 0usize;
    let mut trace = |ev: &LoungeEvent| {
        seen += 1;
        if raw {
            println!("{}", event_line(started.elapsed(), ev, screen_id));
        }
    };
    let result = tv_lounge::now_playing_traced(
        &LoungeEndpoints::default(),
        FetchPolicy::default(),
        screen_id,
        &device_id,
        deadlines,
        &cancel,
        &mut trace,
    )
    .await;
    let total = started.elapsed();
    println!("{}", summary_line(seen, secs, total));
    match result {
        Ok((s, model)) => {
            println!("TV:           {}", model.as_deref().unwrap_or("<not reported>"));
            println!("state:        {}", s.state.as_deref().unwrap_or("<none>"));
            println!("video ID:     {}", s.video_id);
            match s.seconds {
                Some(t) => println!("time:         {t}s"),
                None => println!("time:         <none> (try pausing/playing on the TV)"),
            }
            println!("PASS — the TV is on https://youtu.be/{}", s.video_id);
            Ok(())
        }
        Err(TvError::NothingPlaying) => Err("the TV answered, but nothing is playing — open a video and retry".into()),
        Err(TvError::Unlinked) => Err("Google no longer knows this screen — the TV unlinked Jodd; run `pair` again".into()),
        Err(TvError::Network(s)) => Err(format!("network: {s}")),
        Err(TvError::Cancelled) => Err("cancelled".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_flag_in_the_screen_id_place_is_usage() {
        assert_eq!(parse(&args(&["now", "--raw"])), None);
        assert_eq!(parse(&args(&["now", "--raw", "--raw"])), None);
        assert_eq!(parse(&args(&["now", "--anything"])), None);
        assert_eq!(parse(&args(&["now"])), None);
    }

    #[test]
    fn the_good_shapes_still_parse() {
        assert_eq!(parse(&args(&["now", "abc"])), Some(Cmd::Now { screen_id: "abc", raw: false }));
        assert_eq!(parse(&args(&["now", "abc", "--raw"])), Some(Cmd::Now { screen_id: "abc", raw: true }));
        assert_eq!(parse(&args(&["now", "abc", "--nope"])), None);
        assert_eq!(parse(&args(&["pair", "123456789012"])), Some(Cmd::Pair("123456789012")));
        assert_eq!(parse(&args(&["pair"])), None);
    }

    #[test]
    fn the_device_id_is_the_apps_unless_overridden() {
        let from_app = || Ok::<_, String>("app-id".to_string());
        assert_eq!(pick_device_id(None, from_app), Ok("app-id".into()));
        assert_eq!(pick_device_id(Some(String::new()), from_app), Ok("app-id".into()));
        assert_eq!(pick_device_id(Some("mine".into()), || panic!("read the app's file")), Ok("mine".into()));
        assert_eq!(pick_device_id(None, || Err("no config dir".into())), Err("no config dir".into()));
    }

    #[test]
    fn raw_lines_lead_with_elapsed_ms_and_mask_the_screen() {
        let ev = LoungeEvent { id: 7, name: "nowPlaying".into(), args: vec![serde_json::json!({"screenId": "SECRET", "currentTime": "1.5"})] };
        let line = event_line(Duration::from_millis(1234), &ev, "SECRET");
        assert!(line.starts_with("+1234ms  "), "{line}");
        assert!(line.contains("nowPlaying") && line.contains("<screen>"), "{line}");
        assert!(!line.contains("SECRET"), "{line}");
        assert_eq!(summary_line(3, 6, Duration::from_millis(2500)), "(3 events, overall deadline 6s, total 2500ms)");
    }
}
