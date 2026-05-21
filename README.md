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
  -h, --help     Print help
```

