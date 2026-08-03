//! Parse the pentest methodology markdown into a navigable tree for the
//! Methodology tab. Headings nest by level; `- [ ]` / `- [x]` lines become
//! check items (checked state stored inline); everything else (bullets,
//! blockquotes, prose) becomes a note. Each node records its original source
//! line so the UI can flip a checkbox or splice an `$EDITOR` edit back into the
//! markdown without a serializer.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub enum MethodKind {
    Heading(u8),
    Check,
    Note,
}

#[derive(Debug, Clone)]
pub struct MethodNode {
    pub title: String,
    pub kind: MethodKind,
    pub indent: usize,
    pub checked: bool,
    /// 0-based index of this node's line in the source markdown.
    pub src_line: usize,
    pub refs: Vec<String>,
    pub anchor: Option<String>,
    pub children: Vec<MethodNode>,
}

impl MethodNode {
    pub fn is_heading(&self) -> bool {
        matches!(self.kind, MethodKind::Heading(_))
    }
}

struct Line {
    heading: Option<u8>,
    kind: MethodKind,
    indent: usize,
    checked: bool,
    src_line: usize,
    text: String,
    refs: Vec<String>,
    anchor: Option<String>,
}

fn ref_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"§(T-[A-Z0-9]+(?:-[A-Z0-9]+)*)").unwrap())
}

fn collect_refs(s: &str) -> Vec<String> {
    ref_re().captures_iter(s).map(|c| c[1].to_string()).collect()
}

/// Strip the markdown emphasis/code markers we don't render in the TUI.
fn clean(s: &str) -> String {
    s.replace("**", "").replace('`', "").trim().to_string()
}

fn parse_line(src_line: usize, raw: &str) -> Option<Line> {
    let stripped = raw.trim_start();
    let t = stripped.trim_end();
    if t.is_empty() {
        return None;
    }
    // horizontal rule
    if t.len() >= 3 && t.chars().all(|c| c == '-') {
        return None;
    }
    // heading (#..######)
    if let Some(after_first) = t.strip_prefix('#') {
        let level = (1 + after_first.chars().take_while(|&c| c == '#').count()).min(6) as u8;
        let title_raw = t.trim_start_matches('#').trim();
        let anchor = if title_raw.starts_with("§T-") {
            collect_refs(title_raw).into_iter().next()
        } else {
            None
        };
        return Some(Line {
            heading: Some(level),
            kind: MethodKind::Heading(level),
            indent: 0,
            checked: false,
            src_line,
            text: clean(title_raw),
            refs: collect_refs(title_raw),
            anchor,
        });
    }
    let indent = raw.len() - stripped.len();
    // checklist item: "- [ ] ..." / "- [x] ..."
    if let Some(rest) = stripped.strip_prefix("- [") {
        let b = rest.as_bytes();
        if b.len() >= 2 && b[1] == b']' {
            let checked = b[0] == b'x' || b[0] == b'X';
            let body = rest[2..].trim();
            return Some(Line {
                heading: None,
                kind: MethodKind::Check,
                indent,
                checked,
                src_line,
                text: clean(body),
                refs: collect_refs(body),
                anchor: None,
            });
        }
    }
    // plain bullet / blockquote / prose -> note
    let body = stripped
        .strip_prefix("- ")
        .or_else(|| stripped.strip_prefix("> "))
        .or_else(|| stripped.strip_prefix('>'))
        .unwrap_or(stripped);
    Some(Line {
        heading: None,
        kind: MethodKind::Note,
        indent,
        checked: false,
        src_line,
        text: clean(body),
        refs: collect_refs(body),
        anchor: None,
    })
}

fn node_from_line(l: &Line) -> MethodNode {
    MethodNode {
        title: l.text.clone(),
        kind: l.kind.clone(),
        indent: l.indent,
        checked: l.checked,
        src_line: l.src_line,
        refs: l.refs.clone(),
        anchor: l.anchor.clone(),
        children: Vec::new(),
    }
}

fn build_block(lines: &[Line], pos: &mut usize, parent_level: u8) -> Vec<MethodNode> {
    let mut out = Vec::new();
    while *pos < lines.len() {
        match lines[*pos].heading {
            Some(hl) => {
                if hl <= parent_level {
                    break;
                }
                let mut node = node_from_line(&lines[*pos]);
                node.indent = 0;
                *pos += 1;
                node.children = build_block(lines, pos, hl);
                out.push(node);
            }
            None => {
                let mut node = node_from_line(&lines[*pos]);
                node.anchor = None;
                out.push(node);
                *pos += 1;
            }
        }
    }
    out
}

pub fn parse(md: &str) -> Vec<MethodNode> {
    let lines: Vec<Line> = md
        .lines()
        .enumerate()
        .filter_map(|(i, l)| parse_line(i, l))
        .collect();
    let mut pos = 0;
    build_block(&lines, &mut pos, 0)
}

fn section_re() -> &'static Regex {
    // A section heading starts with an arabic number or a roman numeral, then a
    // dot: "# 4. Server-Side Attacks", "# XII. Post-Compromise".
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^(?:\d+|[IVXLCDM]+)\.").unwrap())
}

/// The top-level engagement sections — the numbered `# ...` headings that become
/// the Methodology sub-tabs. The document title and Table of Contents are skipped.
pub fn sections(tree: &[MethodNode]) -> Vec<&MethodNode> {
    tree.iter()
        .filter(|n| n.is_heading() && section_re().is_match(n.title.trim_start()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(nodes: &[MethodNode], c: &mut usize, checked: &mut usize) {
        for n in nodes {
            if n.kind == MethodKind::Check {
                *c += 1;
                if n.checked {
                    *checked += 1;
                }
            }
            walk(&n.children, c, checked);
        }
    }

    #[test]
    fn parses_web_doc() {
        let md = std::fs::read_to_string("JSONs/methodology/web.md").unwrap();
        let tree = parse(&md);
        let secs = sections(&tree);
        let (mut checks, mut checked) = (0, 0);
        walk(&tree, &mut checks, &mut checked);
        eprintln!("web: sections={} checks={checks} checked={checked}", secs.len());
        assert_eq!(secs.len(), 8, "expected 8 web sections");
        assert!(checks > 200, "expected many checks, got {checks}");
        let server = secs[3];
        assert!(
            server.children.iter().any(|c| c.title == "SQL Injection"),
            "SQL Injection card should exist under section 4"
        );
    }

    #[test]
    fn parses_ad_doc() {
        let md = std::fs::read_to_string("JSONs/methodology/ad.md").unwrap();
        let tree = parse(&md);
        let secs = sections(&tree);
        eprintln!("ad: sections={}", secs.len());
        // Roman-numeral sections I..XVI.
        assert_eq!(secs.len(), 16, "expected 16 AD sections");
        assert_eq!(secs[0].title.trim_start_matches("I. "), "External Recon");
    }
}

