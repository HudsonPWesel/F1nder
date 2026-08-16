# f1nder


Searching cli tool

## Bulk import

Mass-add commands from a file of cmd-maker template blocks (`--- TITLE ---` …
`--- COMMANDS ---`):

```
f1nder --import commands.md
```

Each block is routed to `JSONs/cmds/<SOURCE-FILE>-CMDs.json`, gets a fresh id,
and duplicates (same title + command in the target file) are skipped, so the
command is safe to re-run. Run `f1nder --help` for usage.
