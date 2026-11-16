const PASTE_END: &str = "\x1b[201~";

#[derive(Default)]
pub struct PromptBuffer {
    line: String,
    escape: Escape,
    pasted: String,
}

#[derive(Default, PartialEq)]
enum Escape {
    #[default]
    None,
    Start,
    Csi(String),
    Ss3,
    Paste,
}

impl PromptBuffer {
    pub fn feed(&mut self, data: &str) -> Vec<String> {
        let mut prompts = Vec::new();
        for character in data.chars() {
            prompts.extend(self.push(character));
        }
        prompts
    }

    fn push(&mut self, character: char) -> Option<String> {
        match std::mem::take(&mut self.escape) {
            Escape::None => return self.plain(character),
            Escape::Start => self.after_escape(character),
            Escape::Csi(params) => self.csi(params, character),
            Escape::Ss3 => {}
            Escape::Paste => self.paste(character),
        }
        None
    }

    fn plain(&mut self, character: char) -> Option<String> {
        match character {
            '\r' | '\n' => return self.submit(),
            '\x7f' | '\x08' => {
                self.line.pop();
            }
            '\x03' | '\x15' => self.line.clear(),
            '\x1b' => self.escape = Escape::Start,
            '\t' => self.line.push(character),
            character if character.is_control() => {}
            character => self.line.push(character),
        }
        None
    }

    fn after_escape(&mut self, character: char) {
        self.escape = match character {
            '[' => Escape::Csi(String::new()),
            'O' => Escape::Ss3,
            _ => Escape::None,
        };
    }

    fn csi(&mut self, mut params: String, character: char) {
        if !('\x40'..='\x7e').contains(&character) {
            params.push(character);
            self.escape = Escape::Csi(params);
        } else if params == "200" && character == '~' {
            self.escape = Escape::Paste;
        }
    }

    fn paste(&mut self, character: char) {
        self.pasted.push(character);
        if let Some(text) = self.pasted.strip_suffix(PASTE_END) {
            self.line.push_str(text);
            self.pasted.clear();
        } else {
            self.escape = Escape::Paste;
        }
    }

    fn submit(&mut self) -> Option<String> {
        let prompt = std::mem::take(&mut self.line);
        (!prompt.trim().is_empty()).then_some(prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASTE_START: &str = "\x1b[200~";

    fn prompts(data: &str) -> Vec<String> {
        PromptBuffer::default().feed(data)
    }

    #[test]
    fn submits_on_enter() {
        assert_eq!(prompts("hello\rworld\n"), ["hello", "world"]);
    }

    #[test]
    fn applies_backspace() {
        assert_eq!(prompts("helx\x7flo\x08o\r"), ["hello"]);
    }

    #[test]
    fn drops_escape_sequences() {
        assert_eq!(prompts("a\x1b[Ab\x1bOCc\x1b[1;5Dd\r"), ["abcd"]);
    }

    #[test]
    fn keeps_pasted_text() {
        assert_eq!(
            prompts(&format!(
                "fix {PASTE_START}line one\nline two{PASTE_END} now\r"
            )),
            ["fix line one\nline two now"]
        );
    }

    #[test]
    fn keeps_partial_line_across_feeds() {
        let mut buffer = PromptBuffer::default();
        assert!(buffer.feed("par").is_empty());
        assert_eq!(buffer.feed("tial\r"), ["partial"]);
    }

    #[test]
    fn skips_empty_lines() {
        assert!(prompts("\r\n  \r").is_empty());
    }
}
