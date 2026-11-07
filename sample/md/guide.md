# Markdown sample

A paragraph with *emphasis*, **strong text**, `inline code`, a
[link](https://example.com/path) and ~~something struck out~~. It is long
enough that preview mode has to wrap it across more than one terminal line,
which is the whole point of having it here.

## Lists

- first item
- second item with `code` in it
  - a nested item
  - another nested item
- third item

1. numbered one
2. numbered two
3. numbered three

- [x] a finished task
- [ ] an unfinished task

### Quotes and rules

> A block quote, which preview mode indents behind a bar
> and dims slightly.

---

### Code

```rust
fn main() {
    let xs: Vec<i32> = (1..=5).collect();
    println!("{:?}", xs);
}
```

```python
def squares(n: int) -> list[int]:
    return [i * i for i in range(n)]
```

### A table

| format | checker    | notes              |
|--------|------------|--------------------|
| json   | serde_json | first error only   |
| toml   | toml       | span to line/col   |
| xml    | roxmltree  | rejects bad nesting |

Setext heading
==============

That heading used the underline form rather than hashes.
