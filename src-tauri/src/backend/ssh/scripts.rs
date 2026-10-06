//! Every remote operation as a POSIX `sh` script, and the parsers for what
//! those scripts print. GNU and BSD differ in three commands only — the sha
//! tool, `stat`, and `base64`'s decode flag — so [`Flavor`] carries exactly
//! those, probed once per vertical.

use std::collections::{BTreeMap, HashMap};

use base64::Engine as _;
use sha2::{Digest, Sha256};

use super::session::sh_quote;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sha { Sha256sum, Shasum }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stat { Gnu, Bsd }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flavor {
    pub sha: Sha,
    pub stat: Stat,
    /// Older macOS `base64` decodes with `-D` only.
    pub b64_decode_upper: bool,
}

impl Flavor {
    fn sha_cmd(self) -> &'static str {
        match self.sha { Sha::Sha256sum => "sha256sum", Sha::Shasum => "shasum -a 256" }
    }
    fn stat_cmd(self) -> &'static str {
        match self.stat { Stat::Gnu => "stat -c '%Y %n'", Stat::Bsd => "stat -f '%m %N'" }
    }
    fn b64d(self) -> &'static str {
        if self.b64_decode_upper { "base64 -D" } else { "base64 -d" }
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub const PROBE: &str = "\
if command -v sha256sum >/dev/null 2>&1; then echo sha=sha256sum; \
elif command -v shasum >/dev/null 2>&1; then echo sha=shasum; else echo sha=none; fi
if stat -c %Y / >/dev/null 2>&1; then echo stat=gnu; else echo stat=bsd; fi
if printf 'aGk=' | base64 -d >/dev/null 2>&1; then echo b64=d; else echo b64=D; fi
";

pub fn parse_probe(stdout: &str) -> Result<Flavor, String> {
    let get = |k: &str| stdout.lines().find_map(|l| l.strip_prefix(k)).map(str::trim);
    let sha = match get("sha=") {
        Some("sha256sum") => Sha::Sha256sum,
        Some("shasum") => Sha::Shasum,
        _ => return Err("the server has neither sha256sum nor shasum".into()),
    };
    let stat = if get("stat=") == Some("gnu") { Stat::Gnu } else { Stat::Bsd };
    Ok(Flavor { sha, stat, b64_decode_upper: get("b64=") == Some("D") })
}

fn prelude(root: &str) -> String {
    format!("cd {} || exit 97\n", sh_quote(root))
}

/// `r` holds the raw path; a leading `~` is expanded on the server, where
/// `$HOME` is the right one.
fn tilde(path: &str) -> String {
    format!(
        "r={}\ncase \"$r\" in '~') r=\"$HOME\";; '~/'*) r=\"$HOME/${{r#'~/'}}\";; esac\n",
        sh_quote(path)
    )
}

pub fn resolve_root(root: &str, create: bool) -> String {
    format!(
        "{t}if [ ! -d \"$r\" ]; then if [ {c} = 1 ]; then mkdir -p \"$r\" || exit 1; else exit 4; fi; fi\n\
         [ -w \"$r\" ] || exit 5\ncd \"$r\" && pwd -P\n",
        t = tilde(root),
        c = if create { 1 } else { 0 },
    )
}

pub fn list_dirs(path: &str) -> String {
    format!(
        "{t}cd \"$r\" || exit 4\npwd -P\nfor d in */; do [ -d \"$d\" ] || continue; printf '%s\\n' \"${{d%/}}\"; done\n",
        t = tilde(path)
    )
}

pub fn parse_dirs(stdout: &str) -> Option<(String, Vec<String>)> {
    let mut lines = stdout.lines();
    let abs = lines.next()?.to_string();
    let mut dirs: Vec<String> = lines.filter(|l| !l.is_empty()).map(String::from).collect();
    dirs.sort();
    Some((abs, dirs))
}

const NOTE_FILES: &str = r"-type f \( -name '*.md' -o -name '*.html' \)";

/// `folder: None` walks the whole vault; `Some(dir)` hashes one directory
/// level only — the UI sweeps folders every few seconds (spec A8).
pub fn list(root: &str, f: Flavor, folder: Option<&str>) -> String {
    let (start, depth) = match folder {
        Some(d) => (sh_quote(d), "-maxdepth 1 "),
        None => ("Notes".to_string(), ""),
    };
    format!(
        "{pre}[ -d {start} ] || exit 0\n\
         find {start} {depth}{NOTE_FILES} -exec {sha} {{}} + | sed 's/^/H /'\n\
         find {start} {depth}{NOTE_FILES} -exec {stat} {{}} + | sed 's/^/M /'\n\
         find {start} {depth}-type d | sed 's/^/D /'\n",
        pre = prelude(root),
        sha = f.sha_cmd(),
        stat = f.stat_cmd(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub sha: String,
    pub mtime: i64,
}

#[derive(Debug, Default)]
pub struct Listing {
    pub files: BTreeMap<String, FileEntry>,
    pub dirs: Vec<String>,
    /// Lines that did not parse — in practice file names with a newline or a
    /// backslash, which GNU sha256sum escapes. Counted, never guessed at.
    pub skipped: usize,
}

pub fn parse_listing(stdout: &str) -> Listing {
    let mut shas: HashMap<String, String> = HashMap::new();
    let mut mtimes: HashMap<String, i64> = HashMap::new();
    let mut out = Listing::default();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("H ") {
            match rest.split_once("  ") {
                Some((h, p)) if h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()) => {
                    shas.insert(p.to_string(), h.to_ascii_lowercase());
                }
                _ => out.skipped += 1,
            }
        } else if let Some(rest) = line.strip_prefix("M ") {
            match rest.split_once(' ').and_then(|(t, p)| t.parse::<i64>().ok().map(|t| (t, p))) {
                Some((t, p)) => { mtimes.insert(p.to_string(), t); }
                None => out.skipped += 1,
            }
        } else if let Some(p) = line.strip_prefix("D ") {
            out.dirs.push(p.to_string());
        } else if !line.is_empty() {
            out.skipped += 1;
        }
    }
    for (path, sha) in shas {
        match mtimes.get(&path) {
            Some(&mtime) => { out.files.insert(path, FileEntry { sha, mtime }); }
            None => out.skipped += 1,
        }
    }
    out
}

fn quoted_list(paths: &[String]) -> String {
    paths.iter().map(|p| sh_quote(p)).collect::<Vec<_>>().join(" ")
}

pub fn read_batch(root: &str, paths: &[String]) -> String {
    format!(
        "{pre}for f in {list}; do\n  if [ -f \"$f\" ]; then printf '@@F %s\\n' \"$f\"; base64 < \"$f\"; printf '\\n'; \
         else printf '@@X %s\\n' \"$f\"; fi\ndone\n",
        pre = prelude(root),
        list = quoted_list(paths),
    )
}

/// `@` is not in the base64 alphabet, so a marker line can never be data.
pub fn parse_read_batch(stdout: &str) -> HashMap<String, Option<Vec<u8>>> {
    fn flush(out: &mut HashMap<String, Option<Vec<u8>>>, cur: Option<(String, String)>) {
        if let Some((path, b64)) = cur {
            let bytes = base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()).ok();
            out.insert(path, bytes);
        }
    }
    let mut out = HashMap::new();
    let mut cur: Option<(String, String)> = None;
    for line in stdout.lines() {
        if let Some(p) = line.strip_prefix("@@F ") {
            flush(&mut out, cur.take());
            cur = Some((p.to_string(), String::new()));
        } else if let Some(p) = line.strip_prefix("@@X ") {
            flush(&mut out, cur.take());
            out.insert(p.to_string(), None);
        } else if let Some((_, buf)) = cur.as_mut() {
            buf.push_str(line.trim());
        }
    }
    flush(&mut out, cur);
    out
}

/// Decode `content` into `"$t"`, then prove the temp file hashes to
/// `new_sha` (computed in Rust) before anything may `mv`/`ln` it into place:
/// a decode cut short by a full disk or a dropped heredoc line would
/// otherwise replace a good note with a truncated one.
fn decode_to_tmp(f: Flavor, content: &[u8], new_sha: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(content);
    let lines: Vec<&str> = b64.as_bytes().chunks(76).map(|c| std::str::from_utf8(c).unwrap()).collect();
    format!(
        "{b64d} > \"$t\" <<'@@JODD_EOF' || {{ rm -f \"$t\"; exit 1; }}\n{body}\n@@JODD_EOF\n\
         [ \"$({sha} \"$t\" | cut -d' ' -f1)\" = {new} ] || {{ rm -f \"$t\"; exit 1; }}\n",
        b64d = f.b64d(),
        body = lines.join("\n"),
        sha = f.sha_cmd(),
        new = sh_quote(new_sha),
    )
}

/// Write `content` to `path` only if the file's current sha is `expected`
/// (`""` = the file must not exist). Temp file then `mv`, so a reader never
/// sees half a note. `new_sha` is `sha256_hex(content)`. Prints the new sha.
/// The window between the check and the `mv` is accepted (spec §3).
pub fn cas_write(root: &str, f: Flavor, path: &str, expected: &str, content: &[u8], new_sha: &str, nonce: &str) -> String {
    format!(
        "{pre}f={p}\ncur=$({sha} \"$f\" 2>/dev/null | cut -d' ' -f1)\n[ \"$cur\" = {exp} ] || exit 3\n\
         mkdir -p \"$(dirname \"$f\")\" .jodd/tmp || exit 1\nt=.jodd/tmp/{n}\n{dec}\
         mv \"$t\" \"$f\" || {{ rm -f \"$t\"; exit 1; }}\n{sha} \"$f\" | cut -d' ' -f1\n",
        pre = prelude(root),
        p = sh_quote(path),
        sha = f.sha_cmd(),
        exp = sh_quote(expected),
        n = sh_quote(nonce),
        dec = decode_to_tmp(f, content, new_sha),
    )
}

/// Create a new file in `dir` named `stem.ext`, or `stem-2.ext`, `stem-3.ext`…
/// `ln` fails if the name exists, so two creators can never overwrite each
/// other. `new_sha` is `sha256_hex(content)`. Prints the path, then the sha.
#[allow(clippy::too_many_arguments)]
pub fn create(root: &str, f: Flavor, dir: &str, stem: &str, ext: &str, content: &[u8], new_sha: &str, nonce: &str) -> String {
    format!(
        "{pre}d={d}; s={s}; e={e}\nmkdir -p \"$d\" .jodd/tmp || exit 1\nt=.jodd/tmp/{n}\n{dec}\
         n=1\nwhile :; do\n  if [ \"$n\" -eq 1 ]; then f=\"$d/$s.$e\"; else f=\"$d/$s-$n.$e\"; fi\n\
           if [ ! -e \"$f\" ] && ln \"$t\" \"$f\" 2>/dev/null; then break; fi\n  n=$((n+1))\n\
           if [ \"$n\" -gt 1000 ]; then rm -f \"$t\"; exit 1; fi\ndone\nrm -f \"$t\"\n\
         printf '%s\\n' \"$f\"\n{sha} \"$f\" | cut -d' ' -f1\n",
        pre = prelude(root),
        d = sh_quote(dir),
        s = sh_quote(stem),
        e = sh_quote(ext),
        n = sh_quote(nonce),
        dec = decode_to_tmp(f, content, new_sha),
        sha = f.sha_cmd(),
    )
}

pub fn parse_create(stdout: &str) -> Option<(String, String)> {
    let mut lines = stdout.lines();
    Some((lines.next()?.to_string(), lines.next()?.trim().to_string()))
}

pub fn remove_if(root: &str, f: Flavor, path: &str, expected: &str) -> String {
    format!(
        "{pre}f={p}\ncur=$({sha} \"$f\" 2>/dev/null | cut -d' ' -f1)\n[ \"$cur\" = {exp} ] || exit 3\nrm -f \"$f\"\n",
        pre = prelude(root),
        p = sh_quote(path),
        sha = f.sha_cmd(),
        exp = sh_quote(expected),
    )
}

/// `name` is unique per call (`super::trash_name`), so a plain `mv` can
/// never overwrite an earlier trashed note.
pub fn trash(root: &str, path: &str, name: &str) -> String {
    format!(
        "{pre}f={p}\n[ -e \"$f\" ] || exit 0\nmkdir -p .jodd/trash || exit 1\nmv \"$f\" .jodd/trash/{n}\n",
        pre = prelude(root),
        p = sh_quote(path),
        n = sh_quote(name),
    )
}

pub fn untrash(root: &str, name: &str, dest: &str) -> String {
    format!(
        "{pre}src=.jodd/trash/{n}\n[ -e \"$src\" ] || exit 4\ndst={d}\n[ -e \"$dst\" ] && exit 3\n\
         mkdir -p \"$(dirname \"$dst\")\" || exit 1\nmv \"$src\" \"$dst\"\n",
        pre = prelude(root),
        n = sh_quote(name),
        d = sh_quote(dest),
    )
}

/// The first 8 KiB of each trashed file is enough for its frontmatter and title.
pub fn list_trash(root: &str) -> String {
    format!(
        "{pre}[ -d .jodd/trash ] || exit 0\nfor f in .jodd/trash/*; do\n  [ -f \"$f\" ] || continue\n\
           printf '@@F %s\\n' \"${{f#.jodd/trash/}}\"; head -c 8192 \"$f\" | base64; printf '\\n'\ndone\n",
        pre = prelude(root)
    )
}

pub fn move_to(root: &str, src: &str, dest_dir: &str) -> String {
    format!(
        "{pre}src={s}; d={d}\n[ -e \"$src\" ] || exit 4\nmkdir -p \"$d\" || exit 1\n\
         b=$(basename \"$src\"); stem=${{b%.*}}; ext=${{b##*.}}\nn=1\n\
         while :; do\n  if [ \"$n\" -eq 1 ]; then f=\"$d/$b\"; else f=\"$d/$stem-$n.$ext\"; fi\n\
           [ -e \"$f\" ] || break\n  n=$((n+1))\ndone\nmv \"$src\" \"$f\" && printf '%s\\n' \"$f\"\n",
        pre = prelude(root),
        s = sh_quote(src),
        d = sh_quote(dest_dir),
    )
}

pub fn mkdir_p(root: &str, dir: &str) -> String {
    format!("{}mkdir -p {}\n", prelude(root), sh_quote(dir))
}

/// Source gone and destination present = already renamed (exit 0); both gone
/// = stale row (exit 4). Both present is a conflict (exit 3): `mv` would
/// nest the source inside the destination.
pub fn rename_dir(root: &str, src: &str, dst: &str) -> String {
    format!(
        "{pre}s={s}; d={d}\nif [ ! -e \"$s\" ]; then [ -e \"$d\" ] && exit 0; exit 4; fi\n\
         [ -e \"$d\" ] && exit 3\nmkdir -p \"$(dirname \"$d\")\" || exit 1\nmv \"$s\" \"$d\"\n",
        pre = prelude(root),
        s = sh_quote(src),
        d = sh_quote(dst),
    )
}

/// Never `rm -rf`: a folder on a shared vault may hold files that are not
/// notes, and notes another device added since this one last pulled. The
/// whole directory moves under `.jodd/trash-dirs/<name>` instead; the caller
/// makes `name` unique and refuses the Notes root.
pub fn trash_dir(root: &str, dir: &str, name: &str) -> String {
    format!(
        "{pre}d={d}\n[ -e \"$d\" ] || exit 0\nmkdir -p .jodd/trash-dirs || exit 1\nmv \"$d\" .jodd/trash-dirs/{n}\n",
        pre = prelude(root),
        d = sh_quote(dir),
        n = sh_quote(name),
    )
}

/// `path \t uuid \t pinned` for every note file, read from the frontmatter
/// block only — a `pinned: true` line in a note's body must not pin it.
pub fn header_scan(root: &str) -> String {
    format!(
        "{pre}[ -d Notes ] || exit 0\nfind Notes {NOTE_FILES} -exec awk '\n\
         {{ sub(/\\r$/, \"\") }}\n\
         FNR == 1 {{ infm = ($0 == \"---\"); u = \"\"; p = \"false\"; if (!infm) print FILENAME \"\\t\\tfalse\"; next }}\n\
         infm && $0 == \"---\" {{ print FILENAME \"\\t\" u \"\\t\" p; infm = 0; next }}\n\
         infm && /^uuid:/ {{ v = $0; sub(/^uuid:[ \\t]*/, \"\", v); u = v; next }}\n\
         infm && /^pinned:/ {{ v = $0; sub(/^pinned:[ \\t]*/, \"\", v); p = v; next }}\n\
         ' {{}} +\n",
        pre = prelude(root)
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub path: String,
    pub uuid: Option<String>,
    pub pinned: bool,
}

pub fn parse_headers(stdout: &str) -> Vec<Header> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let path = parts.next()?.to_string();
            let uuid = parts.next()?.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
            let pinned = parts.next()?.trim().trim_matches(|c| c == '"' || c == '\'').eq_ignore_ascii_case("true");
            Some(Header { path, uuid: (!uuid.is_empty()).then_some(uuid), pinned })
        })
        .collect()
}

/// The `ssh-ed25519 AAAA...` fields of a public key line, without the
/// trailing `user@host` comment ssh-keygen appends. Two key lines with
/// different comments but the same body are the same key.
pub fn key_body(pubkey_line: &str) -> &str {
    let line = pubkey_line.trim();
    match line.split_whitespace().take(2).collect::<Vec<_>>()[..] {
        [alg, body] => {
            let end = alg.len() + 1 + body.len();
            &line[..end]
        }
        _ => line,
    }
}

/// Idempotent: creates `~/.ssh` (700) and `authorized_keys` (600) if
/// missing, then appends `pubkey_line` only if its key body is not already
/// present. Run once, over a password-authenticated connection, during
/// Managed SSH setup — never as part of `SshVertical`'s ongoing script
/// suite.
pub fn install_authorized_key(pubkey_line: &str) -> String {
    let quoted_line = sh_quote(pubkey_line.trim());
    let quoted_body = sh_quote(key_body(pubkey_line));
    format!(
        "umask 077\n\
         mkdir -p ~/.ssh\n\
         touch ~/.ssh/authorized_keys\n\
         chmod 700 ~/.ssh\n\
         chmod 600 ~/.ssh/authorized_keys\n\
         if ! grep -qF {quoted_body} ~/.ssh/authorized_keys 2>/dev/null; then\n\
         printf '%s\\n' {quoted_line} >> ~/.ssh/authorized_keys\n\
         fi\n"
    )
}

/// Removes only the line whose key body matches `pubkey_body`. The temp file
/// is created in the SAME directory as `authorized_keys` (never a bare
/// `mktemp`, which can land on a different filesystem — a cross-filesystem
/// `mv` silently degrades to a non-atomic copy+unlink) and explicitly
/// `chmod 600`'d before the move, since a platform's `mktemp` default mode is
/// not guaranteed. The final `mv` is then a same-directory, same-filesystem
/// rename: atomic on POSIX, so a reader (including sshd evaluating this file
/// mid-login) never observes a half-written or truncated file, and a kill
/// between steps leaves either the old file or the new one, never neither.
/// Exits 0 whether or not a matching line existed.
///
/// Refuses a body that is not a plausible `<algorithm> <base64>` pair:
/// `grep -vF ''` matches EVERY line, so an empty body (an empty or truncated
/// `.pub` file) would otherwise rewrite `authorized_keys` as an empty file and
/// lock the user out with every key they own. For the same reason grep's
/// status is checked: 0 (lines kept) and 1 (nothing left) are results, but 2
/// is an error, and a failed grep's partial output must never replace the file.
pub fn revoke_authorized_key(pubkey_body: &str) -> Result<String, String> {
    if !is_plausible_key_body(pubkey_body) {
        return Err(format!("refusing to revoke an implausible key body ({} chars)", pubkey_body.len()));
    }
    let quoted_body = sh_quote(pubkey_body);
    Ok(format!(
        "f=\"$HOME/.ssh/authorized_keys\"\n\
         [ -f \"$f\" ] || exit 0\n\
         tmp=$(mktemp \"$HOME/.ssh/authorized_keys.XXXXXX\") || exit 1\n\
         grep -vF {quoted_body} \"$f\" > \"$tmp\"\n\
         rc=$?\n\
         if [ \"$rc\" -gt 1 ]; then rm -f \"$tmp\"; exit 1; fi\n\
         chmod 600 \"$tmp\"\n\
         mv \"$tmp\" \"$f\"\n"
    ))
}

/// `ssh-ed25519 AAAA…` / `ecdsa-sha2-… AAAA…` / `sk-…`: an algorithm name and
/// a base64 blob long enough that it cannot be a substring of an unrelated
/// line by accident.
fn is_plausible_key_body(body: &str) -> bool {
    let fields: Vec<&str> = body.split_whitespace().collect();
    let [alg, blob] = fields[..] else { return false };
    let alg_ok = ["ssh-", "ecdsa-", "sk-"].iter().any(|p| alg.starts_with(p));
    let blob_ok = blob.len() >= 32 && blob.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=');
    alg_ok && blob_ok
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::backend::ssh::session::{ProcessSession, SpawnSpec, SshSession};
    use std::time::Duration;

    fn has(tool: &str) -> bool {
        std::process::Command::new("sh").arg("-c").arg(format!("command -v {tool}")).output().map(|o| o.status.success()).unwrap_or(false)
    }

    /// The flavours this machine can run. CI (ubuntu) has both tools; a CI
    /// run that could only test one would be a guard skipping itself.
    fn flavors() -> Vec<Flavor> {
        let mut v = Vec::new();
        let stat = if std::process::Command::new("stat").args(["-c", "%Y", "/"]).output().map(|o| o.status.success()).unwrap_or(false) { Stat::Gnu } else { Stat::Bsd };
        let upper = !std::process::Command::new("sh").arg("-c").arg("printf aGk= | base64 -d").output().map(|o| o.status.success()).unwrap_or(false);
        if has("sha256sum") { v.push(Flavor { sha: Sha::Sha256sum, stat, b64_decode_upper: upper }); }
        if has("shasum") { v.push(Flavor { sha: Sha::Shasum, stat, b64_decode_upper: upper }); }
        if std::env::var_os("CI").is_some() {
            assert_eq!(v.len(), 2, "CI must exercise both sha256sum and shasum");
        }
        assert!(!v.is_empty(), "no sha256 tool on this machine");
        v
    }

    async fn run(s: &ProcessSession, script: &str) -> crate::backend::ssh::session::ExecOutput {
        s.exec(script).await.unwrap()
    }

    fn session(dir: &std::path::Path) -> ProcessSession {
        ProcessSession::new(SpawnSpec::local_sh(dir.to_path_buf()), Duration::from_secs(20))
    }

    #[tokio::test]
    async fn the_probe_reads_this_machine() {
        let d = tempfile::tempdir().unwrap();
        let s = session(d.path());
        let f = parse_probe(&run(&s, PROBE).await.stdout).unwrap();
        assert!(flavors().iter().any(|x| x.sha == f.sha));
        assert_eq!(parse_probe("sha=none\nstat=gnu\nb64=d\n").unwrap_err(), "the server has neither sha256sum nor shasum");
    }

    #[tokio::test]
    async fn resolve_root_expands_tilde_creates_and_refuses() {
        let d = tempfile::tempdir().unwrap();
        let s = session(d.path());
        let want = d.path().join("vault");
        let o = run(&s, &resolve_root(want.to_str().unwrap(), false)).await;
        assert_eq!(o.exit, 4, "missing and create=false");
        let o = run(&s, &resolve_root(want.to_str().unwrap(), true)).await;
        assert_eq!(o.exit, 0);
        assert_eq!(o.stdout.trim(), want.canonicalize().unwrap().to_str().unwrap());
        let o = run(&s, &format!("HOME={} ; {}", sh_quote(d.path().to_str().unwrap()), resolve_root("~/vault", false))).await;
        assert_eq!(o.stdout.trim(), want.canonicalize().unwrap().to_str().unwrap());
    }

    #[tokio::test]
    async fn listing_hashes_every_note_file_and_nothing_else() {
        for f in flavors() {
            let d = tempfile::tempdir().unwrap();
            let root = d.path().to_str().unwrap().to_string();
            std::fs::create_dir_all(d.path().join("Notes/Work")).unwrap();
            std::fs::write(d.path().join("Notes/a.md"), "hello").unwrap();
            std::fs::write(d.path().join("Notes/Work/b.html"), "<div>x</div>").unwrap();
            std::fs::write(d.path().join("Notes/Work/ignore.txt"), "no").unwrap();
            std::fs::write(d.path().join("Notes/it's $(x).md"), "q").unwrap();
            let s = session(d.path());
            let l = parse_listing(&run(&s, &list(&root, f, None)).await.stdout);
            assert_eq!(l.files.keys().cloned().collect::<Vec<_>>(), vec!["Notes/Work/b.html", "Notes/a.md", "Notes/it's $(x).md"], "{f:?}");
            assert_eq!(l.files["Notes/a.md"].sha, sha256_hex(b"hello"), "{f:?}");
            assert!(l.files["Notes/a.md"].mtime > 1_600_000_000);
            assert!(l.dirs.contains(&"Notes/Work".to_string()));
            let scoped = parse_listing(&run(&s, &list(&root, f, Some("Notes"))).await.stdout);
            assert!(!scoped.files.contains_key("Notes/Work/b.html"), "folder listing is one level");
        }
    }

    #[tokio::test]
    async fn an_empty_vault_lists_nothing() {
        let f = flavors()[0];
        let d = tempfile::tempdir().unwrap();
        let s = session(d.path());
        let o = run(&s, &list(d.path().to_str().unwrap(), f, None)).await;
        assert_eq!(o.exit, 0);
        assert!(parse_listing(&o.stdout).files.is_empty());
    }

    #[tokio::test]
    async fn a_missing_root_is_exit_97() {
        let f = flavors()[0];
        let d = tempfile::tempdir().unwrap();
        let s = session(d.path());
        assert_eq!(run(&s, &list("/nonexistent/jodd", f, None)).await.exit, 97);
    }

    #[tokio::test]
    async fn read_batch_returns_bytes_and_marks_missing_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("Notes")).unwrap();
        let body = "line @@F fake\n\u{0e44}\n".repeat(200);
        std::fs::write(d.path().join("Notes/a.md"), &body).unwrap();
        std::fs::write(d.path().join("Notes/empty.md"), "").unwrap();
        let s = session(d.path());
        let paths = vec!["Notes/a.md".to_string(), "Notes/empty.md".to_string(), "Notes/gone.md".to_string()];
        let got = parse_read_batch(&run(&s, &read_batch(d.path().to_str().unwrap(), &paths)).await.stdout);
        assert_eq!(got["Notes/a.md"].as_deref(), Some(body.as_bytes()));
        assert_eq!(got["Notes/empty.md"].as_deref(), Some(&b""[..]));
        assert_eq!(got["Notes/gone.md"], None);
    }

    #[tokio::test]
    async fn cas_write_creates_updates_and_refuses_a_stale_version() {
        for f in flavors() {
            let d = tempfile::tempdir().unwrap();
            let root = d.path().to_str().unwrap();
            let s = session(d.path());
            let o = run(&s, &cas_write(root, f, "Notes/x/n.md", "", b"one", &sha256_hex(b"one"), "t1")).await;
            assert_eq!((o.exit, o.stdout.trim()), (0, sha256_hex(b"one").as_str()), "{f:?} {}", o.stderr);
            let o = run(&s, &cas_write(root, f, "Notes/x/n.md", "", b"two", &sha256_hex(b"two"), "t2")).await;
            assert_eq!(o.exit, 3, "create over an existing file is a conflict");
            let o = run(&s, &cas_write(root, f, "Notes/x/n.md", &sha256_hex(b"one"), b"two", &sha256_hex(b"two"), "t3")).await;
            assert_eq!(o.exit, 0);
            assert_eq!(std::fs::read(d.path().join("Notes/x/n.md")).unwrap(), b"two");
            let o = run(&s, &cas_write(root, f, "Notes/x/n.md", &sha256_hex(b"one"), b"three", &sha256_hex(b"three"), "t4")).await;
            assert_eq!(o.exit, 3, "stale expected version");
            assert_eq!(std::fs::read(d.path().join("Notes/x/n.md")).unwrap(), b"two", "a refused write changes nothing");
            assert_eq!(std::fs::read_dir(d.path().join(".jodd/tmp")).unwrap().count(), 0, "no temp file left behind");
        }
    }

    #[tokio::test]
    async fn create_never_overwrites_and_numbers_collisions() {
        let f = flavors()[0];
        let d = tempfile::tempdir().unwrap();
        let root = d.path().to_str().unwrap();
        let s = session(d.path());
        let a = parse_create(&run(&s, &create(root, f, "Notes/W", "meeting", "md", b"a", &sha256_hex(b"a"), "n1")).await.stdout).unwrap();
        let b = parse_create(&run(&s, &create(root, f, "Notes/W", "meeting", "md", b"b", &sha256_hex(b"b"), "n2")).await.stdout).unwrap();
        assert_eq!(a.0, "Notes/W/meeting.md");
        assert_eq!(b.0, "Notes/W/meeting-2.md");
        assert_eq!(b.1, sha256_hex(b"b"));
        assert_eq!(std::fs::read(d.path().join("Notes/W/meeting.md")).unwrap(), b"a");
    }

    #[tokio::test]
    async fn trash_untrash_and_move_round_trip() {
        let f = flavors()[0];
        let d = tempfile::tempdir().unwrap();
        let root = d.path().to_str().unwrap();
        let s = session(d.path());
        run(&s, &create(root, f, "Notes/A", "n", "md", b"x", &sha256_hex(b"x"), "c")).await;
        assert_eq!(run(&s, &trash(root, "Notes/A/n.md", "Notes%2FA%2Fn.md")).await.exit, 0);
        assert!(d.path().join(".jodd/trash/Notes%2FA%2Fn.md").exists());
        let listed = parse_read_batch(&run(&s, &list_trash(root)).await.stdout);
        assert_eq!(listed["Notes%2FA%2Fn.md"].as_deref(), Some(&b"x"[..]));
        assert_eq!(run(&s, &untrash(root, "Notes%2FA%2Fn.md", "Notes/A/n.md")).await.exit, 0);
        let o = run(&s, &move_to(root, "Notes/A/n.md", "Notes/B")).await;
        assert_eq!((o.exit, o.stdout.trim()), (0, "Notes/B/n.md"));
        assert_eq!(run(&s, &untrash(root, "nope", "Notes/A/n.md")).await.exit, 4);
        assert_eq!(run(&s, &remove_if(root, f, "Notes/B/n.md", "0000")).await.exit, 3);
        assert_eq!(run(&s, &remove_if(root, f, "Notes/B/n.md", &sha256_hex(b"x"))).await.exit, 0);
        assert!(!d.path().join("Notes/B/n.md").exists());
    }

    /// The destination existing means "someone else already made that
    /// folder" — `mv` would nest the source inside it instead.
    #[tokio::test]
    async fn rename_dir_refuses_an_existing_destination() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().to_str().unwrap();
        std::fs::create_dir_all(d.path().join("Notes/A")).unwrap();
        std::fs::create_dir_all(d.path().join("Notes/B")).unwrap();
        let s = session(d.path());
        assert_eq!(run(&s, &rename_dir(root, "Notes/A", "Notes/B")).await.exit, 3);
        assert!(d.path().join("Notes/A").is_dir(), "source untouched");
        assert!(!d.path().join("Notes/B/A").exists(), "never nested");
    }

    #[tokio::test]
    async fn trash_dir_moves_the_whole_directory_aside() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().to_str().unwrap();
        std::fs::create_dir_all(d.path().join("Notes/A")).unwrap();
        std::fs::write(d.path().join("Notes/A/keep.txt"), "k").unwrap();
        let s = session(d.path());
        assert_eq!(run(&s, &trash_dir(root, "Notes/A", "n1-Notes%2FA")).await.exit, 0);
        assert!(!d.path().join("Notes/A").exists());
        assert_eq!(std::fs::read(d.path().join(".jodd/trash-dirs/n1-Notes%2FA/keep.txt")).unwrap(), b"k");
        assert_eq!(run(&s, &trash_dir(root, "Notes/A", "n2-Notes%2FA")).await.exit, 0, "already gone");
    }

    /// C-2: the decoded temp file is checked against the sha Rust computed
    /// before it may replace anything. A wrong expected sha stands in for a
    /// truncated decode (full disk, a dropped heredoc line).
    #[tokio::test]
    async fn a_decode_that_does_not_hash_to_the_new_sha_replaces_nothing() {
        for f in flavors() {
            let d = tempfile::tempdir().unwrap();
            let root = d.path().to_str().unwrap();
            let s = session(d.path());
            let o = run(&s, &cas_write(root, f, "Notes/n.md", "", b"one", &sha256_hex(b"one"), "t1")).await;
            assert_eq!(o.exit, 0, "{f:?} {}", o.stderr);
            let o = run(&s, &cas_write(root, f, "Notes/n.md", &sha256_hex(b"one"), b"two", &sha256_hex(b"wrong"), "t2")).await;
            assert_eq!(o.exit, 1, "{f:?}");
            assert_eq!(std::fs::read(d.path().join("Notes/n.md")).unwrap(), b"one", "{f:?} original kept");
            let o = run(&s, &create(root, f, "Notes", "c", "md", b"x", &sha256_hex(b"wrong"), "t3")).await;
            assert_eq!(o.exit, 1, "{f:?}");
            assert!(!d.path().join("Notes/c.md").exists(), "{f:?} nothing created");
            assert_eq!(std::fs::read_dir(d.path().join(".jodd/tmp")).unwrap().count(), 0, "{f:?} no temp file left behind");
        }
    }

    #[tokio::test]
    async fn header_scan_reads_frontmatter_only() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("Notes")).unwrap();
        std::fs::write(d.path().join("Notes/a.md"), "---\r\nuuid: \"AbC-1\"\r\npinned: true\r\n---\r\n# A\n").unwrap();
        std::fs::write(d.path().join("Notes/b.md"), "# B\npinned: true\nuuid: nope\n").unwrap();
        let s = session(d.path());
        let mut h = parse_headers(&run(&s, &header_scan(d.path().to_str().unwrap())).await.stdout);
        h.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(h, vec![
            Header { path: "Notes/a.md".into(), uuid: Some("abc-1".into()), pinned: true },
            Header { path: "Notes/b.md".into(), uuid: None, pinned: false },
        ]);
    }

    #[tokio::test]
    async fn list_dirs_lists_visible_subdirectories() {
        let d = tempfile::tempdir().unwrap();
        for sub in ["b", "a", ".hidden"] { std::fs::create_dir_all(d.path().join(sub)).unwrap(); }
        std::fs::write(d.path().join("file"), "").unwrap();
        let s = session(d.path());
        let (abs, dirs) = parse_dirs(&run(&s, &list_dirs(d.path().to_str().unwrap())).await.stdout).unwrap();
        assert_eq!(abs, d.path().canonicalize().unwrap().to_str().unwrap());
        assert_eq!(dirs, vec!["a", "b"]);
    }
}

#[cfg(all(test, unix))]
mod key_scripts_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn run_local(dir: &std::path::Path, script: &str) -> (String, String, i32) {
        let out = Command::new("sh")
            .arg("-c").arg(script)
            .env("HOME", dir)
            .output()
            .unwrap();
        (String::from_utf8_lossy(&out.stdout).into_owned(),
         String::from_utf8_lossy(&out.stderr).into_owned(),
         out.status.code().unwrap_or(-1))
    }

    const KEY_A: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleA jodd-1@laptop";
    const KEY_B: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleB jodd-2@desktop";

    #[test]
    fn key_body_strips_the_trailing_comment() {
        assert_eq!(key_body(KEY_A), "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExampleA");
    }

    #[test]
    fn install_creates_ssh_dir_at_700_and_key_at_600() {
        let d = tempfile::tempdir().unwrap();
        let (_, err, code) = run_local(d.path(), &install_authorized_key(KEY_A));
        assert_eq!(code, 0, "{err}");
        let ssh_dir = d.path().join(".ssh");
        let ak = ssh_dir.join("authorized_keys");
        assert_eq!(std::fs::metadata(&ssh_dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&ak).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(std::fs::read_to_string(&ak).unwrap().contains(key_body(KEY_A)));
    }

    #[test]
    fn install_is_idempotent() {
        let d = tempfile::tempdir().unwrap();
        run_local(d.path(), &install_authorized_key(KEY_A));
        run_local(d.path(), &install_authorized_key(KEY_A));
        let ak = d.path().join(".ssh").join("authorized_keys");
        let content = std::fs::read_to_string(&ak).unwrap();
        assert_eq!(content.matches(key_body(KEY_A)).count(), 1, "{content}");
    }

    #[test]
    fn install_does_not_disturb_an_unrelated_existing_key() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".ssh")).unwrap();
        std::fs::write(d.path().join(".ssh/authorized_keys"), format!("{KEY_B}\n")).unwrap();
        run_local(d.path(), &install_authorized_key(KEY_A));
        let content = std::fs::read_to_string(d.path().join(".ssh/authorized_keys")).unwrap();
        assert!(content.contains(key_body(KEY_A)) && content.contains(key_body(KEY_B)));
    }

    #[test]
    fn revoke_removes_only_the_matching_line() {
        let d = tempfile::tempdir().unwrap();
        run_local(d.path(), &install_authorized_key(KEY_A));
        run_local(d.path(), &install_authorized_key(KEY_B));
        let (_, err, code) = run_local(d.path(), &revoke_authorized_key(key_body(KEY_A)).unwrap());
        assert_eq!(code, 0, "{err}");
        let content = std::fs::read_to_string(d.path().join(".ssh/authorized_keys")).unwrap();
        assert!(!content.contains(key_body(KEY_A)) && content.contains(key_body(KEY_B)));
    }

    #[test]
    fn revoke_of_an_absent_key_is_a_harmless_no_op() {
        let d = tempfile::tempdir().unwrap();
        run_local(d.path(), &install_authorized_key(KEY_B));
        let (_, err, code) = run_local(d.path(), &revoke_authorized_key(key_body(KEY_A)).unwrap());
        assert_eq!(code, 0, "{err}");
        assert!(std::fs::read_to_string(d.path().join(".ssh/authorized_keys")).unwrap().contains(key_body(KEY_B)));
    }

    /// Revoking the ONLY key must leave a present, still-600, empty file —
    /// not a missing or corrupted one. This is also the atomic-rewrite path's
    /// sharpest edge case: the file goes from one line to zero, so a
    /// non-atomic rewrite (truncate-then-write) would have the widest window
    /// in which a concurrent reader (or a kill) could observe a wiped file.
    #[test]
    fn revoke_of_the_only_key_leaves_an_empty_but_valid_file() {
        let d = tempfile::tempdir().unwrap();
        run_local(d.path(), &install_authorized_key(KEY_A));
        let (_, err, code) = run_local(d.path(), &revoke_authorized_key(key_body(KEY_A)).unwrap());
        assert_eq!(code, 0, "{err}");
        let ak = d.path().join(".ssh/authorized_keys");
        assert!(ak.exists(), "authorized_keys must still exist after revoking its only key");
        assert_eq!(std::fs::metadata(&ak).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&ak).unwrap(), "", "file is empty, not missing");
    }

    /// `grep -vF ''` matches every line: an empty body from an empty or
    /// truncated `.pub` file would have emptied authorized_keys, the user's
    /// own keys included. No script is produced for it at all.
    #[test]
    fn revoke_refuses_an_empty_or_implausible_body() {
        for bad in ["", "   ", "ssh-ed25519", "ssh-ed25519 AAAA", "not-a-key AAAAC3NzaC1lZDI1NTE5AAAAIExampleA", "ssh-ed25519 AAAA C3Nz"] {
            assert!(revoke_authorized_key(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(revoke_authorized_key(key_body(KEY_A)).is_ok());
    }

    /// A grep that fails (exit 2, here an unreadable file) must leave
    /// authorized_keys exactly as it was, not replaced by grep's empty output.
    #[test]
    fn revoke_leaves_the_file_untouched_when_grep_fails() {
        let d = tempfile::tempdir().unwrap();
        run_local(d.path(), &install_authorized_key(KEY_A));
        run_local(d.path(), &install_authorized_key(KEY_B));
        let ak = d.path().join(".ssh/authorized_keys");
        let before = std::fs::read_to_string(&ak).unwrap();
        std::fs::set_permissions(&ak, std::fs::Permissions::from_mode(0o000)).unwrap();
        let (_, _, code) = run_local(d.path(), &revoke_authorized_key(key_body(KEY_A)).unwrap());
        std::fs::set_permissions(&ak, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_ne!(code, 0, "a failed grep must fail the revoke");
        assert_eq!(std::fs::read_to_string(&ak).unwrap(), before, "authorized_keys must be unchanged");
        let leftovers: Vec<_> = std::fs::read_dir(d.path().join(".ssh")).unwrap()
            .filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("authorized_keys.")).collect();
        assert!(leftovers.is_empty(), "temp file must be cleaned up: {leftovers:?}");
    }
}
