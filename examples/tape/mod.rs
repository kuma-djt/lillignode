//! Development tool, not part of the product: tapes.
//!
//! A tape is what a real agent wrote at one moment, recorded once so the
//! checks can replay it with no agent running. It is a file of JSON lines.
//! The first describes it:
//! `{"tape":1,"agent":"claude-code","version":"2.1.332","cols":100,"rows":40}`.
//! Each line after is one thing that happened, with the milliseconds since
//! the line before:
//!
//! - `{"ms":12,"out":"<hex>"}`: what the agent wrote;
//! - `{"ms":900,"key":"<hex>"}`: what a person pressed or pasted;
//! - `{"ms":0,"answer":"<hex>"}`: what the terminal answered by itself
//!   (the status question, the size); a player does not wait for these;
//! - `{"ms":0,"moment":"needs-you"}`: a name for what is happening, for
//!   whoever reads the tape.
//!
//! `play` writes what the agent wrote, in order and at its pace
//! (`STANDIN_TAPE_FAST` caps every pause at 50 ms, `STANDIN_TAPE_CAP_MS` at
//! any number of milliseconds). At a key it waits until
//! that key has arrived, whatever else arrives first; a terminal's own
//! answers are never taken for a person's key. It cannot do anything the
//! agent did not do while it was recorded. Used by the stand-in
//! (`standin play <tape>`).
#![allow(dead_code)]

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2).filter_map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()).collect()
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Input cut into keys: one escape sequence, one paste or one character
/// each. A paste's line breaks are made alike, since a terminal may send
/// either.
pub fn keys(bytes: &[u8]) -> Vec<Vec<u8>> {
    cut(bytes).into_iter().flat_map(from_windows_key).collect()
}

/// A key in Windows' own form (`ESC [ Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`), which
/// a Windows console sends once a program has asked for it, as the
/// character it stands for. A key let go, or a key with no character
/// (Shift alone), is nothing.
fn from_windows_key(key: Vec<u8>) -> Vec<Vec<u8>> {
    if !(key.starts_with(b"\x1b[") && key.ends_with(b"_")) {
        return vec![key];
    }
    let params: Vec<u32> =
        String::from_utf8_lossy(&key[2..key.len() - 1]).split(';').map(|p| p.parse().unwrap_or(0)).collect();
    if params.len() < 4 || params[3] != 1 || params[2] == 0 {
        return Vec::new();
    }
    let Some(c) = char::from_u32(params[2]) else { return Vec::new() };
    let mut b = [0u8; 4];
    let one = c.encode_utf8(&mut b).as_bytes().to_vec();
    vec![one; params.get(5).copied().unwrap_or(1).max(1) as usize]
}

/// What a Windows console asks of the terminal it writes to: that keys be
/// sent in Windows' own form. In a tape it was the recording console asking
/// the recorder. Replayed as a program's output inside another console, it
/// would be that program asking its own console, which is not what the
/// agent did; so the player leaves it out.
pub fn without_console_requests(bytes: &[u8]) -> Vec<u8> {
    const ASK: &[u8] = b"\x1b[?9001h";
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(ASK) {
            i += ASK.len();
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

fn cut(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i..].starts_with(PASTE_START) {
            let body = &bytes[i + PASTE_START.len()..];
            let end = body.windows(PASTE_END.len()).position(|w| w == PASTE_END).unwrap_or(body.len());
            let mut paste = PASTE_START.to_vec();
            let mut j = 0;
            while j < end {
                if body[j] == b'\r' && body.get(j + 1) == Some(&b'\n') {
                    j += 1;
                }
                paste.push(if body[j] == b'\n' { b'\r' } else { body[j] });
                j += 1;
            }
            paste.extend_from_slice(PASTE_END);
            out.push(paste);
            i += PASTE_START.len() + (end + PASTE_END.len()).min(body.len());
            continue;
        }
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            i += 2;
            while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                i += 1;
            }
            i = (i + 1).min(bytes.len());
        } else if bytes[i] == 0x1b && matches!(bytes.get(i + 1), Some(&b']') | Some(&b'P')) {
            i += 2;
            while i < bytes.len() && bytes[i] != 0x07 && !bytes[i..].starts_with(b"\x1b\\") {
                i += 1;
            }
            i = (i + if bytes[i..].starts_with(b"\x1b\\") { 2 } else { 1 }).min(bytes.len());
        } else if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'O') && i + 2 < bytes.len() {
            i += 3;
        } else if bytes[i] < 0x80 {
            i += 1;
        } else {
            i += 1;
            while i < bytes.len() && bytes[i] & 0xc0 == 0x80 {
                i += 1;
            }
        }
        out.push(bytes[start..i].to_vec());
    }
    out
}

/// True for what a terminal sends by itself, never a person: answers to
/// questions (its kind, where the cursor is, its size, its modes), status
/// answers, and focus coming and going.
pub fn is_answer(key: &[u8]) -> bool {
    if key.starts_with(b"\x1b]") {
        return true;
    }
    if !key.starts_with(b"\x1b[") || key.len() < 3 || key.starts_with(PASTE_START) {
        return false;
    }
    let last = key[key.len() - 1];
    let body = &key[2..key.len() - 1];
    match last {
        b'I' | b'O' => body.is_empty(),
        b'R' => body.contains(&b';') && !body.starts_with(b"1;"),
        b'c' | b'n' | b't' | b'y' => true,
        b'~' => false,
        _ => body.first().is_some_and(|b| *b == b'?' || *b == b'>'),
    }
}

/// One key as matched: its bytes, and whether it came inside a paste.
type Spelt = (Vec<u8>, bool);

/// A paste as the characters it holds, each marked as pasted, so a paste
/// matches the same text however it arrived, and how it arrived can still
/// be said. (On Windows a marked paste can reach the agent with its first
/// character typed and the rest pasted: the console takes the markers out
/// and hands over the first key on its own.) Each item also says which key
/// of `keys` it came from.
fn spelt(keys: &[Vec<u8>]) -> Vec<(Spelt, usize)> {
    keys.iter()
        .enumerate()
        .flat_map(|(i, k)| match k.strip_prefix(PASTE_START).and_then(|k| k.strip_suffix(PASTE_END)) {
            Some(text) => cut(text).into_iter().map(|c| ((c, true), i)).collect::<Vec<_>>(),
            None => vec![((k.clone(), false), i)],
        })
        .collect()
}

/// How a key the tape waited for arrived.
#[derive(Debug, PartialEq, Eq)]
pub enum Taken {
    /// Not (yet) all there.
    No,
    /// All there. For a paste: how many of its characters came typed, and
    /// whether those were only its first.
    Yes { pasted_typed: usize, only_first: bool },
}

/// Takes `want` out of `got` if it is there in order, with anything else
/// before or between dropped, and says how a paste in it arrived.
pub fn take(got: &mut Vec<Vec<u8>>, want: &[Vec<u8>]) -> Taken {
    let want = spelt(want);
    if want.is_empty() {
        return Taken::Yes { pasted_typed: 0, only_first: true };
    }
    let have = spelt(got);
    let mut at = 0;
    let (mut typed, mut only_first) = (0, true);
    for (j, ((key, came_pasted), from)) in have.iter().enumerate() {
        let ((wanted, want_pasted), _) = &want[at];
        if key != wanted {
            continue;
        }
        if *want_pasted && !came_pasted {
            typed += 1;
            // Typed, a paste's character may only be its first.
            only_first &= at == 0 || !want[at - 1].0 .1;
        }
        at += 1;
        if at == want.len() {
            // What is left of a key partly used (a paste) goes back as its
            // characters; the keys after it stay as they came.
            let mut rest: Vec<Vec<u8>> =
                have[j + 1..].iter().take_while(|(_, f)| f == from).map(|((k, _), _)| k.clone()).collect();
            rest.extend(got.drain(from + 1..));
            *got = rest;
            return Taken::Yes { pasted_typed: typed, only_first };
        }
    }
    Taken::No
}

/// How much of `bytes` is whole keys: what is left after is the start of
/// an escape sequence, a paste or a character, and waits for the rest.
fn whole(bytes: &[u8]) -> usize {
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        if rest.starts_with(PASTE_START) {
            match rest.windows(PASTE_END.len()).position(|w| w == PASTE_END) {
                Some(end) => i += end + PASTE_END.len(),
                None => return i,
            }
        } else if PASTE_START.starts_with(rest) || rest == b"\x1bO" {
            // An Esc alone at the end, or the start of a longer key: the
            // rest may follow. (Taken as it is if nothing does.)
            return i;
        } else if rest[0] == 0x1b && rest.len() >= 2 && rest[1] == b'[' {
            match rest[2..].iter().position(|b| (0x40..=0x7e).contains(b)) {
                Some(end) => i += 3 + end,
                None => return i,
            }
        } else if rest[0] == 0x1b && rest.len() >= 2 && (rest[1] == b']' || rest[1] == b'P') {
            let end = rest.iter().position(|&b| b == 0x07).map(|p| p + 1);
            let st = rest.windows(2).position(|w| w == b"\x1b\\").map(|p| p + 2);
            match end.into_iter().chain(st).min() {
                Some(n) => i += n,
                None => return i,
            }
        } else if rest[0] >= 0xc0 {
            let need = if rest[0] >= 0xf0 {
                4
            } else if rest[0] >= 0xe0 {
                3
            } else {
                2
            };
            if rest.len() < need {
                return i;
            }
            i += need;
        } else {
            i += 1;
        }
    }
    i
}

/// Only to the stand-in's log, if it has one: anything on screen would
/// change what the tape shows.
fn log(text: &str) {
    if let Some(path) = std::env::var_os("STANDIN_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{text}");
        }
    }
}

/// A tape that cannot be read is said on screen, where a check will see
/// it, and the player stops.
fn broken(what: &str) -> ! {
    print!("standin: the tape is not readable: {what}\r\n");
    let _ = std::io::stdout().flush();
    log(&format!("TAPE BROKEN {what}"));
    std::process::exit(2);
}

/// Hex that is not two hex digits a byte is a broken tape.
fn unhex_strict(text: &str, line: usize) -> Vec<u8> {
    if text.len() % 2 != 0 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        broken(&format!("line {line} has bytes that are not hex"));
    }
    unhex(text)
}

pub fn play(path: &str) {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| broken(&format!("{path}: {e}")));
    let head: serde_json::Value =
        serde_json::from_str(text.lines().next().unwrap_or("")).unwrap_or_else(|_| broken("no first line"));
    if head["tape"] != 1 {
        broken(&format!("tape format {} (this player knows 1)", head["tape"]));
    }
    let cap = match std::env::var("STANDIN_TAPE_CAP_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
        Some(ms) => ms,
        None if std::env::var_os("STANDIN_TAPE_FAST").is_some() => 50,
        None => u64::MAX,
    };
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    log(&format!("TAPE INPUT ENDED {e}"));
                    break;
                }
            }
        }
    });
    let mut got: Vec<Vec<u8>> = Vec::new();
    // Bytes of a key whose rest has not arrived yet. If nothing more comes
    // in 50 ms they are taken as they are: an Esc pressed alone.
    let mut pending: Vec<u8> = Vec::new();
    let debug = std::env::var_os("STANDIN_TAPE_DEBUG").is_some();
    // False once the input has ended or Ctrl+D was pressed.
    let mut take_input = |got: &mut Vec<Vec<u8>>, wait: Duration| -> bool {
        let wait = if pending.is_empty() { wait } else { wait.min(Duration::from_millis(50)) };
        let complete = match rx.recv_timeout(wait) {
            Ok(bytes) => {
                if debug {
                    log(&format!("TAPE GOT {}", String::from_utf8_lossy(&bytes).escape_debug()));
                }
                pending.extend_from_slice(&bytes);
                whole(&pending)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => pending.len(),
            Err(mpsc::RecvTimeoutError::Disconnected) => return false,
        };
        let ready: Vec<u8> = pending.drain(..complete).collect();
        let new = keys(&ready);
        if new.iter().any(|k| k == b"\x04") {
            return false;
        }
        got.extend(new.into_iter().filter(|k| !is_answer(k)));
        true
    };
    let mut out = std::io::stdout();
    for (n, line) in text.lines().enumerate().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let event: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|_| broken(&format!("line {} is not JSON", n + 1)));
        let ms = event["ms"].as_u64().unwrap_or(0);
        let until = Instant::now() + Duration::from_millis(ms.min(cap));
        while let Some(left) = until.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) {
            if !take_input(&mut got, left) {
                return;
            }
        }
        if let Some(bytes) = event["out"].as_str() {
            let _ = out.write_all(&without_console_requests(&unhex_strict(bytes, n + 1)));
            let _ = out.flush();
        } else if let Some(bytes) = event["key"].as_str() {
            let raw = unhex_strict(bytes, n + 1);
            let want: Vec<Vec<u8>> = keys(&raw).into_iter().filter(|k| !is_answer(k)).collect();
            if want.is_empty() {
                log(&format!(
                    "TAPE SKIPS A KEY IT CANNOT WAIT FOR {}",
                    String::from_utf8_lossy(&raw).escape_debug()
                ));
                continue;
            }
            let paste = want.iter().any(|k| k.starts_with(PASTE_START));
            log(&format!("TAPE WAITS FOR {}", String::from_utf8_lossy(&raw).escape_debug()));
            loop {
                if let Taken::Yes { pasted_typed, only_first } = take(&mut got, &want) {
                    if paste {
                        // Read by the replay check: a paste must not arrive
                        // as typed keys.
                        log(&format!("TAPE PASTE TAKEN typed={pasted_typed} only-first={only_first}"));
                    }
                    break;
                }
                if !take_input(&mut got, Duration::from_millis(200)) {
                    return;
                }
            }
        } else if let Some(name) = event["moment"].as_str() {
            log(&format!("TAPE MOMENT {name}"));
        }
    }
    log("TAPE ENDED");
    // The agent is still there, idle, as it was when the tape stopped.
    while take_input(&mut got, Duration::from_secs(3600)) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_cut_one_each() {
        let k = keys(b"a\x1b[A\x1b\x1b[200~x\ny\r\nz\x1b[201~\r");
        let want: Vec<&[u8]> = vec![b"a", b"\x1b[A", b"\x1b", b"\x1b[200~x\ry\rz\x1b[201~", b"\r"];
        assert_eq!(k, want.into_iter().map(<[u8]>::to_vec).collect::<Vec<_>>());
    }

    #[test]
    fn a_terminal_answer_is_never_a_key() {
        for answer in [
            &b"\x1b]7501;?\x07"[..],
            b"\x1b[?62;22c",
            b"\x1b[12;40R",
            b"\x1b[I",
            b"\x1b[?2004;1$y",
            b"\x1b[8;40;100t",
        ] {
            assert!(keys(answer).iter().all(|k| is_answer(k)), "{answer:?}");
        }
        for key in [&b"\x1b"[..], b"\x1b[A", b"\x1b[24~", b"\r", b"y", b"\x1b[1;5R", b"\x1b[Z"] {
            assert!(!keys(key).iter().any(|k| is_answer(k)), "{key:?}");
        }
    }

    #[test]
    fn a_key_is_found_past_anything_else() {
        let mut got = keys(b"zz\x1b[?1;2c1");
        got.retain(|k| !is_answer(k));
        assert_ne!(take(&mut got, &keys(b"1")), Taken::No);
        assert!(got.is_empty());
        let mut early = keys(b"y");
        assert_eq!(take(&mut early, &keys(b"\r")), Taken::No);
        assert_eq!(early.len(), 1);
    }

    #[test]
    fn how_a_paste_arrived_is_said() {
        let want = keys(b"\x1b[200~ab\rc\x1b[201~");
        let mut pasted = keys(b"\x1b[200~ab\rc\x1b[201~\r");
        assert_eq!(take(&mut pasted, &want), Taken::Yes { pasted_typed: 0, only_first: true });
        assert_eq!(pasted, vec![b"\r".to_vec()]);
        let mut first_typed = keys(b"a\x1b[200~b\rc\x1b[201~");
        assert_eq!(take(&mut first_typed, &want), Taken::Yes { pasted_typed: 1, only_first: true });
        let mut all_typed = keys(b"ab\rc");
        assert_eq!(take(&mut all_typed, &want), Taken::Yes { pasted_typed: 4, only_first: false });
    }

    #[test]
    fn what_is_left_of_a_paste_stays() {
        let mut got = keys(b"\x1b[200~xyz\x1b[201~q");
        assert_ne!(take(&mut got, &keys(b"x")), Taken::No);
        assert_eq!(got, vec![b"y".to_vec(), b"z".to_vec(), b"q".to_vec()]);
    }

    #[test]
    fn an_empty_paste_is_waited_for_by_nothing() {
        let mut got = keys(b"a");
        assert_ne!(take(&mut got, &keys(b"\x1b[200~\x1b[201~")), Taken::No);
    }

    #[test]
    fn a_key_cut_in_two_waits_for_its_rest() {
        assert_eq!(whole(b"ab\x1b[20"), 2);
        assert_eq!(whole(b"ab\x1b"), 2);
        assert_eq!(whole(b"\x1b[200~half"), 0);
        assert_eq!(whole(b"\x1b[13;28;13;1;0;1_"), 17);
        assert_eq!(whole(&[b'a', 0xe2, 0x94]), 1);
        assert_eq!(whole(b"\x1b]7501;?"), 0);
        assert_eq!(whole(b"\x1b]7501;?\x07z"), 10);
    }

    #[test]
    fn a_windows_key_is_its_character() {
        assert_eq!(keys(b"\x1b[13;28;13;1;0;1_\x1b[13;28;13;0;0;1_"), vec![b"\r".to_vec()]);
        assert_eq!(keys(b"\x1b[16;42;0;1;16;1_"), Vec::<Vec<u8>>::new());
        assert_eq!(keys(b"\x1b[27;1;27;1;0;1_"), vec![b"\x1b".to_vec()]);
        assert_eq!(without_console_requests(b"a\x1b[?9001hb\x1b[?1004h"), b"ab\x1b[?1004h".to_vec());
    }

    #[test]
    fn hex_comes_back_the_same() {
        let bytes = b"\x1b]7501;state=idle\x07\xe2\x94\x80";
        assert_eq!(unhex(&hex(bytes)), bytes.to_vec());
    }
}
