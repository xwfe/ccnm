use super::*;

const T: Timing = Timing {
    ping_after: Duration::from_secs(30),
    give_up_after: Duration::from_secs(600),
    tick: Duration::from_secs(1),
};

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[test]
fn a_client_that_talks_is_never_asked() {
    let t0 = Instant::now();
    assert_eq!(step(&T, t0 + secs(29), t0, None), Step::Wait);
}

#[test]
fn silence_asks_then_asks_again_every_interval_until_giving_up() {
    let t0 = Instant::now();
    assert_eq!(step(&T, t0 + secs(30), t0, None), Step::Ping);
    let first = t0 + secs(30);
    assert_eq!(step(&T, t0 + secs(45), t0, Some(first)), Step::Wait);
    assert_eq!(step(&T, t0 + secs(60), t0, Some(first)), Step::Ping);
    assert_eq!(
        step(&T, t0 + secs(599), t0, Some(t0 + secs(590))),
        Step::Wait
    );
    assert_eq!(
        step(&T, t0 + secs(600), t0, Some(t0 + secs(590))),
        Step::GiveUp
    );
}

/// A client that answers is heard from, and hearing from it starts the
/// count again: it is asked every half minute and never given up on.
#[test]
fn a_client_that_answers_every_ask_is_kept_for_ever() {
    let t0 = Instant::now();
    let mut heard = t0;
    let mut last_ping = None;
    let mut asked = 0;
    for s in 0..3 * 600 {
        let now = t0 + secs(s);
        match step(&T, now, heard, last_ping) {
            Step::Wait => {}
            Step::Ping => {
                asked += 1;
                last_ping = Some(now);
                // The answer arrives within the same second.
                heard = now;
            }
            Step::GiveUp => panic!("gave up on a client that answered, at {s} s"),
        }
    }
    assert_eq!(asked, 59);
}

#[test]
fn any_byte_starts_the_count_again_even_after_asking() {
    let t0 = Instant::now();
    let ping = t0 + secs(30);
    let byte = t0 + secs(500);
    assert_eq!(step(&T, byte + secs(10), byte, Some(ping)), Step::Wait);
    // Six hundred seconds after the start, but only a hundred after the
    // byte. The ping before the byte is not a pending ask, so it asks.
    assert_eq!(step(&T, t0 + secs(600), byte, Some(ping)), Step::Ping);
    assert_eq!(
        step(&T, byte + secs(599), byte, Some(byte + secs(590))),
        Step::Wait
    );
    assert_eq!(
        step(&T, byte + secs(600), byte, Some(byte + secs(590))),
        Step::GiveUp
    );
}

#[test]
fn heard_only_moves_forward_and_counts_bytes_not_messages() {
    let heard = Heard::new();
    let start = heard.at();
    let mut reader = Touching::new(&b"{\"id\":1,"[..], Arc::clone(&heard));
    std::thread::sleep(Duration::from_millis(20));
    let mut buf = [0u8; 4];
    assert_eq!(reader.read(&mut buf).unwrap(), 4);
    let touched = heard.at();
    assert!(touched > start, "a partial message is still a byte heard");
    // End of input is not hearing from anyone.
    let mut empty = Touching::new(&b""[..], Arc::clone(&heard));
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(empty.read(&mut buf).unwrap(), 0);
    assert_eq!(heard.at(), touched);
}

#[test]
fn only_answers_to_liveness_ids_are_taken_out_of_the_stream() {
    let line = ping(7);
    assert_eq!(line.last(), Some(&b'\n'));
    let request: Value = serde_json::from_slice(&line).unwrap();
    assert_eq!(request["method"], "ccnm/liveness");
    assert!(!is_answer(&request), "the request itself is not an answer");

    // What Codex 0.154.0 sent back, measured in P26.1.
    let answer = serde_json::json!({"id": request["id"], "error": {"code": -32601, "message": "exec-server client does not implement `ccnm/liveness` yet"}});
    assert!(is_answer(&answer));
    for other in [
        serde_json::json!({"id": 7, "result": {}}),
        serde_json::json!({"id": "7", "error": {}}),
        serde_json::json!({"id": "ccnm-liveness-1", "method": "fs/readFile", "params": {}}),
    ] {
        assert!(!is_answer(&other), "{other}");
    }
}
