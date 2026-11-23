/// Splits a byte stream into newline-delimited records, buffering partial lines across chunks.
#[derive(Debug, Default)]
pub struct NdjsonDecoder {
    pending: Vec<u8>,
}

impl NdjsonDecoder {
    /// Adds a chunk and returns every line it completes, without the trailing newline. Blank lines
    /// are skipped.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(newline) = self.pending.iter().position(|&byte| byte == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=newline).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if !line.iter().all(u8::is_ascii_whitespace) {
                lines.push(line);
            }
        }
        lines
    }

    /// Returns the final line if the stream ended without a trailing newline.
    pub fn finish(&mut self) -> Option<Vec<u8>> {
        let line = std::mem::take(&mut self.pending);
        (!line.iter().all(u8::is_ascii_whitespace)).then_some(line)
    }
}

#[cfg(test)]
#[path = "ndjson_tests.rs"]
mod tests;
