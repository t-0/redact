# sample

Files for exercising syntax highlighting, format checking and preview mode.
One folder per format. `sh/` also has a script with no extension, detected
from its shebang. Each has a valid file and an `invalid-*` file whose
single deliberate mistake should show up underlined in the editor, in the
problems pane and at the top of preview mode.

| folder | highlighting | checked by | preview |
|--------|--------------|------------|---------|
| `md`   | tree-sitter markdown + inline | not checked | formatted text |
| `json` | tree-sitter json  | `serde_json` | indented tree |
| `toml` | tree-sitter toml  | `toml`       | indented tree |
| `xml`  | tree-sitter xml   | `roxmltree`  | element tree |
| `ini`  | tree-sitter ini   | not checked  | not previewed |
| `sh`   | tree-sitter bash  | `shellcheck`, else `bash -n` | not previewed |

Open one and press `ctrl+k v` to switch between editing and preview.
