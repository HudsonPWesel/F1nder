Notes on disk  →  Parse into Vec<Entry>  →  Fuzzy match against query  →  Render results
                  { cmd, attack, desc }      scored & sorted                in TUI list

The structure means your fuzzy search can match against any field — search "kerberoast" and it hits topic/section, search "ntlmrelayx" and it hits cmd, search "dump" and it hits desc. When you display results in the TUI you can show the topic/section as breadcrumbs above the command so you know where it came from.

ratatui — the standard TUI framework (renders the search box, results list, preview pane). This is what gives you that nvim-like feel.
crossterm — handles raw terminal input (keypresses, escape, etc.). Ratatui sits on top of this.
nucleo or fuzzy-matcher — fuzzy search scoring (like fzf). nucleo is what the Helix editor uses and is very fast.
serde + serde_yaml/toml — if you want structured note files instead of raw markdown parsing.

Each Entry is just a struct with fields like cmd: String, attack: String, desc: String, and maybe source_file: String.
How the TUI Loop Works
This is the core pattern for every ratatui app:

Draw the UI (search bar at top, results list below, maybe a preview pane)
Poll for keyboard input (crossterm)
Update app state based on input (typing updates query → re-runs fuzzy search, arrow keys move selection, Enter copies/opens, Esc quits)
Repeat

All your mutable state lives in one App struct — the query string, the filtered results, which result is highlighted, etc. This actually plays nicely with the borrow checker because you pass &mut app around in one place rather than scattering state everywhere.
Note Format Suggestion
Keep it simple. Markdown tables work, or even a flat TOML/YAML per category: