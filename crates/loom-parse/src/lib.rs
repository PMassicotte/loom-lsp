// https://docs.rs/tree-sitter/latest/tree_sitter/#editing
// Maybe good to check for faster parse on code change.
use tree_sitter::Parser;

#[derive(Debug, Clone, PartialEq)]
pub struct CodeChunk {
    pub language: String,
    pub content: String,
    pub start_line: u32, // first line of the code (not the fence)
    pub end_line: u32,   // last line of the code (not the fence)
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("Failed to load language: {0}")]
    LanguageLoad(#[from] tree_sitter::LanguageError),
    #[error("Failed to parse document")]
    ParseFailed,
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

pub fn parse_qmd(source: &str) -> Result<Vec<CodeChunk>, ParseError> {
    let mut parser = Parser::new();

    parser.set_language(&tree_sitter_md::LANGUAGE.into())?;

    let tree = parser.parse(source, None).ok_or(ParseError::ParseFailed)?;
    let root_node = tree.root_node();

    let mut chunks = Vec::new();
    collect_chunks(root_node, source.as_bytes(), &mut chunks);

    Ok(chunks)
}

#[cfg(test)]
mod test {
    #[test]
    fn test_parse_qmd() {
        let source = r#"```{r}
x <- 1:10
y <- x^2
plot(x, y)
```
```{python}
import matplotlib.pyplot as plt
x = range(1, 11)
y = [i**2 for i in x]
plt.plot(x, y)
plt.show()
```

"#;

        let chunks = super::parse_qmd(source).unwrap();
        insta::assert_debug_snapshot!(chunks);
    }

    #[test]
    fn test_plain_fence_skipped() {
        let source = "```r\nx <- 1\n```\n";
        let chunks = super::parse_qmd(source).unwrap();
        assert!(chunks.is_empty());
    }
}
