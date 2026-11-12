//! Randomised robustness tests for the parsers: whatever the input, they must
//! return a verdict without panicking, and agree with themselves however the
//! input is split.

use crate::chunked::{Decoder, Progress};
use crate::parse::{find_head_end, header_lines, parse_head};

/// xorshift64*: deterministic, so a failure can be reproduced from its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const SEEDS: &[&[u8]] = &[
    b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n",
    b"POST /submit?a=1 HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nConnection: close\r\n\r\n",
    b"PUT /u HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nExpect: 100-continue\r\n\r\n",
    b"GET http://h/p HTTP/1.0\r\nConnection: keep-alive\r\nAccept: */*\r\n\r\n",
];

const INTERESTING: &[u8] = b"\r\n\r\n: ,;\t\0\x7f\xff0123456789abcdefHTTP/1.1";

fn mutate(rng: &mut Rng, input: &mut Vec<u8>) {
    for _ in 0..1 + rng.below(4) {
        if input.is_empty() {
            input.push(rng.next() as u8);
            continue;
        }
        let at = rng.below(input.len());
        match rng.below(6) {
            0 => input[at] = rng.next() as u8,
            1 => input[at] = INTERESTING[rng.below(INTERESTING.len())],
            2 => {
                input.remove(at);
            }
            3 => input.insert(at, INTERESTING[rng.below(INTERESTING.len())]),
            4 => {
                let end = (at + 1 + rng.below(8)).min(input.len());
                let piece = input[at..end].to_vec();
                let to = rng.below(input.len());
                input.splice(to..to, piece);
            }
            _ => input.truncate(at),
        }
    }
}

#[test]
fn the_head_parser_never_panics_and_accepts_only_what_it_can_serve() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut accepted = 0;
    for round in 0..300_000 {
        let mut input = SEEDS[round % SEEDS.len()].to_vec();
        mutate(&mut rng, &mut input);

        let Some(end) = find_head_end(&input, 0) else {
            continue;
        };
        // Searching from any earlier offset finds the same terminator.
        let from = rng.below(end.saturating_sub(3) + 1);
        assert_eq!(find_head_end(&input, from), Some(end));

        let head = &input[..end];
        match parse_head(head) {
            Ok(parsed) => {
                accepted += 1;
                assert_eq!(parsed.len, end);
                // Everything a `Request` later slices out must be in bounds
                // and valid UTF-8 where it is exposed as `&str`.
                assert!(std::str::from_utf8(&head[parsed.method.clone()]).is_ok());
                assert!(std::str::from_utf8(&head[parsed.target.clone()]).is_ok());
                for (name, value) in header_lines(&head[parsed.headers.clone()]) {
                    assert!(std::str::from_utf8(name).is_ok());
                    assert!(!value.contains(&b'\r') && !value.contains(&b'\n'));
                }
                // An accepted head never has a stray line break or NUL.
                let lines = &head[..end - 4];
                assert!(!lines.contains(&0));
                assert_eq!(
                    lines.iter().filter(|&&b| b == b'\r').count(),
                    lines.iter().filter(|&&b| b == b'\n').count()
                );
                assert!(!(parsed.chunked && parsed.content_length.is_some()));
            }
            Err(status) => assert!(matches!(status, 400 | 417 | 501 | 505), "{status}"),
        }
    }
    // The mutations must not be so destructive that nothing gets through.
    assert!(accepted > 1_000, "only {accepted} inputs were accepted");
}

#[test]
fn the_chunked_decoder_never_panics_and_ignores_how_input_is_split() {
    let seeds: &[&[u8]] = &[
        b"5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\n\r\n",
        b"1\r\na\r\n1\r\nb\r\nA\r\n0123456789\r\n0\r\nTrailer: x\r\n\r\n",
        b"0\r\n\r\n",
    ];
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut completed = 0;
    for round in 0..200_000 {
        let mut input = seeds[round % seeds.len()].to_vec();
        if round % 4 != 0 {
            mutate(&mut rng, &mut input);
        }
        let max = [0, 5, 11, 64, 1 << 20][rng.below(5)];

        let mut whole = Decoder::new(max);
        let expected = whole.feed(&input);

        // The same bytes, delivered in random pieces.
        let mut pieces = Decoder::new(max);
        let mut end = 0;
        let got = loop {
            end = (end + 1 + rng.below(6)).min(input.len());
            match pieces.feed(&input[..end]) {
                Ok(Progress::NeedMore) if end < input.len() => continue,
                other => break other,
            }
        };
        assert_eq!(got, expected, "input {:?}", String::from_utf8_lossy(&input));
        if let Ok(Progress::Done(used)) = expected {
            completed += 1;
            assert!(used <= input.len());
            assert!(whole.body.len() <= max);
            assert_eq!(whole.body, pieces.body);
        }
    }
    assert!(completed > 1_000, "only {completed} inputs decoded");
}
