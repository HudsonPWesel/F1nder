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
    /// A checklist item: `- [ ]`/`- [x]`, or a bare `- text` sub-bullet (which
    /// is treated as an unchecked todo).
    Check,
    /// Floating prose or a `> blockquote` — a standalone comment.
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

    fn has_check_descendant(&self) -> bool {
        self.children
            .iter()
            .any(|c| c.kind == MethodKind::Check || c.has_check_descendant())
    }

    /// A `Check` with no `Check` descendant — a real, directly-checkable todo
    /// (it may still have `Note` children). Parent checks are derived, not counted.
    pub fn is_leaf_check(&self) -> bool {
        self.kind == MethodKind::Check && !self.has_check_descendant()
    }

    /// `(done, total)` counting only leaf checks in this subtree, so a parent and
    /// its children are never double-counted.
    pub fn leaf_counts(&self) -> (usize, usize) {
        let (mut done, mut total) = (0, 0);
        if self.is_leaf_check() {
            total += 1;
            if self.checked {
                done += 1;
            }
        }
        for c in &self.children {
            let (d, t) = c.leaf_counts();
            done += d;
            total += t;
        }
        (done, total)
    }

    /// Whether every leaf check in this subtree is checked (used for a parent's
    /// derived checked state). Vacuously true when there are no leaf checks.
    pub fn all_leaves_checked(&self) -> bool {
        let (done, total) = self.leaf_counts();
        total > 0 && done == total
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
    // A bare `- text` dash bullet under an entry is an unchecked todo.
    if let Some(rest) = stripped.strip_prefix("- ") {
        return Some(Line {
            heading: None,
            kind: MethodKind::Check,
            indent,
            checked: false,
            src_line,
            text: clean(rest),
            refs: collect_refs(rest),
            anchor: None,
        });
    }
    // A `> blockquote` or plain prose line is a floating comment.
    let body = stripped
        .strip_prefix("> ")
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

/// Build a nested tree from a run of non-heading lines, nesting each item under
/// the nearest preceding item with a smaller indent (a stack keyed by indent).
fn build_items(items: &[&Line]) -> Vec<MethodNode> {
    let mut nodes: Vec<MethodNode> = items
        .iter()
        .map(|l| {
            let mut n = node_from_line(l);
            n.indent = 0; // depth carries the hierarchy now
            n.anchor = None;
            n
        })
        .collect();
    // parent[i] = index of i's parent, or None for a root.
    let mut parent: Vec<Option<usize>> = vec![None; nodes.len()];
    let mut stack: Vec<usize> = Vec::new(); // indices with strictly increasing indent
    for i in 0..items.len() {
        let ind = items[i].indent;
        while let Some(&top) = stack.last() {
            if items[top].indent >= ind {
                stack.pop();
            } else {
                break;
            }
        }
        parent[i] = stack.last().copied();
        stack.push(i);
    }
    // Assemble children lists in order (reverse walk + insert(0, ..)).
    let mut opt: Vec<Option<MethodNode>> = nodes.drain(..).map(Some).collect();
    let mut roots: Vec<MethodNode> = Vec::new();
    for i in (0..opt.len()).rev() {
        let node = opt[i].take().unwrap();
        match parent[i] {
            Some(p) => opt[p].as_mut().unwrap().children.insert(0, node),
            None => roots.insert(0, node),
        }
    }
    roots
}

fn build_block(lines: &[Line], pos: &mut usize, parent_level: u8) -> Vec<MethodNode> {
    let mut out = Vec::new();
    let mut body: Vec<&Line> = Vec::new();
    while *pos < lines.len() {
        match lines[*pos].heading {
            Some(hl) => {
                if hl <= parent_level {
                    break;
                }
                // Flush any buffered body items before this sub-heading.
                if !body.is_empty() {
                    out.extend(build_items(&body));
                    body.clear();
                }
                let mut node = node_from_line(&lines[*pos]);
                node.indent = 0;
                *pos += 1;
                node.children = build_block(lines, pos, hl);
                out.push(node);
            }
            None => {
                body.push(&lines[*pos]);
                *pos += 1;
            }
        }
    }
    if !body.is_empty() {
        out.extend(build_items(&body));
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

/// The top-level engagement sections that become the Methodology sub-tabs: any
/// top-level heading that is numbered (`# 4.` / `# XII.`) **or** contains at
/// least one checklist item. A document title / Table of Contents has no
/// checkboxes and isn't numbered, so it's skipped.
pub fn sections(tree: &[MethodNode]) -> Vec<&MethodNode> {
    tree.iter()
        .filter(|n| {
            n.is_heading()
                && (section_re().is_match(n.title.trim_start()) || n.leaf_counts().1 > 0)
        })
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

    fn find<'a>(nodes: &'a [MethodNode], title: &str) -> Option<&'a MethodNode> {
        for n in nodes {
            if n.title == title {
                return Some(n);
            }
            if let Some(f) = find(&n.children, title) {
                return Some(f);
            }
        }
        None
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

        // Indented items nest into a real tree.
        let rfi = find(&tree, "RFI").expect("RFI item");
        assert!(
            rfi.children.iter().any(|c| c.title.starts_with("RCE via malicious")),
            "RFI should own the RCE child"
        );
        assert!(
            rfi.children.iter().any(|c| c.title == "Enum localhost ports"),
            "RFI should own 'Enum localhost ports'"
        );
        let rce = find(&rfi.children, "RCE via malicious script we host (include function must have execute)")
            .or_else(|| rfi.children.iter().find(|c| c.title.starts_with("RCE via malicious")))
            .expect("RCE node");
        for leaf in ["HTTP", "FTP", "SMB"] {
            assert!(rce.children.iter().any(|c| c.title == leaf), "RCE should own {leaf}");
        }
        assert!(!rfi.is_leaf_check(), "RFI is a parent check");
        assert!(rce.children.iter().all(|c| c.is_leaf_check()), "HTTP/FTP/SMB are leaves");
    }

    #[test]
    fn parses_ad_doc() {
        let md = std::fs::read_to_string("JSONs/methodology/ad.md").unwrap();
        let tree = parse(&md);
        let secs = sections(&tree);
        eprintln!("ad: sections={}", secs.len());
        // Roman-numeral sections I..VIII.
        assert_eq!(secs.len(), 8, "expected 8 AD sections");
        assert_eq!(
            secs[0].title.trim_start_matches("I. "),
            "Recon & Uncredentialed Enumeration"
        );
        // AD CS is its own section and nests ESC nodes under `##` cards.
        let adcs = secs[3];
        assert_eq!(adcs.title.trim_start_matches("IV. "), "AD CS");
        let templates = adcs
            .children
            .iter()
            .find(|c| c.title == "Template Misconfigurations")
            .expect("Template Misconfigurations card");
        for esc in ["ESC1", "ESC9", "ESC13", "ESC14", "ESC15"] {
            assert!(
                templates.children.iter().any(|c| c.title == esc),
                "{esc} should nest under Template Misconfigurations"
            );
        }
    }

    #[test]
    fn parses_azure_and_external_docs() {
        let az = parse(&std::fs::read_to_string("JSONs/methodology/azure.md").unwrap());
        let az_secs = sections(&az);
        let names: Vec<&str> = az_secs.iter().map(|s| s.title.as_str()).collect();
        eprintln!("azure sections: {names:?}");
        // The numbered I..IV sections are tabs; empty non-numbered headers (WKL,
        // and "Authenticated Enum" once its checks are removed) are skipped.
        assert!(az_secs.len() >= 4, "expected the numbered Azure sections");
        assert!(names.iter().any(|n| n.starts_with("I. ")), "section I present");
        assert!(!names.contains(&"WKL"), "empty non-numbered WKL is skipped");

        let ex = parse(&std::fs::read_to_string("JSONs/methodology/external.md").unwrap());
        let ex_secs = sections(&ex);
        eprintln!("external sections={}", ex_secs.len());
        // User-edited doc — assert it parses into several numbered sections rather
        // than a brittle exact count.
        assert!(ex_secs.len() >= 4, "expected several external sections, got {}", ex_secs.len());
        assert!(
            ex_secs.iter().all(|s| section_re().is_match(s.title.trim_start())),
            "all external sections should be numbered"
        );
    }

    #[test]
    fn parses_privesc_docs() {
        // Linux and Windows priv-esc docs: a title line (skipped) plus
        // roman-numeral sections that become the sub-tabs, with no leftover
        // Notion `[text](url)` link markup in any title.
        for (file, want) in [("linux.md", 5), ("windows.md", 8)] {
            let md = std::fs::read_to_string(format!("JSONs/methodology/{file}")).unwrap();
            let tree = parse(&md);
            let secs = sections(&tree);
            let names: Vec<&str> = secs.iter().map(|s| s.title.as_str()).collect();
            eprintln!("{file} sections: {names:?}");
            assert_eq!(secs.len(), want, "expected {want} sections in {file}");
            assert!(
                secs.iter().all(|s| section_re().is_match(s.title.trim_start())),
                "all {file} sections should be roman-numeral numbered"
            );
            assert!(
                names.iter().all(|n| !n.contains('[') && !n.contains("](")),
                "no leftover link markup in {file} titles"
            );
        }
    }
}



