use loom_parse::CodeChunk;
use std::collections::HashMap;
use std::ops::Range;

#[derive(Debug, Clone)]
pub struct VirtualDocument {
    pub language: String,
    pub content: String,
    pub version: i32,
    pub live_ranges: Vec<Range<u32>>,
}

impl VirtualDocument {
    pub fn is_live(&self, line: u32) -> bool {
        self.live_ranges.iter().any(|r| r.contains(&line))
    }
}

pub fn build_virtual_docs(chunks: &[CodeChunk], total_lines: u32) -> Vec<VirtualDocument> {
    let mut by_language: HashMap<String, Vec<&CodeChunk>> = HashMap::new();

    for chunk in chunks {
        by_language
            .entry(chunk.language.clone())
            .or_default()
            .push(chunk);
    }

    let mut vdoc: Vec<VirtualDocument> = Vec::with_capacity(by_language.len());

    for (language, chunks) in by_language {
        let mut lines: Vec<&str> = vec![""; total_lines as usize];

        for chunk in &chunks {
            for (i, line) in chunk.content.lines().enumerate() {
                lines[chunk.start_line as usize + i] = line;
            }
        }

        let content = lines.join("\n");

        let live_ranges: Vec<Range<u32>> = chunks
            .iter()
            .map(|chunk| chunk.start_line..chunk.end_line + 1)
            .collect();

        vdoc.push(VirtualDocument {
            language,
            content,
            version: 0,
            live_ranges,
        });
    }

    vdoc.sort_by(|a, b| a.language.cmp(&b.language));
    vdoc
}

#[cfg(test)]
mod test {
    use loom_parse::parse_qmd;

    use crate::build_virtual_docs;

    macro_rules! fixture {
        ($name:expr) => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/",
                $name
            ))
        };
    }

    #[test]
    fn test_creating_virtual_documents() {
        let input_str = fixture!("mixed_languages.qmd");
        let total_lines = input_str.lines().count();

        let chunks = parse_qmd(input_str).unwrap();
        let vdoc = build_virtual_docs(&chunks, total_lines as u32);

        insta::assert_debug_snapshot!(vdoc);
    }
}
