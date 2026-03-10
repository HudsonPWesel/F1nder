"""
Universal Notion markdown-to-JSON parser.
Handles: Cmd|Desc tables, Command|Language tables, Resource|Description tables.
Builds heading breadcrumbs from h1/h2/h3.
Outputs clean JSON ready for F1nder.

Usage: python3 parse_notes.py <input_dir_or_file> <output.json>
"""
import re
import json
import sys
import os

def clean(text):
    """Strip markdown artifacts from text."""
    # Remove markdown links [text](url) -> text
    text = re.sub(r'\[([^\]]+)\]\([^)]+\)', r'\1', text)
    # Remove backticks
    text = text.replace('`', '')
    # Remove bold/italic
    text = text.replace('**', '').replace('*', '')
    # Clean non-breaking spaces
    text = text.replace('\u00a0', ' ')
    # Collapse whitespace
    text = re.sub(r'  +', ' ', text)
    return text.strip()

def split_table_row(row):
    """Split a markdown table row on | but ignore pipes inside backticks."""
    parts = []
    current = ""
    in_backtick = False

    for char in row:
        if char == '`':
            in_backtick = not in_backtick
            current += char
        elif char == '|' and not in_backtick:
            parts.append(current.strip())
            current = ""
        else:
            current += char

    if current.strip():
        parts.append(current.strip())

    # Remove empty strings from leading/trailing |
    return [p for p in parts if p]

def parse_md_file(filepath):
    """Parse a single markdown file into entries."""
    with open(filepath, 'r', encoding='utf-8') as f:
        content = f.read()

    lines = content.split('\n')
    entries = []
    h1 = ""
    h2 = ""
    h3 = ""
    context_lines = []
    in_table = False
    table_header_cols = None

    def heading():
        parts = [p for p in [h1, h2, h3] if p]
        return " > ".join(parts)

    i = 0
    while i < len(lines):
        stripped = lines[i].strip()

        # Track headings
        if re.match(r'^# [^#]', stripped):
            h1 = clean(stripped.lstrip('# '))
            h2 = ""
            h3 = ""
            context_lines = []
            in_table = False
            table_header_cols = None
        elif re.match(r'^## [^#]', stripped):
            h2 = clean(stripped.lstrip('# '))
            h3 = ""
            context_lines = []
            in_table = False
            table_header_cols = None
        elif re.match(r'^### ', stripped):
            h3 = clean(stripped.lstrip('# '))
            context_lines = []
            in_table = False
            table_header_cols = None

        # Detect table header row
        elif stripped.startswith('|') and not in_table:
            parts = split_table_row(stripped)
            if len(parts) >= 2:
                # Check if next line is separator
                if i + 1 < len(lines) and '---' in lines[i + 1]:
                    table_header_cols = [clean(p).lower() for p in parts]
                    in_table = True
                    i += 2  # skip header and separator
                    continue

        # Parse table data rows
        elif in_table and stripped.startswith('|'):
            # Handle multi-line table cells (lines that don't start with |)
            full_row = stripped
            while i + 1 < len(lines):
                next_line = lines[i + 1].strip()
                # If next line starts with | it's a new row
                if next_line.startswith('|') or next_line.startswith('#') or next_line == '':
                    break
                # Otherwise it's a continuation of this cell
                full_row += '\n' + next_line
                i += 1

            parts = split_table_row(full_row)

            if len(parts) >= 2:
                col1 = clean(parts[0])
                col2 = clean(parts[1])

                # Skip empty rows
                if not col1 and not col2:
                    i += 1
                    continue

                # Determine which is cmd and which is desc based on header
                cmd = col1
                desc = col2

                # Add context if available
                if context_lines:
                    extra = [c for c in context_lines[-2:] if c.lower() not in desc.lower()]
                    if extra:
                        desc = desc + '\n' + '\n'.join(extra)

                if cmd:  # only add if there's actually a command
                    entries.append({
                        "cmd": cmd,
                        "desc": desc,
                        "heading": heading(),
                    })
        elif in_table and not stripped.startswith('|'):
            # Exited the table
            in_table = False
            table_header_cols = None
            # This line might be context for next table
            c = clean(stripped)
            if c:
                context_lines.append(c)
        elif stripped and not stripped.startswith('|') and not stripped.startswith('- ['):
            c = clean(stripped)
            if c:
                context_lines.append(c)

        i += 1

    return entries

def parse_directory(path):
    """Parse all .md files in a directory."""
    all_entries = []
    md_files = []

    if os.path.isfile(path):
        md_files = [path]
    else:
        for root, dirs, files in os.walk(path):
            for f in files:
                if f.endswith('.md'):
                    md_files.append(os.path.join(root, f))

    for filepath in sorted(md_files):
        filename = os.path.basename(filepath)
        print(f"Parsing: {filename}")
        entries = parse_md_file(filepath)
        print(f"  -> {len(entries)} entries")
        all_entries.extend(entries)

    return all_entries

if __name__ == "__main__":
    if len(sys.argv) < 3:
        print("Usage: python3 parse_notes.py <input_dir_or_file> <output.json>")
        sys.exit(1)

    input_path = sys.argv[1]
    output_path = sys.argv[2]

    entries = parse_directory(input_path)

    output = {
        "source": os.path.basename(input_path),
        "entries": entries,
    }

    with open(output_path, 'w') as f:
        json.dump(output, f, indent=2)

    print(f"\nTotal: {len(entries)} entries -> {output_path}")