use serde_json::Value;

const ESC: u8 = 0x1b;
const TERMINATOR: &[u8] = b"\x1b\\";
const MAX_HELD_BYTES: usize = 1 << 20;

#[derive(Debug, PartialEq)]
pub enum Piece {
    Output(Vec<u8>),
    Control(Value),
}

pub struct StreamParser {
    prefix: Vec<u8>,
    synced: bool,
    held: Vec<u8>,
}

impl StreamParser {
    pub fn new(nonce: u64) -> Self {
        Self {
            prefix: format!("\x1b_rex1;{nonce};").into_bytes(),
            synced: false,
            held: Vec::new(),
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Piece> {
        let mut buffer = std::mem::take(&mut self.held);
        buffer.extend_from_slice(chunk);
        let mut pieces = Vec::new();
        let mut output = Vec::new();
        let mut at = 0;
        while at < buffer.len() {
            let Some(esc) = find(&buffer[at..], &[ESC]).map(|i| at + i) else {
                output.extend_from_slice(&buffer[at..]);
                break;
            };
            output.extend_from_slice(&buffer[at..esc]);
            let rest = &buffer[esc..];
            match self.match_control(rest) {
                Match::Partial => {
                    self.held = rest.to_vec();
                    break;
                }
                Match::Control(body, length) => {
                    self.flush(&mut output, &mut pieces);
                    if let Ok(control) = serde_json::from_slice::<Value>(body) {
                        self.synced |= control["type"] == "resize";
                        pieces.push(Piece::Control(control));
                    }
                    at = esc + length;
                }
                Match::None => {
                    output.push(ESC);
                    at = esc + 1;
                }
            }
        }
        self.flush(&mut output, &mut pieces);
        pieces
    }

    fn match_control<'a>(&self, rest: &'a [u8]) -> Match<'a> {
        if rest.len() < self.prefix.len() {
            return match self.prefix.starts_with(rest) {
                true => Match::Partial,
                false => Match::None,
            };
        }
        if !rest.starts_with(&self.prefix) {
            return Match::None;
        }
        let body = &rest[self.prefix.len()..];
        match find(body, TERMINATOR) {
            Some(end) => Match::Control(&body[..end], self.prefix.len() + end + TERMINATOR.len()),
            None if rest.len() > MAX_HELD_BYTES => Match::None,
            None => Match::Partial,
        }
    }

    fn flush(&self, output: &mut Vec<u8>, pieces: &mut Vec<Piece>) {
        let bytes = std::mem::take(output);
        if self.synced && !bytes.is_empty() {
            pieces.push(Piece::Output(bytes));
        }
    }
}

enum Match<'a> {
    Partial,
    Control(&'a [u8], usize),
    None,
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NONCE: u64 = 9489415115013900158;

    fn apc(nonce: u64, body: &str) -> Vec<u8> {
        format!("\x1b_rex1;{nonce};{body}\x1b\\").into_bytes()
    }

    fn resize() -> Vec<u8> {
        apc(NONCE, r#"{"type":"resize","cols":80,"rows":24}"#)
    }

    fn synced() -> StreamParser {
        let mut parser = StreamParser::new(NONCE);
        parser.feed(&resize());
        parser
    }

    fn output(pieces: &[Piece]) -> Vec<u8> {
        pieces
            .iter()
            .filter_map(|piece| match piece {
                Piece::Output(bytes) => Some(bytes.clone()),
                Piece::Control(_) => None,
            })
            .flatten()
            .collect()
    }

    #[test]
    fn drops_snapshot_before_first_resize() {
        let mut parser = StreamParser::new(NONCE);
        let mut chunk = b"GHOSTSNP\x01\x00\x1b[0m junk".to_vec();
        chunk.extend(resize());
        chunk.extend(b"hello");
        assert_eq!(
            parser.feed(&chunk),
            vec![
                Piece::Control(json!({"type":"resize","cols":80,"rows":24})),
                Piece::Output(b"hello".to_vec()),
            ]
        );
    }

    #[test]
    fn drops_output_until_resize_even_across_chunks() {
        let mut parser = StreamParser::new(NONCE);
        assert!(parser.feed(b"GHOSTSNP part one").is_empty());
        let mut chunk = b"part two".to_vec();
        chunk.extend(resize());
        let pieces = parser.feed(&chunk);
        assert!(output(&pieces).is_empty());
        assert_eq!(output(&parser.feed(b"$ ")), b"$ ");
    }

    #[test]
    fn joins_control_split_across_chunks() {
        let mut parser = synced();
        let mut stream = b"before".to_vec();
        stream.extend(apc(NONCE, r#"{"type":"resize","cols":100,"rows":30}"#));
        stream.extend(b"after");
        for split in 1..stream.len() {
            let mut parser_copy = StreamParser::new(NONCE);
            parser_copy.feed(&resize());
            let mut pieces = parser_copy.feed(&stream[..split]);
            pieces.extend(parser_copy.feed(&stream[split..]));
            assert_eq!(output(&pieces), b"beforeafter", "split at {split}");
            assert!(pieces.contains(&Piece::Control(
                json!({"type":"resize","cols":100,"rows":30})
            )));
        }
        assert_eq!(output(&parser.feed(&stream)), b"beforeafter");
    }

    #[test]
    fn splits_byte_by_byte() {
        let mut parser = synced();
        let mut stream = apc(NONCE, r#"{"type":"child_exited"}"#);
        stream.extend(b"x");
        let pieces: Vec<Piece> = stream.iter().flat_map(|b| parser.feed(&[*b])).collect();
        assert_eq!(
            pieces,
            vec![
                Piece::Control(json!({"type":"child_exited"})),
                Piece::Output(b"x".to_vec()),
            ]
        );
    }

    #[test]
    fn forwards_control_with_wrong_nonce() {
        let mut parser = synced();
        let foreign = apc(42, r#"{"type":"resize","cols":1,"rows":1}"#);
        assert_eq!(parser.feed(&foreign), vec![Piece::Output(foreign.clone())]);
    }

    #[test]
    fn forwards_other_escapes() {
        let mut parser = synced();
        let bytes = b"\x1b[31mred\x1b]0;title\x07\x1b_other\x1b\\".to_vec();
        assert_eq!(output(&parser.feed(&bytes)), bytes);
    }

    #[test]
    fn holds_trailing_escape_until_next_chunk() {
        let mut parser = synced();
        assert_eq!(output(&parser.feed(b"ab\x1b")), b"ab");
        assert_eq!(output(&parser.feed(b"[0m")), b"\x1b[0m");
    }

    #[test]
    fn keeps_order_of_output_and_controls() {
        let mut parser = synced();
        let mut stream = b"one".to_vec();
        stream.extend(apc(NONCE, r#"{"type":"theme"}"#));
        stream.extend(b"two");
        assert_eq!(
            parser.feed(&stream),
            vec![
                Piece::Output(b"one".to_vec()),
                Piece::Control(json!({"type":"theme"})),
                Piece::Output(b"two".to_vec()),
            ]
        );
    }
}
