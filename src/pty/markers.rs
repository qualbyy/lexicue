//! OSC 133 semantic prompt marker parser.
//!
//! Recognizes FTCS (FinalTerm / iTerm2 / WezTerm / shell integration) prompt markers:
//! - `OSC 133 ; A ST` => PromptStart
//! - `OSC 133 ; B ST` => InputStart
//! - `OSC 133 ; C ST` => CommandStart
//! - `OSC 133 ; D [; exit_code] ST` => Finished { exit_code }
//!
//! Sequences are terminated by BEL (`\x07`) or ST (`ESC \`).
//! Split chunks across PTY reads are handled with a bounded tail buffer (<= 128 bytes).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptMarker {
    PromptStart,                         // OSC 133;A
    InputStart,                          // OSC 133;B
    CommandStart,                        // OSC 133;C
    Finished { exit_code: Option<i32> }, // OSC 133;D[;n]
}

pub struct MarkerParser {
    tail: Vec<u8>,
}

impl MarkerParser {
    const MAX_TAIL: usize = 128;

    pub fn new() -> Self {
        Self {
            tail: Vec::with_capacity(Self::MAX_TAIL),
        }
    }

    /// Feed byte stream and extract complete PromptMarker events.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<PromptMarker> {
        let mut stream = Vec::with_capacity(self.tail.len() + bytes.len());
        stream.extend_from_slice(&self.tail);
        stream.extend_from_slice(bytes);
        self.tail.clear();

        let mut markers = Vec::new();
        let mut idx = 0;
        let len = stream.len();

        while idx < len {
            // Find ESC ] (0x1b, 0x5d)
            if stream[idx] != 0x1b {
                idx += 1;
                continue;
            }

            if idx + 1 >= len {
                // Potential split ESC at boundary
                break;
            }

            if stream[idx + 1] != b']' {
                idx += 1;
                continue;
            }

            // Look for terminator: BEL (0x07) or ESC \ (0x1b 0x5c)
            let mut term_idx = None;
            let mut term_len = 0;
            let mut search = idx + 2;
            while search < len {
                if stream[search] == 0x07 {
                    term_idx = Some(search);
                    term_len = 1;
                    break;
                } else if stream[search] == 0x1b {
                    if search + 1 < len {
                        if stream[search + 1] == b'\\' {
                            term_idx = Some(search);
                            term_len = 2;
                            break;
                        }
                    } else {
                        // Incomplete ESC at the end
                        break;
                    }
                }
                search += 1;
            }

            if let Some(t_idx) = term_idx {
                let body = &stream[idx + 2..t_idx];
                if let Some(marker) = Self::parse_osc_133_payload(body) {
                    markers.push(marker);
                }
                idx = t_idx + term_len;
            } else {
                // Unterminated sequence. If remaining bytes are within MAX_TAIL, save into tail.
                break;
            }
        }

        if idx < len {
            let rem = &stream[idx..];
            let keep_start = if rem.len() > Self::MAX_TAIL {
                rem.len() - Self::MAX_TAIL
            } else {
                0
            };
            self.tail.extend_from_slice(&rem[keep_start..]);
        }

        markers
    }

    fn parse_osc_133_payload(payload: &[u8]) -> Option<PromptMarker> {
        // Must start with "133;"
        if !payload.starts_with(b"133;") {
            return None;
        }
        let rest = &payload[4..];
        if rest.is_empty() {
            return None;
        }

        match rest[0] {
            b'A' => Some(PromptMarker::PromptStart),
            b'B' => Some(PromptMarker::InputStart),
            b'C' => Some(PromptMarker::CommandStart),
            b'D' => {
                if rest.len() == 1 {
                    Some(PromptMarker::Finished { exit_code: None })
                } else if rest[1] == b';' {
                    let code_str = std::str::from_utf8(&rest[2..]).ok()?;
                    let code = code_str.trim().parse::<i32>().ok();
                    Some(PromptMarker::Finished { exit_code: code })
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

impl Default for MarkerParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_markers_with_bel_and_st() {
        let mut p = MarkerParser::new();
        assert_eq!(p.feed(b"\x1b]133;A\x07"), vec![PromptMarker::PromptStart]);
        assert_eq!(p.feed(b"\x1b]133;B\x1b\\"), vec![PromptMarker::InputStart]);
        assert_eq!(p.feed(b"\x1b]133;C\x07"), vec![PromptMarker::CommandStart]);
        assert_eq!(
            p.feed(b"\x1b]133;D\x07"),
            vec![PromptMarker::Finished { exit_code: None }]
        );
        assert_eq!(
            p.feed(b"\x1b]133;D;0\x1b\\"),
            vec![PromptMarker::Finished { exit_code: Some(0) }]
        );
        assert_eq!(
            p.feed(b"\x1b]133;D;127\x07"),
            vec![PromptMarker::Finished {
                exit_code: Some(127)
            }]
        );
    }

    #[test]
    fn handles_chunked_split_feed() {
        let mut p = MarkerParser::new();
        let chunk1 = b"some random output \x1b]133";
        let chunk2 = b";D;1\x07 normal text continues";

        assert_eq!(p.feed(chunk1), vec![]);
        assert_eq!(
            p.feed(chunk2),
            vec![PromptMarker::Finished { exit_code: Some(1) }]
        );
    }

    #[test]
    fn ignores_other_oscs_and_malformed() {
        let mut p = MarkerParser::new();
        assert_eq!(p.feed(b"\x1b]7;file:///home/user\x07"), vec![]);
        assert_eq!(p.feed(b"\x1b]133;X\x07"), vec![]);
        assert_eq!(
            p.feed(b"\x1b]133;D;abc\x07"),
            vec![PromptMarker::Finished { exit_code: None }]
        );
    }
}
