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


## Pushdown Script Decoding

Calcite pushdown scripts (`"lang": "opensearch_compounded_script"`) are base64 blobs in the DSL.
Both `--explain` and `--decode` replace each one's `source` with a readable block, in place:

```
"source": $SCRIPT_1 {
  # reads: doc[email.keyword]
  $t0 = doc[email.keyword] IS NULL
  $t1 = doc[email.keyword] = ""
  $t2 = $t0 OR $t1
  $t3 = REGEXP_REPLACE(doc[email.keyword], "[a-zA-Z0-9]+", "<*>")  # regex
  $t4 = CASE($t2 => "", else $t3)
  return $t4
},
```

- One `$tN` line per call; fields and literals inline. Identical subtrees share one line, so repeated work shows up as one `$tN` used twice.
- `doc[f]` reads doc values, `_source[f]` parses `_source` per hit, `flat[f]` is a flat_object leaf. These come from the script's `SOURCES`/`DIGESTS` params, which are then dropped from the output.
- Annotations: result type where the plan records one (UDFs), `expensive UDF` (the sql repo's `ScriptDetector` list), `regex`.
- Lines are in evaluation-dependency order, not strict execution order: `CASE`/`AND`/`OR` operands may be lazily evaluated even though they're hoisted onto their own lines.
- Not decodable: `langType: v2` scripts (Java-serialized objects) and the 3.2–3.3 map-format scripts. These print `# could not decode` plus the raw source.

Use `--decode` offline (no request sent) for whatever oncall hands you. It auto-detects:

```bash
ppl --decode explain-response.json   # full _explain response
ppl --decode slowlog-source.json     # DSL, e.g. a slow log's source
pbpaste | ppl --decode               # pasted plan text with sourceBuilder=...
echo 'rO0ABXQCHHsK...' | ppl --decode  # bare base64 blob(s), whitespace-separated
```

- Bare blobs have no params, so fields and literals show as `?N`. Paste the surrounding `params` too (or the whole plan line) to resolve them.
- If the `sourceBuilder` JSON won't parse (a truncated log, or the test wildcard `"utcTimestamp":*`), each blob is replaced in place with `$SCRIPT_N` and the decoded blocks are listed under the line. Params are scraped from the raw text where possible.
- Scripts are numbered across the whole plan, so a join's two scans don't both print `$SCRIPT_1`.
