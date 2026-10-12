//! Incremental parser for `text/event-stream` bodies that pass **through** the proxy (roadmap §7.2).
//!
//! The engine already knew how to *stream* SSE: `response_must_stream` prevents a `text/event-stream`
//! response from being buffered before its headers go out. What it could not do was *read* one. An
//! SSE response is a long-lived, unbounded body — the shape a buffer-first proxy handles worst — so
//! the only workable model is to parse it as it goes by, without holding it.
//!
//! This is deliberately a separate module from [`crate::sse`], which describes the *engine's own*
//! `/api/v1/events` contract. That module is this proxy talking to its clients; this one is somebody
//! else's event stream passing through. The field names coincide because both follow the SSE spec,
//! and conflating them would tie the engine's own wire contract to whatever a user's app happens to
//! send.
//!
//! Behavior follows the WHATWG event-stream rules, because that is what a browser client sees and
//! therefore what has to be reported faithfully:
//!
//! * lines end with CRLF, LF or CR;
//! * a line starting with `:` is a comment and is ignored;
//! * `field: value` — one optional leading space is stripped from the value;
//! * `data` lines accumulate, joined with `\n`, and the trailing `\n` is removed when dispatched;
//! * a blank line dispatches the event, unless no `data` was collected;
//! * `retry` must be all digits to count, and is stream state rather than per-event;
//! * an event that never gets its blank line is **not** dispatched at end of stream.
//!
//! Everything here works on the raw bytes that actually crossed the wire: lines are split on byte
//! boundaries and only decoded once complete, so a multi-byte character split across two chunks
//! cannot be mangled.

/// One parsed event.
///
/// Serialisable because it travels: the event is published as an incremental `FlowUpdate`, so the
/// CLI, the HTTP adapter and any MCP consumer read the same parsed shape rather than each re-parsing
/// the raw bytes.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SseEvent {
    /// The `event:` field, when the stream set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// The collected `data:` payload, with the final newline removed.
    pub data: String,
    /// The last event id in effect when this event was dispatched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The reconnection time in effect, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<u64>,
}

impl SseEvent {
    /// What a list of events should call this one: the `event:` field, else `message` as the spec
    /// defines for an unnamed event.
    pub fn kind(&self) -> &str {
        self.event.as_deref().unwrap_or("message")
    }
}

/// Why events were lost, so "the capture is shorter than the stream" is answerable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SseLosses {
    /// Events dropped because their payload exceeded the parser's budget.
    pub oversized: u64,
    /// A line that never ended before the budget was reached, so the parse could not continue.
    pub unterminated_line: u64,
    /// A trailing event that had no closing blank line when the stream ended. The spec discards it
    /// and so do we — reporting it as an event would invent something the client never received.
    pub truncated_at_end: bool,
}

/// Streaming `text/event-stream` parser.
///
/// Feed it the bytes as they arrive; it returns whatever events became complete. It never holds more
/// than one event's worth of data plus one partial line, which is the property that makes it usable
/// on a stream that might run for hours.
#[derive(Debug)]
pub struct SseParser {
    /// Bytes of a line that has not ended yet.
    tail: Vec<u8>,
    event_type: Option<String>,
    data: String,
    /// Stream state: the last id seen, carried onto every dispatched event until replaced.
    last_id: Option<String>,
    /// Stream state: the reconnection time, likewise.
    retry: Option<u64>,
    /// A single event's payload is capped so a stream that never dispatches cannot grow without
    /// bound; the cap is per event, not per stream.
    max_event_bytes: usize,
    /// Once an event is over budget its remaining bytes are discarded rather than accumulated, and
    /// the event is not dispatched — a truncated event reported as a whole one would be a lie.
    overflowing: bool,
    events: u64,
    losses: SseLosses,
}

/// Default cap for one event's payload: 1 MiB.
///
/// SSE payloads are messages, not files — a stream carrying an event larger than this is either
/// mislabelled (a download served as `text/event-stream`) or hostile, and either way holding it
/// costs memory the engine would rather spend elsewhere. The cap is per event so a long stream with
/// many small events is unaffected.
pub const DEFAULT_MAX_EVENT_BYTES: usize = 1024 * 1024;

/// Longest a single line may be before the parser gives up on it: the same order as the event cap,
/// because a line that never ends is an event that never ends.
const MAX_LINE_BYTES: usize = 1024 * 1024;

impl Default for SseParser {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_EVENT_BYTES)
    }
}

impl SseParser {
    pub fn new(max_event_bytes: usize) -> Self {
        Self {
            tail: Vec::new(),
            event_type: None,
            data: String::new(),
            last_id: None,
            retry: None,
            max_event_bytes,
            overflowing: false,
            events: 0,
            losses: SseLosses::default(),
        }
    }

    /// Events dispatched so far.
    pub fn events(&self) -> u64 {
        self.events
    }

    /// What was lost, and why.
    pub fn losses(&self) -> SseLosses {
        self.losses
    }

    /// The reconnection time the stream has announced, if any.
    pub fn retry(&self) -> Option<u64> {
        self.retry
    }

    /// Feed a chunk of the body and collect every event that became complete.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.tail.extend_from_slice(chunk);
        self.drain_lines(false)
    }

    /// Split out every complete line currently buffered.
    ///
    /// `at_eof` matters for one byte: a trailing CR might be a lone terminator or the first half of a
    /// CRLF, and only the end of the stream settles it. Deciding early would either invent a line
    /// break or swallow one.
    fn drain_lines(&mut self, at_eof: bool) -> Vec<SseEvent> {
        let mut dispatched = Vec::new();

        loop {
            let Some((line_end, next)) = find_line_end(&self.tail, at_eof) else {
                // No complete line. A line longer than the cap is not a line; stop accumulating it.
                if self.tail.len() > MAX_LINE_BYTES {
                    self.losses.unterminated_line += 1;
                    self.tail.clear();
                    self.event_type = None;
                    self.data.clear();
                    self.overflowing = false;
                }
                break;
            };

            let line: Vec<u8> = self.tail.drain(..line_end).collect();
            self.tail.drain(..next - line_end);
            if let Some(event) = self.handle_line(&line) {
                dispatched.push(event);
            }
        }

        dispatched
    }

    /// The stream ended.
    ///
    /// The spec discards an event that never got its closing blank line, and this parser discards it
    /// too — but it says so, because "the capture stops mid-event" is exactly the detail a person
    /// debugging a dropped connection needs, and silence would look like a stream that simply ended.
    pub fn finish(&mut self) -> SseLosses {
        // The end of the stream terminates the last line, so a stream ending in `data: x\r` does
        // dispatch: that CR was a line break, and the blank line before it closed the event.
        let _ = self.drain_lines(true);
        if !self.tail.is_empty() || !self.data.is_empty() || self.event_type.is_some() {
            self.losses.truncated_at_end = true;
        }
        self.tail.clear();
        self.data.clear();
        self.event_type = None;
        self.overflowing = false;
        self.losses
    }

    /// Apply one line, returning an event when the line completed one.
    fn handle_line(&mut self, line: &[u8]) -> Option<SseEvent> {
        // Lossy is what the spec prescribes for a stream that is not valid UTF-8: decode with
        // replacement rather than refusing the whole stream.
        let line = String::from_utf8_lossy(line);
        let line = line.strip_suffix('\r').unwrap_or(&line);

        if line.is_empty() {
            return self.dispatch();
        }

        // A comment keeps the connection alive and carries nothing. `retry`-only blocks are comments
        // in spirit but are real fields, so they are handled below rather than filtered here.
        if line.starts_with(':') {
            return None;
        }

        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            // A line with no colon is a field name with an empty value, per the spec.
            None => (line, ""),
        };

        match field {
            "event" => self.event_type = Some(value.to_string()),
            "data" => {
                if self.overflowing {
                    return None;
                }
                self.data.push_str(value);
                self.data.push('\n');
                if self.data.len() > self.max_event_bytes {
                    // Over budget: keep nothing of it. A prefix would be dispatched as if it were the
                    // whole event, and a consumer cannot tell a truncated payload from a short one.
                    self.overflowing = true;
                    self.data.clear();
                }
            }
            "id" => {
                // The spec ignores an id containing NUL rather than truncating it.
                if !value.contains('\0') {
                    self.last_id = Some(value.to_string());
                }
            }
            // A guard rather than a nested `if`: a value that is not a plain integer falls through
            // to the ignored-fields arm below, which is exactly what the spec asks for.
            "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                self.retry = value.parse::<u64>().ok();
            }
            // Unknown fields are ignored, as the spec requires: that is what lets the format grow.
            _ => {}
        }
        None
    }

    /// A blank line: dispatch what has been collected, if anything.
    fn dispatch(&mut self) -> Option<SseEvent> {
        let had_data = !self.data.is_empty() || self.overflowing;
        let event_type = self.event_type.take();
        let overflowing = std::mem::replace(&mut self.overflowing, false);
        let mut data = std::mem::take(&mut self.data);

        if overflowing {
            self.losses.oversized += 1;
            return None;
        }
        if !had_data {
            // No data: the spec resets the event type and dispatches nothing. A bare `event: foo`
            // followed by a blank line is not an event.
            return None;
        }
        // The spec strips exactly one trailing newline: the one this parser added after the last
        // `data` line.
        if data.ends_with('\n') {
            data.pop();
        }

        self.events += 1;
        Some(SseEvent {
            event: event_type,
            data,
            id: self.last_id.clone(),
            retry: self.retry,
        })
    }
}

/// Find the first line terminator, returning `(bytes before it, total bytes it occupies)`.
///
/// CRLF counts as one terminator; a lone CR or LF each count, because a stream that mixes them is
/// accepted and that is what the spec says a client does.
///
/// The one thing this cannot decide is a CR at the very end of what it has: `\r` may be a lone
/// terminator or the first half of `\r\n`. It waits for the next byte — which a later chunk or
/// `at_eof` supplies — rather than guessing, because guessing wrong dispatches a spurious empty
/// event or swallows the blank line that was supposed to dispatch a real one.
fn find_line_end(buf: &[u8], at_eof: bool) -> Option<(usize, usize)> {
    for (i, byte) in buf.iter().enumerate() {
        match byte {
            b'\n' => return Some((i, i + 1)),
            b'\r' => {
                return match buf.get(i + 1) {
                    Some(b'\n') => Some((i, i + 2)),
                    // Not followed by LF: a lone CR is its own terminator.
                    Some(_) => Some((i, i + 1)),
                    None if at_eof => Some((i, i + 1)),
                    None => None,
                };
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(parser: &mut SseParser, chunk: &str) -> Vec<SseEvent> {
        parser.push(chunk.as_bytes())
    }

    #[test]
    fn a_named_event_carries_its_type_and_data() {
        let mut parser = SseParser::default();
        let dispatched = events(&mut parser, "event: tick\ndata: 42\n\n");

        assert_eq!(
            dispatched,
            vec![SseEvent {
                event: Some("tick".to_string()),
                data: "42".to_string(),
                id: None,
                retry: None,
            }]
        );
        assert_eq!(parser.events(), 1);
        assert_eq!(parser.losses(), SseStats::default().losses_marker());
    }

    #[test]
    fn an_event_without_a_type_is_a_message() {
        let mut parser = SseParser::default();
        let dispatched = events(&mut parser, "data: hello\n\n");

        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0].kind(), "message");
        assert_eq!(dispatched[0].data, "hello");
    }

    #[test]
    fn several_data_lines_are_joined_with_newlines() {
        let mut parser = SseParser::default();
        let dispatched = events(&mut parser, "data: one\ndata: two\ndata: three\n\n");

        assert_eq!(dispatched[0].data, "one\ntwo\nthree");
    }

    #[test]
    fn only_one_leading_space_is_stripped_from_a_value() {
        let mut parser = SseParser::default();
        let dispatched = events(&mut parser, "data:  two spaces\n\n");

        assert_eq!(dispatched[0].data, " two spaces");
    }

    #[test]
    fn it_survives_a_chunk_boundary_in_the_middle_of_a_line() {
        let mut parser = SseParser::default();
        assert!(events(&mut parser, "event: ti").is_empty());
        assert!(events(&mut parser, "ck\ndata: 4").is_empty());
        let dispatched = events(&mut parser, "2\n\n");

        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0].event.as_deref(), Some("tick"));
        assert_eq!(dispatched[0].data, "42");
    }

    #[test]
    fn a_crlf_split_across_chunks_is_one_terminator() {
        let mut parser = SseParser::default();
        // The CR arrives at the end of one chunk and its LF at the start of the next: treating them
        // as two terminators would dispatch an empty event in between.
        assert!(events(&mut parser, "data: x\r").is_empty());
        let dispatched = events(&mut parser, "\n\r\n");

        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0].data, "x");
    }

    #[test]
    fn a_lone_cr_terminates_a_line() {
        let mut parser = SseParser::default();
        // Two events in one chunk, ending with an unambiguous terminator: each CR is resolved by the
        // byte that follows it, so neither has to wait for the end of the stream.
        let dispatched = events(&mut parser, "data: x\r\rdata: y\r\r\n");

        assert_eq!(dispatched.len(), 2);
        assert_eq!(dispatched[0].data, "x");
        assert_eq!(dispatched[1].data, "y");
    }

    /// A CR at the very end of what has arrived could still turn out to be the first half of a CRLF,
    /// so the parser waits — and the end of the stream is what settles it.
    #[test]
    fn a_trailing_cr_is_resolved_by_the_end_of_the_stream() {
        let mut parser = SseParser::default();
        // `data: x`, then a blank line whose only byte is a CR whose meaning is still open.
        assert!(events(&mut parser, "data: x\r\r").is_empty());

        let losses = parser.finish();
        assert_eq!(
            parser.events(),
            1,
            "the stream ended, so that CR terminated the blank line and the event did happen"
        );
        assert!(
            !losses.truncated_at_end,
            "nothing was cut short here — the stream delivered a complete event"
        );
    }

    /// And when the CR *was* the first half of a CRLF, the next chunk says so and nothing extra is
    /// dispatched in between.
    #[test]
    fn a_trailing_cr_followed_by_lf_is_not_two_terminators() {
        let mut parser = SseParser::default();
        assert!(events(&mut parser, "data: x\r").is_empty());
        let dispatched = events(&mut parser, "\n\r\n");

        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0].data, "x");
        assert_eq!(parser.events(), 1);
    }

    #[test]
    fn a_multi_byte_character_split_across_chunks_survives() {
        let mut parser = SseParser::default();
        let text = "data: 你好\n\n".as_bytes().to_vec();
        // Split inside the first character's three bytes.
        assert!(parser.push(&text[..7]).is_empty());
        let dispatched = parser.push(&text[7..]);

        assert_eq!(dispatched.len(), 1);
        assert_eq!(
            dispatched[0].data, "你好",
            "splitting a code point across chunks must not corrupt it"
        );
    }

    #[test]
    fn comments_are_ignored() {
        let mut parser = SseParser::default();
        let dispatched = events(&mut parser, ": keep-alive\n\ndata: real\n\n");

        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0].data, "real");
    }

    #[test]
    fn an_event_type_without_data_dispatches_nothing() {
        let mut parser = SseParser::default();
        assert!(events(&mut parser, "event: only-a-type\n\n").is_empty());
        // And the type must not leak into the next event, which the spec resets.
        let dispatched = events(&mut parser, "data: real\n\n");
        assert_eq!(dispatched[0].event, None);
        assert_eq!(dispatched[0].kind(), "message");
    }

    #[test]
    fn id_and_retry_are_stream_state_carried_onto_later_events() {
        let mut parser = SseParser::default();
        let first = events(&mut parser, "id: 7\nretry: 1500\ndata: a\n\n");
        let second = events(&mut parser, "data: b\n\n");

        assert_eq!(first[0].id.as_deref(), Some("7"));
        assert_eq!(first[0].retry, Some(1500));
        assert_eq!(
            second[0].id.as_deref(),
            Some("7"),
            "an event with no id keeps the last one in effect"
        );
        assert_eq!(second[0].retry, Some(1500));
        assert_eq!(parser.retry(), Some(1500));
    }

    #[test]
    fn a_retry_that_is_not_a_number_is_ignored() {
        let mut parser = SseParser::default();
        events(&mut parser, "retry: soon\nretry: 250\ndata: a\n\n");
        assert_eq!(parser.retry(), Some(250));

        let mut other = SseParser::default();
        events(&mut other, "retry: -5\ndata: a\n\n");
        assert_eq!(other.retry(), None, "a negative retry is not a retry");
    }

    #[test]
    fn an_id_containing_nul_is_ignored_rather_than_truncated() {
        let mut parser = SseParser::default();
        let dispatched = events(&mut parser, "id: bad\0id\ndata: a\n\n");
        assert_eq!(dispatched[0].id, None);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let mut parser = SseParser::default();
        let dispatched = events(&mut parser, "future-field: whatever\ndata: a\n\n");
        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0].data, "a");
    }

    #[test]
    fn a_line_without_a_colon_is_a_field_with_an_empty_value() {
        let mut parser = SseParser::default();
        // `data` alone is a `data` line with an empty value, so the event exists and its payload is
        // the empty string — which is different from no event at all.
        let dispatched = events(&mut parser, "data\n\n");
        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0].data, "");
    }

    #[test]
    fn an_oversized_event_is_dropped_and_counted_rather_than_truncated() {
        let mut parser = SseParser::new(16);
        let dispatched = events(&mut parser, "data: 0123456789abcdefghij\n\n");

        assert!(
            dispatched.is_empty(),
            "a payload over budget must not be reported as an event"
        );
        assert_eq!(parser.losses().oversized, 1);
        assert_eq!(parser.events(), 0);

        // And the parser recovers: the next event is dispatched normally.
        let next = events(&mut parser, "data: ok\n\n");
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].data, "ok");
    }

    #[test]
    fn many_small_events_are_unaffected_by_the_per_event_cap() {
        let mut parser = SseParser::new(32);
        let mut total = 0;
        for i in 0..1000 {
            total += events(&mut parser, &format!("data: {i}\n\n")).len();
        }
        assert_eq!(total, 1000);
        assert_eq!(parser.losses().oversized, 0);
    }

    #[test]
    fn a_line_that_never_ends_is_abandoned_and_counted() {
        let mut parser = SseParser::new(8);
        // No terminator anywhere: the parser must not accumulate it forever.
        let dispatched = parser.push(&vec![b'x'; MAX_LINE_BYTES + 10]);
        assert!(dispatched.is_empty());
        assert_eq!(parser.losses().unterminated_line, 1);

        // The stream is still usable afterwards.
        let next = parser.push(b"data: ok\n\n");
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].data, "ok");
    }

    #[test]
    fn an_event_without_its_blank_line_is_not_dispatched_at_the_end() {
        let mut parser = SseParser::default();
        assert!(events(&mut parser, "event: cut\ndata: half").is_empty());

        let losses = parser.finish();
        assert!(
            losses.truncated_at_end,
            "an event the client never received must be reported as truncated, not as an event"
        );
        assert_eq!(
            parser.events(),
            0,
            "reporting it would invent something that never reached the client"
        );
    }

    #[test]
    fn finishing_a_clean_stream_reports_nothing_lost() {
        let mut parser = SseParser::default();
        events(&mut parser, "data: a\n\n");
        let losses = parser.finish();

        assert!(!losses.truncated_at_end);
        assert_eq!(losses.oversized, 0);
        assert_eq!(parser.events(), 1);
    }

    #[test]
    fn a_stream_of_many_events_is_parsed_in_one_pass() {
        let mut parser = SseParser::default();
        let body: String = (0..5).map(|i| format!("event: n\ndata: {i}\n\n")).collect();
        let dispatched = events(&mut parser, &body);

        assert_eq!(dispatched.len(), 5);
        assert_eq!(dispatched[4].data, "4");
        assert_eq!(parser.events(), 5);
    }

    /// Small helper so the happy-path test can state "nothing was lost" without repeating three
    /// fields; `SseLosses` is compared directly everywhere it matters.
    struct SseStats;
    impl SseStats {
        fn default() -> Self {
            Self
        }
        fn losses_marker(&self) -> SseLosses {
            SseLosses::default()
        }
    }
}
