// https://docs.rs/tree-sitter/latest/tree_sitter/#editing
// Maybe good to check for faster parse on code change.
use ropey::Rope;
use tree_sitter::{InputEdit, Parser, Tree};

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("Failed to load language: {0}")]
    LanguageLoad(#[from] tree_sitter::LanguageError),
    #[error("Failed to parse document")]
    ParseFailed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodeChunk {
    pub language: String,
    pub content: String,
    pub start_line: u32, // first line of the code (not the fence)
    pub end_line: u32,   // last line of the code (not the fence)
}

pub struct DocumentParser {
    parser: Parser,
    tree: Option<Tree>,
    rope: Rope,
}

impl std::fmt::Debug for DocumentParser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentParser")
            .field("has_tree", &self.tree.is_some())
            .finish()
    }
}

impl DocumentParser {
    pub fn new(source: &str) -> Result<(Self, Vec<CodeChunk>), ParseError> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_md::LANGUAGE.into())?;

        let rope = Rope::from_str(source);
        let source = ensure_trailing_newline(source);

        let tree = parser
            .parse(source.as_bytes(), None)
            .ok_or(ParseError::ParseFailed)?;

        let mut chunks = Vec::new();
        collect_chunks(tree.root_node(), source.as_bytes(), &mut chunks);

        Ok((
            Self {
                parser,
                tree: Some(tree),
                rope,
            },
            chunks,
        ))
    }

    pub fn update(&mut self, new_source: &str) -> Result<Vec<CodeChunk>, ParseError> {
        let new_rope = Rope::from_str(new_source);
        let old_rope = &self.rope;

        // Compute the edit range by diffing the old and new rope
        let edit = compute_edit(old_rope, &new_rope);

        // Tell TS about it
        if let Some(ref mut tree) = self.tree {
            tree.edit(&edit);
        }

        // Re-parse with the old tree for incremental speedup
        let source = ensure_trailing_newline(new_source);
        let new_tree = self
            .parser
            .parse(source.as_bytes(), self.tree.as_ref())
            .ok_or(ParseError::ParseFailed)?;

        let mut chunks = Vec::new();
        collect_chunks(new_tree.root_node(), source.as_bytes(), &mut chunks);

        self.tree = Some(new_tree);
        self.rope = new_rope;

        Ok(chunks)
    }
}

fn compute_edit(old_rope: &Rope, new_rope: &Rope) -> InputEdit {
    // Find first differing byte from the start
    let old_bytes: Vec<u8> = old_rope.bytes().collect();
    let new_bytes: Vec<u8> = new_rope.bytes().collect();

    let start_byte = old_bytes
        .iter()
        .zip(new_bytes.iter())
        .position(|(a, b)| a != b)
        .unwrap_or(old_bytes.len().min(new_bytes.len()));

    // Find first differing byte from the end
    let common_suffix = old_bytes[start_byte..]
        .iter()
        .rev()
        .zip(new_bytes[start_byte..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();

    let old_end_byte = old_bytes.len() - common_suffix;
    let new_end_byte = new_bytes.len() - common_suffix;

    // Convert byte offsets to tree-sitter Points (row, column)
    let start_point = byte_to_point(old_rope, start_byte);
    let old_end_point = byte_to_point(old_rope, old_end_byte);
    let new_end_point = byte_to_point(new_rope, new_end_byte);

    InputEdit {
        start_byte,
        old_end_byte,
        new_end_byte,
        start_position: start_point,
        old_end_position: old_end_point,
        new_end_position: new_end_point,
    }
}

fn byte_to_point(rope: &Rope, byte_offset: usize) -> tree_sitter::Point {
    use ropey::LineType;
    let byte_offset = byte_offset.min(rope.len());
    let line = rope.byte_to_line_idx(byte_offset, LineType::LF_CR);
    let line_start = rope.line_to_byte_idx(line, LineType::LF_CR);
    let col = byte_offset - line_start;
    tree_sitter::Point {
        row: line,
        column: col,
    }
}

fn ensure_trailing_newline(source: &str) -> String {
    if source.ends_with('\n') {
        source.to_string()
    } else {
        format!("{source}\n")
    }
}

fn collect_chunks(node: tree_sitter::Node, source: &[u8], chunks: &mut Vec<CodeChunk>) {
    if node.kind() == "fenced_code_block" {
        let mut language = None;
        let mut content_node = None;

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "info_string" => {
                    let raw = child.utf8_text(source).unwrap_or("");
                    if raw.starts_with('{') {
                        let mut ic = child.walk();
                        for info_child in child.children(&mut ic) {
                            if info_child.kind() == "language" {
                                language = info_child.utf8_text(source).ok().map(str::to_owned);
                            }
                        }
                    }
                }
                // The content of the code block is usually in a child node named
                // "code_fence_content".
                "code_fence_content" => content_node = Some(child),
                _ => {}
            }
        }

        // if we have both a language and content, we can create a CodeChunk. If either is missing,
        // we skip this block.
        if let (Some(lang), Some(cn)) = (language, content_node) {
            let raw = cn.utf8_text(source).unwrap_or("");
            let content = raw.trim_end_matches('\n').to_owned();
            let start_line = cn.start_position().row as u32;
            let end_line = (cn.end_position().row - 1) as u32;

            chunks.push(CodeChunk {
                language: lang,
                content,
                start_line,
                end_line,
            });
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_chunks(child, source, chunks);
    }
}

/// Returns the language of the code chunk that contains `line`, or `None` if the line is in
/// markdown/yaml/prose rather than a code chunk.
pub fn language_at_position(chunks: &[CodeChunk], line: u32) -> Option<&str> {
    chunks
        .iter()
        .find(|c| c.start_line <= line && line <= c.end_line)
        .map(|c| c.language.as_str())
}

#[cfg(test)]
mod test {
    use super::DocumentParser;

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
    fn test_language_at_position() {
        let chunks = vec![
            super::CodeChunk {
                language: "python".to_string(),
                content: "x = 1".to_string(),
                start_line: 2,
                end_line: 4,
            },
            super::CodeChunk {
                language: "r".to_string(),
                content: "x <- 1".to_string(),
                start_line: 8,
                end_line: 10,
            },
        ];

        assert_eq!(super::language_at_position(&chunks, 2), Some("python"));
        assert_eq!(super::language_at_position(&chunks, 3), Some("python"));
        assert_eq!(super::language_at_position(&chunks, 4), Some("python"));
        assert_eq!(super::language_at_position(&chunks, 5), None); // prose
        assert_eq!(super::language_at_position(&chunks, 9), Some("r"));
        assert_eq!(super::language_at_position(&chunks, 0), None); // before any chunk
    }

    #[test]
    fn test_parse_qmd() {
        let source = fixture!("mixed_languages.qmd");

        let chunks = DocumentParser::new(source).unwrap().1;

        insta::assert_debug_snapshot!(chunks);
    }

    #[test]
    fn test_plain_fence_skipped() {
        let source = fixture!("plain_fence.qmd");

        let chunks = DocumentParser::new(source).unwrap().1;
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_integration_realistic_qmd() {
        let source = fixture!("python_r.qmd");
        let chunks = DocumentParser::new(source).unwrap().1;
        // #| lines are kept in content for 1:1 buffer mapping
        // start_line/end_line refer to actual buffer positions (not fence lines)
        insta::assert_debug_snapshot!(chunks);
    }

    #[test]
    fn test_incremental_parse() {
        let source = "# Title\n\n```{python}\nx = 1\n```\n";
        let (mut parser, chunks) = DocumentParser::new(source).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "x = 1");

        let new_source = "# Title\n\n```{python}\nx = 2\n```\n";
        let chunks = parser.update(new_source).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "x = 2");
    }
}
