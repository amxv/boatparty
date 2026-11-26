//! The runner's stderr frames.
//!
//! The runner ends stderr with one trailer, `\n<nonce> <exit-code> <tree>
//! <patch-bytes>\n` or `\n<nonce> needfull\n`, and may emit event lines
//! mid-stream, `\n<nonce>!<event>\n`. [`TrailerFilter`] passes every other
//! byte through untouched, collects events and captures the trailer, whatever
//! the chunk boundaries.

/// What the trailer reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trailer {
    Done {
        exit_code: i32,
        tree: String,
        patch_bytes: u64,
        cpu_steal_percent: Option<u8>,
    },
    /// The pack assumed objects the sandbox lacks (exit 97).
    NeedFull,
}

/// Output in order: bytes to print, or an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Bytes(Vec<u8>),
    Event(String),
}

#[derive(Debug)]
pub struct TrailerFilter {
    marker: Vec<u8>,
    event_marker: Vec<u8>,
    pending: Vec<u8>,
    trailer: Option<Vec<u8>>,
    events: Vec<String>,
}

impl TrailerFilter {
    pub fn new(nonce: &str) -> Self {
        Self {
            marker: format!("\n{nonce} ").into_bytes(),
            event_marker: format!("\n{nonce}!").into_bytes(),
            pending: Vec::new(),
            trailer: None,
            events: Vec::new(),
        }
    }

    /// Feed a chunk; returns the bytes that are safe to print now. Events
    /// are collected for [`TrailerFilter::take_events`].
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.step(chunk, false)
    }

    /// Feed a chunk; returns what is safe to emit now, with each event in
    /// its place among the bytes.
    pub fn push_pieces(&mut self, chunk: &[u8]) -> Vec<Piece> {
        let mut pieces = Vec::new();
        let mut input = chunk;
        loop {
            let before = self.events.len();
            let bytes = self.step(input, true);
            if !bytes.is_empty() {
                pieces.push(Piece::Bytes(bytes));
            }
            if self.events.len() == before {
                return pieces;
            }
            if let Some(event) = self.events.pop() {
                pieces.push(Piece::Event(event));
            }
            input = &[];
        }
    }

    fn step(&mut self, chunk: &[u8], stop_at_event: bool) -> Vec<u8> {
        if let Some(trailer) = &mut self.trailer {
            trailer.extend_from_slice(chunk);
            return Vec::new();
        }
        let mut combined = std::mem::take(&mut self.pending);
        combined.extend_from_slice(chunk);
        let mut printable = Vec::new();
        loop {
            let trailer_at = find(&combined, &self.marker);
            let event_at = find(&combined, &self.event_marker);
            match (event_at, trailer_at) {
                (Some(event), trailer) if trailer.is_none_or(|t| event < t) => {
                    let body = event + self.event_marker.len();
                    let Some(end) = combined[body..].iter().position(|&b| b == b'\n') else {
                        // Hold an incomplete event line until its newline arrives.
                        printable.extend_from_slice(&combined[..event]);
                        self.pending = combined[event..].to_vec();
                        return printable;
                    };
                    printable.extend_from_slice(&combined[..event]);
                    let name = String::from_utf8_lossy(&combined[body..body + end]);
                    self.events.push(name.trim().to_string());
                    combined.drain(..body + end + 1);
                    if stop_at_event {
                        self.pending = combined;
                        return printable;
                    }
                }
                (_, Some(trailer)) => {
                    printable.extend_from_slice(&combined[..trailer]);
                    self.trailer = Some(combined[trailer + self.marker.len()..].to_vec());
                    return printable;
                }
                _ => break,
            }
        }
        // Hold back a tail that could be the start of either marker.
        let keep = partial_suffix(&combined, &self.marker)
            .max(partial_suffix(&combined, &self.event_marker));
        let split = combined.len() - keep;
        printable.extend_from_slice(&combined[..split]);
        self.pending = combined[split..].to_vec();
        printable
    }

    /// Flush held-back bytes at the end of the stream.
    pub fn end(&mut self) -> Vec<u8> {
        if self.trailer.is_some() {
            self.pending.clear();
            return Vec::new();
        }
        std::mem::take(&mut self.pending)
    }

    /// Events seen since the last call.
    pub fn take_events(&mut self) -> Vec<String> {
        std::mem::take(&mut self.events)
    }

    /// Whether the whole trailer line has arrived.
    pub fn complete(&self) -> bool {
        self.trailer
            .as_ref()
            .is_some_and(|trailer| trailer.contains(&b'\n'))
    }

    /// The parsed trailer, once complete.
    pub fn result(&self) -> Option<Trailer> {
        let text = String::from_utf8_lossy(self.trailer.as_ref()?);
        let fields: Vec<&str> = text.split_whitespace().collect();
        if fields.first() == Some(&"needfull") {
            return Some(Trailer::NeedFull);
        }
        let (code, tree) = (fields.first()?, fields.get(1)?);
        if !code.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(Trailer::Done {
            exit_code: code.parse().ok()?,
            tree: (*tree).to_string(),
            cpu_steal_percent: fields
                .get(3)
                .and_then(|n| n.parse::<u8>().ok())
                .filter(|n| *n <= 100),
            patch_bytes: fields.get(2).and_then(|n| n.parse().ok()).unwrap_or(0),
        })
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Length of the longest suffix of `text` that is a proper prefix of `marker`.
fn partial_suffix(text: &[u8], marker: &[u8]) -> usize {
    (1..marker.len().min(text.len() + 1))
        .rev()
        .find(|&length| marker.starts_with(&text[text.len() - length..]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_run(nonce: &str, stream: &str, cut: usize) -> (String, TrailerFilter) {
        let mut filter = TrailerFilter::new(nonce);
        let mut printed = filter.push(&stream.as_bytes()[..cut]);
        printed.extend(filter.push(&stream.as_bytes()[cut..]));
        printed.extend(filter.end());
        (String::from_utf8(printed).unwrap(), filter)
    }

    #[test]
    fn trailer_is_split_from_stderr_across_arbitrary_chunk_boundaries() {
        let stream = "warning: x\nmore output\nabc123 3 deadbeef 42\n";
        for cut in 0..=stream.len() {
            let (printed, filter) = split_run("abc123", stream, cut);
            assert_eq!(printed, "warning: x\nmore output", "cut {cut}");
            assert_eq!(
                filter.result(),
                Some(Trailer::Done {
                    cpu_steal_percent: None,
                    exit_code: 3,
                    tree: "deadbeef".into(),
                    patch_bytes: 42
                })
            );
        }
    }

    #[test]
    fn events_are_reported_and_removed_across_arbitrary_chunk_boundaries() {
        let stream = "building\n\nabc123!ready\napp log\nabc123 0 deadbeef 0\n";
        for cut in 0..=stream.len() {
            let (printed, mut filter) = split_run("abc123", stream, cut);
            assert_eq!(printed, "building\napp log", "cut {cut}");
            assert_eq!(filter.take_events(), vec!["ready".to_string()]);
            assert_eq!(
                filter.result(),
                Some(Trailer::Done {
                    cpu_steal_percent: None,
                    exit_code: 0,
                    tree: "deadbeef".into(),
                    patch_bytes: 0
                })
            );
        }
    }

    #[test]
    fn pieces_keep_events_in_their_place_among_the_bytes() {
        let stream = "before\n\nabc!started\nafter\n\nabc 0 t 0\n";
        for cut in 0..=stream.len() {
            let mut filter = TrailerFilter::new("abc");
            let mut pieces = filter.push_pieces(&stream.as_bytes()[..cut]);
            pieces.extend(filter.push_pieces(&stream.as_bytes()[cut..]));
            let mut flat = String::new();
            for piece in pieces {
                match piece {
                    Piece::Bytes(bytes) => flat.push_str(&String::from_utf8(bytes).unwrap()),
                    Piece::Event(name) => flat.push_str(&format!("<{name}>")),
                }
            }
            flat.push_str(&String::from_utf8(filter.end()).unwrap());
            assert_eq!(flat, "before\n<started>after\n", "cut {cut}");
            assert!(filter.take_events().is_empty());
            assert!(filter.complete());
        }
    }

    #[test]
    fn every_byte_split_of_a_needfull_trailer_is_recognized() {
        let stream = "index-pack noise\n\nn1 needfull\n";
        for cut in 0..=stream.len() {
            let (printed, filter) = split_run("n1", stream, cut);
            assert_eq!(printed, "index-pack noise\n", "cut {cut}");
            assert_eq!(filter.result(), Some(Trailer::NeedFull));
        }
    }

    #[test]
    fn needfull_an_empty_prefix_and_a_missing_trailer() {
        let mut needfull = TrailerFilter::new("n1");
        let mut printed = needfull.push(b"\nn1 needfull\n");
        printed.extend(needfull.end());
        assert!(printed.is_empty());
        assert_eq!(needfull.result(), Some(Trailer::NeedFull));

        let mut none = TrailerFilter::new("n2");
        let mut printed = none.push(b"output without newline\nn");
        printed.extend(none.end());
        assert_eq!(printed, b"output without newline\nn");
        assert_eq!(none.result(), None);
    }

    #[test]
    fn binary_output_passes_through_byte_for_byte() {
        let mut stream = vec![0u8, 0xff, b'\n', 0xfe, b'\n'];
        stream.extend_from_slice(b"\nzz 0 t 0\n");
        for cut in 0..=stream.len() {
            let mut filter = TrailerFilter::new("zz");
            let mut printed = filter.push(&stream[..cut]);
            printed.extend(filter.push(&stream[cut..]));
            printed.extend(filter.end());
            assert_eq!(printed, vec![0u8, 0xff, b'\n', 0xfe, b'\n']);
        }
    }

    #[test]
    fn a_malformed_trailer_is_not_a_result() {
        let mut filter = TrailerFilter::new("n");
        filter.push(b"\nn x tree 0\n");
        assert_eq!(filter.result(), None);
    }
}
