//! The wire protocol between Jodd and a remote POSIX `sh`, shared by both
//! transports: `ProcessSession` reads it from a child's stdout as it streams,
//! `RusshSession` from a channel's collected stdout. ONE parser, so a framing
//! fix (I-1's banner, the no-trailing-newline marker) lands on both platforms.

use base64::Engine as _;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use super::session::{ExecOutput, SshError};

/// Wrap `script` so its stdout, stderr and exit code come back
/// distinguishable. The BEGIN marker comes first because `ssh host sh` runs
/// the login user's shell startup, and a MOTD or `.bashrc` echo would
/// otherwise be the start of the first response. It is preceded by a newline
/// so a banner that does not end in one cannot put the marker mid-line. `</dev/null` keeps a script that reads stdin from eating
/// the command stream; heredocs inside `script` still work because the shell
/// reads their bodies while parsing.
pub(crate) fn frame(script: &str, nonce: &str) -> String {
    format!(
        "printf '\\n@@JODD_BEGIN_{nonce}\\n'\n__jodd_e=$(mktemp)\n( {script}\n) </dev/null 2>\"$__jodd_e\"\n__jodd_s=$?\n\
         printf '\\n@@JODD_ERR_{nonce}\\n'\nbase64 < \"$__jodd_e\"\nrm -f \"$__jodd_e\"\n\
         printf '@@JODD_END_{nonce} %d\\n' \"$__jodd_s\"\n"
    )
}

fn disconnected() -> SshError {
    SshError::Disconnected { stderr: String::new() }
}

async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<Vec<u8>, SshError> {
    let mut line = Vec::new();
    let n = r.read_until(b'\n', &mut line).await.map_err(|_| disconnected())?;
    if n == 0 {
        return Err(disconnected());
    }
    Ok(line)
}

fn trimmed(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r') {
        end -= 1;
    }
    &line[..end]
}

/// Read one framed reply: skip everything before the BEGIN marker (shell
/// startup output), then stdout up to ERR, base64 stderr up to END, and the
/// exit code on END's line.
pub(crate) async fn read_framed<R: AsyncBufRead + Unpin>(r: &mut R, nonce: &str) -> Result<ExecOutput, SshError> {
    let begin_marker = format!("@@JODD_BEGIN_{nonce}");
    let err_marker = format!("@@JODD_ERR_{nonce}");
    let end_prefix = format!("@@JODD_END_{nonce} ");
    while trimmed(&read_line(r).await?) != begin_marker.as_bytes() {}
    let mut stdout = Vec::new();
    loop {
        let line = read_line(r).await?;
        if trimmed(&line) == err_marker.as_bytes() {
            break;
        }
        stdout.extend_from_slice(&line);
    }
    // `frame` prints a newline before the marker so the marker always starts
    // a line; that newline is not the script's.
    if stdout.last() == Some(&b'\n') {
        stdout.pop();
    }
    let mut b64 = String::new();
    let exit = loop {
        let line = read_line(r).await?;
        let text = String::from_utf8_lossy(trimmed(&line)).into_owned();
        if let Some(code) = text.strip_prefix(&end_prefix) {
            break code
                .trim()
                .parse::<i32>()
                .map_err(|_| SshError::Protocol(format!("bad exit line: {text}")))?;
        }
        b64.push_str(text.trim());
    };
    let stderr = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| SshError::Protocol(format!("stderr is not base64: {e}")))?;
    Ok(ExecOutput {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(nonce: &str, banner: &str, stdout: &str, stderr: &str, exit: i32) -> Vec<u8> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(stderr);
        format!("{banner}\n@@JODD_BEGIN_{nonce}\n{stdout}\n@@JODD_ERR_{nonce}\n{b64}\n@@JODD_END_{nonce} {exit}\n").into_bytes()
    }

    #[tokio::test]
    async fn a_complete_reply_parses_from_any_buffered_reader() {
        let bytes = reply("n1", "Welcome to the box", "hi\n", "oops\n", 7);
        let out = read_framed(&mut &bytes[..], "n1").await.unwrap();
        assert_eq!(out, ExecOutput { stdout: "hi\n".into(), stderr: "oops\n".into(), exit: 7 });
    }

    #[tokio::test]
    async fn a_truncated_reply_is_a_disconnect_not_a_hang() {
        let bytes = b"@@JODD_BEGIN_n1\nhalf a li".to_vec();
        assert!(matches!(read_framed(&mut &bytes[..], "n1").await, Err(SshError::Disconnected { .. })));
    }

    #[test]
    fn frame_runs_the_script_in_a_subshell_with_no_stdin() {
        let f = frame("echo hi", "n1");
        assert!(f.contains("( echo hi\n) </dev/null"), "{f}");
        assert!(f.starts_with("printf '\\n@@JODD_BEGIN_n1\\n'"), "{f}");
    }
}
