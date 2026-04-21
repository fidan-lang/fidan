use fidan_source::SourceFile;

#[derive(Debug, Clone)]
pub struct FmtComment {
    pub start: u32,
    pub text: String,
    pub inline: bool,
}

pub fn collect_comments(file: &SourceFile) -> Vec<FmtComment> {
    let src = file.src.as_ref();
    let mut comments = Vec::new();
    let mut pos = 0usize;

    while pos < src.len() {
        let rest = &src[pos..];

        if rest.starts_with("r\"") {
            pos += 2;
            while pos < src.len() {
                let ch = src[pos..].chars().next().expect("char boundary");
                pos += ch.len_utf8();
                if ch == '"' {
                    break;
                }
            }
            continue;
        }

        let ch = rest.chars().next().expect("char boundary");
        if ch == '"' {
            pos += 1;
            while pos < src.len() {
                let inner = &src[pos..];
                let inner_ch = inner.chars().next().expect("char boundary");
                pos += inner_ch.len_utf8();
                match inner_ch {
                    '\\' if pos < src.len() => {
                        let escaped = src[pos..].chars().next().expect("char boundary");
                        pos += escaped.len_utf8();
                    }
                    '"' => break,
                    _ => {}
                }
            }
            continue;
        }

        if ch == '#' {
            let start = pos;
            let inline = has_code_before_on_line(src, start);
            if rest.starts_with("#/") {
                pos += 2;
                let mut depth = 1u32;
                while pos < src.len() && depth > 0 {
                    let tail = &src[pos..];
                    if tail.starts_with("#/") {
                        depth += 1;
                        pos += 2;
                    } else if tail.starts_with("/#") {
                        depth -= 1;
                        pos += 2;
                    } else {
                        let nested = tail.chars().next().expect("char boundary");
                        pos += nested.len_utf8();
                    }
                }
            } else {
                pos += 1;
                while pos < src.len() {
                    let inner = &src[pos..];
                    let inner_ch = inner.chars().next().expect("char boundary");
                    if inner_ch == '\n' {
                        break;
                    }
                    pos += inner_ch.len_utf8();
                }
            }
            comments.push(FmtComment {
                start: start as u32,
                text: src[start..pos].to_string(),
                inline,
            });
            continue;
        }

        pos += ch.len_utf8();
    }

    comments
}

fn has_code_before_on_line(src: &str, offset: usize) -> bool {
    let line_start = src[..offset].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    src[line_start..offset]
        .chars()
        .any(|ch| !matches!(ch, ' ' | '\t' | '\r'))
}

pub fn normalize_comment_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().map(|line| line.to_string()).collect();
    if lines.is_empty() {
        return lines;
    }

    let min_indent = lines
        .iter()
        .filter_map(|line| {
            if line.trim().is_empty() {
                None
            } else {
                Some(
                    line.chars()
                        .take_while(|ch| *ch == ' ' || *ch == '\t')
                        .count(),
                )
            }
        })
        .min()
        .unwrap_or(0);

    if min_indent == 0 {
        return lines;
    }

    for line in &mut lines {
        let trimmed = line
            .chars()
            .take(min_indent)
            .all(|ch| ch == ' ' || ch == '\t');
        if trimmed {
            *line = line.chars().skip(min_indent).collect();
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_source::{FileId, SourceFile};

    #[test]
    fn collects_line_and_block_comments_without_confusing_strings() {
        let file = SourceFile::new(
            FileId(0),
            "<comments>",
            "var x = \"# nope\"\n# heading\nvar y = 2 # tail\n#/ block\n  keep\n/#\n",
        );
        let comments = collect_comments(&file);
        assert_eq!(comments.len(), 3);
        assert_eq!(comments[0].text, "# heading");
        assert!(!comments[0].inline);
        assert_eq!(comments[1].text, "# tail");
        assert!(comments[1].inline);
        assert!(comments[2].text.starts_with("#/ block"));
    }

    #[test]
    fn raw_strings_do_not_start_comments() {
        let file = SourceFile::new(FileId(0), "<raw>", "print(r\"\\n # kept raw\")\n# real\n");
        let comments = collect_comments(&file);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].text, "# real");
    }
}
