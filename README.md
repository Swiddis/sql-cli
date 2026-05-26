# ppl

`ppl` is a CLI for PPL devs.

It primarily serves as a util to run PPL queries against localhost and make the results easy to inspect.

```
$ ppl --help
Run PPL queries on localhost:9200

Usage: ppl [OPTIONS] [FILE]

Arguments:
  [FILE]  Input file (if not provided, reads from stdin)

Options:
      --table    Pretty-print results as a table
      --compact  Compact JSON output (one object per line)
      --export   Output curl command instead of executing query
      --explain  Use explain endpoint for query plan
  -h, --help     Print help
```

## Explain Mode

The `--explain` flag uses the `/_plugins/_ppl/_explain` endpoint to get query plans with syntax-highlighted Calcite logical and physical plans:

```bash
echo "source=accounts | where age > 18" | ppl --explain
```

The output highlights:
- Function names (cyan)
- Variables like $0, $1 (yellow)
- String literals (green)
- Numbers (blue)
- Attributes (magenta)
- Keywords null/true/false (bright blue)

Physical plan `sourceBuilder=` JSON is automatically pretty-printed and indented.

