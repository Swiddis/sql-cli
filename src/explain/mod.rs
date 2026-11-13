use serde_json::Value;

mod parser;
mod script;
use parser::CalciteHighlighter;

const COMPOUNDED_LANG: &str = "opensearch_compounded_script";

pub fn format_calcite_explain(error_json: &Value) -> Option<String> {
    let calcite = error_json.get("calcite")?;
    let logical = calcite.get("logical")?.as_str()?;
    let physical = calcite.get("physical")?.as_str()?;

    let mut report = String::new();
    report.push_str("= Calcite Plan =\n");
    report.push_str("== Logical ==\n");
    report.push_str(logical);
    report.push_str("\n\n");
    report.push_str("== Physical ==\n");

    report.push_str(&process_physical(physical));

    Some(highlight_calcite_plan(&report))
}

/// Offline decode for `--decode`: no request is sent. Accepts, by shape:
/// - an `_explain` response (`{"calcite": {"logical", "physical"}}`)
/// - DSL / any JSON containing compounded scripts (e.g. a slow log `source`)
/// - a `{"langType": ..., "script": ...}` wrapper
/// - pasted physical-plan text containing `sourceBuilder=`
/// - one or more bare base64 script blobs (whitespace-separated); params are unknown, so
///   fields and literals show as `?N`
pub fn decode_input(input: &str) -> anyhow::Result<String> {
    let input = input.trim();
    if input.starts_with('{')
        && let Ok(v) = serde_json::from_str::<Value>(input)
    {
        if let Some(out) = format_calcite_explain(&v) {
            return Ok(out);
        }
        if v.get("langType").is_some() {
            let lines = script::decode(input, None)?;
            return Ok(highlight_calcite_plan(&script_block(1, &lines)));
        }
        return Ok(format_dsl(&v));
    }
    if input.contains("sourceBuilder=") {
        return Ok(highlight_calcite_plan(&process_physical(input)));
    }

    // Bare base64. Tolerate copy-paste debris (quotes, `\"`, `"script":`) by taking the longest
    // base64 run in each whitespace-separated token.
    let is_b64 = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=');
    let blobs: Vec<&str> = input
        .split_whitespace()
        .filter_map(|t| t.split(|c| !is_b64(c)).max_by_key(|s| s.len()))
        .filter(|t| t.len() >= 8)
        .collect();
    if blobs.is_empty() {
        anyhow::bail!("nothing to decode");
    }
    let mut out = vec![
        "# params not provided: ?N are fields or literals (SOURCES/DIGESTS live next to the script)"
            .to_string(),
    ];
    for (i, blob) in blobs.iter().enumerate() {
        let lines = script::decode_script(blob, None)
            .unwrap_or_else(|e| vec![format!("# could not decode: {e:#}")]);
        out.push(script_block(i + 1, &lines));
    }
    Ok(highlight_calcite_plan(&out.join("\n")))
}

fn script_block(n: usize, lines: &[String]) -> String {
    let body: Vec<String> = lines.iter().map(|l| format!("  {l}")).collect();
    format!("$SCRIPT_{n} {{\n{}\n}}", body.join("\n"))
}

/// Prettify every `sourceBuilder=` in a physical plan, numbering scripts across the whole plan
/// (a join has one scan per side).
fn process_physical(physical: &str) -> String {
    let mut seen = 0;
    physical
        .lines()
        .map(|l| process_physical_calcite_line(l, &mut seen))
        .collect::<Vec<_>>()
        .join("\n")
}

fn process_physical_calcite_line(line: &str, seen: &mut usize) -> String {
    const SOURCE_BUILDER: &str = "sourceBuilder=";

    if let Some(start_idx) = line.find(SOURCE_BUILDER) {
        let json_start = start_idx + SOURCE_BUILDER.len();
        let rest = &line[json_start..];

        // Parse exactly one JSON value off the front (brace counting breaks on braces in strings)
        let mut stream = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
        if let Some(Ok(parsed)) = stream.next() {
            let end_idx = stream.byte_offset();
            let pretty = pretty_dsl_numbered(&parsed, seen);

            // Get leading whitespace from original line
            let leading_space = line.len() - line.trim_start().len();
            let indent = " ".repeat(leading_space);

            // Indent each line of the pretty JSON
            let indented_json = pretty
                .lines()
                .enumerate()
                .map(|(i, l)| {
                    if i == 0 {
                        l.to_string()
                    } else {
                        format!("{}{}", indent, l)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
                .trim_start_matches("| ")
                .to_string();

            return format!(
                "{}{}{}",
                &line[..json_start],
                indented_json,
                &rest[end_idx..]
            );
        }
    }

    if line.contains(JAVA_STREAM_B64) {
        return fallback_decode_line(line, seen);
    }

    line.to_string()
}

/// Base64 of the Java serialization magic `AC ED 00 05`, the start of every encoded script.
const JAVA_STREAM_B64: &str = "rO0AB";

/// For lines whose JSON won't parse (truncated logs, test wildcards like `"utcTimestamp":*`):
/// replace each base64 script with `$SCRIPT_N` in place and list decoded blocks under the line.
fn fallback_decode_line(line: &str, seen: &mut usize) -> String {
    let is_b64 = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=');
    // (start, end) of each blob
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(off) = line[from..].find(JAVA_STREAM_B64) {
        let start = from + off;
        let end = line[start..]
            .find(|c: char| !is_b64(c))
            .map_or(line.len(), |n| start + n);
        spans.push((start, end));
        from = end;
    }

    let indent = &line[..line.len() - line.trim_start().len()];
    let mut head = String::new();
    let mut blocks = Vec::new();
    let mut last = 0;
    for (i, &(start, end)) in spans.iter().enumerate() {
        *seen += 1;
        head.push_str(&line[last..start]);
        head.push_str(&format!("$SCRIPT_{seen}"));
        last = end;
        // params sit after the source string, before the next script
        let next = spans.get(i + 1).map_or(line.len(), |s| s.0);
        let params = scrape_params(&line[end..next]);
        let lines = script::decode_script(&line[start..end], params.as_ref())
            .unwrap_or_else(|e| vec![format!("# could not decode: {e:#}")]);
        let block = script_block(*seen, &lines);
        blocks.push(
            block
                .lines()
                .map(|l| format!("{indent}  {l}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    head.push_str(&line[last..]);
    format!(
        "{head}\n{indent}  # sourceBuilder JSON didn't parse; scripts decoded below\n{}",
        blocks.join("\n")
    )
}

/// Best-effort `"SOURCES":[...]` / `"DIGESTS":[...]` from unparseable text.
fn scrape_params(text: &str) -> Option<serde_json::Map<String, Value>> {
    let text = text.replace("\\\"", "\"");
    let array_after = |key: &str| -> Option<Value> {
        let start = text.find(&format!("\"{key}\":["))? + key.len() + 3;
        let mut stream = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        stream.next()?.ok()
    };
    let mut map = serde_json::Map::new();
    map.insert("SOURCES".into(), array_after("SOURCES")?);
    map.insert("DIGESTS".into(), array_after("DIGESTS")?);
    Some(map)
}

/// Pretty-print OpenSearch DSL with every Calcite pushdown script decoded in place:
/// `"source": "<b64 wrapper>"` becomes `"source": $SCRIPT_N { ...three-address code... }`.
pub fn pretty_dsl(dsl: &Value) -> String {
    pretty_dsl_numbered(dsl, &mut 0)
}

/// [`pretty_dsl`], numbering scripts after the `seen` already emitted.
fn pretty_dsl_numbered(dsl: &Value, seen: &mut usize) -> String {
    let base = *seen;
    let mut dsl = dsl.clone();
    let mut blocks = Vec::new();
    replace_scripts(&mut dsl, &mut blocks, base);
    *seen += blocks.len();
    let pretty = serde_json::to_string_pretty(&dsl).unwrap_or_default();
    splice_scripts(&pretty, &blocks, base)
}

/// Highlighted variant of [`pretty_dsl`] for terminal output.
pub fn format_dsl(dsl: &Value) -> String {
    highlight_calcite_plan(&pretty_dsl(dsl))
}

/// Swap each compounded script's `source` for a `$SCRIPT_N` placeholder, collecting decoded lines.
fn replace_scripts(v: &mut Value, blocks: &mut Vec<Vec<String>>, base: usize) {
    match v {
        Value::Object(map) => {
            let source = match (map.get("lang"), map.get("source")) {
                (Some(Value::String(lang)), Some(Value::String(src)))
                    if lang == COMPOUNDED_LANG =>
                {
                    Some(src.clone())
                }
                _ => None,
            };
            if let Some(src) = source {
                let lines = match script::decode(&src, map.get("params").and_then(Value::as_object))
                {
                    Ok(lines) => {
                        // Resolved into the decoded lines; drop the noise, keep any other params.
                        if let Some(Value::Object(params)) = map.get_mut("params") {
                            params.remove("SOURCES");
                            params.remove("DIGESTS");
                        }
                        lines
                    }
                    Err(e) => vec![
                        format!("# could not decode: {e:#}"),
                        format!("# raw: {src}"),
                    ],
                };
                blocks.push(lines);
                map.insert(
                    "source".into(),
                    Value::String(format!("$SCRIPT_{}", base + blocks.len())),
                );
            }
            for child in map.values_mut() {
                replace_scripts(child, blocks, base);
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|x| replace_scripts(x, blocks, base)),
        _ => {}
    }
}

/// Replace each `"$SCRIPT_N"` string in pretty JSON with an indented block of decoded lines.
fn splice_scripts(pretty: &str, blocks: &[Vec<String>], base: usize) -> String {
    let mut out = Vec::new();
    for line in pretty.lines() {
        let found = line.find("\"$SCRIPT_").and_then(|start| {
            let digits_start = start + "\"$SCRIPT_".len();
            let digits_len = line[digits_start..].find('"')?;
            let n: usize = line[digits_start..digits_start + digits_len].parse().ok()?;
            Some((
                start,
                digits_start + digits_len + 1,
                blocks.get(n.checked_sub(base + 1)?)?,
                n,
            ))
        });
        let Some((start, end, block, n)) = found else {
            out.push(line.to_string());
            continue;
        };
        let indent = &line[..line.len() - line.trim_start().len()];
        out.push(format!("{}$SCRIPT_{n} {{", &line[..start]));
        out.extend(block.iter().map(|l| format!("{indent}  {l}")));
        out.push(format!("{indent}}}{}", &line[end..]));
    }
    out.join("\n")
}

fn highlight_calcite_plan(text: &str) -> String {
    let mut highlighter = CalciteHighlighter::new(text);
    highlighter.highlight()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_builder_json_extraction() {
        let line = r#"  OpenSearchIndexScan(indexName=test, sourceBuilder={"from":0,"size":150}, done=false)"#;
        let processed = process_physical(line);
        assert!(processed.contains("\"from\": 0"));
        assert!(processed.contains("\"size\": 150"));
    }

    #[test]
    fn test_scripts_decoded_inline_in_source_builder() {
        // Real physical-plan line from sql integ-test explain_script_push_on_text.yaml
        let line = format!(
            "    {}",
            include_str!("testdata/script_push_on_text.txt").trim()
        );
        let processed = process_physical(&line);

        assert!(
            !processed.contains("rO0AB"),
            "base64 should be gone:\n{processed}"
        );
        assert!(!processed.contains("DIGESTS"));
        assert!(
            processed.contains("\"utcTimestamp\": 0"),
            "other params kept"
        );
        // query.script.script block, spliced at the JSON's own indentation (+4 line indent)
        let expected_filter = [
            "            \"source\": $SCRIPT_1 {",
            "              # reads: _source[address]  (_source reads parse the document per hit)",
            "              $t0 = CHAR_LENGTH(_source[address])",
            "              $t1 = 0 < $t0",
            "              return $t1",
            "            },",
        ]
        .join("\n");
        assert!(processed.contains(&expected_filter), "got:\n{processed}");
        assert!(
            processed.contains("$SCRIPT_2 {"),
            "agg terms script decoded too"
        );
        // trailing text after the JSON survives
        assert!(processed.contains("requestedTotalSize=10000"));
    }

    const PLAN_LINE: &str = include_str!("testdata/script_push_on_text.txt");

    fn plain(s: String) -> String {
        // strip ANSI color codes
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            match (esc, c) {
                (false, '\x1b') => esc = true,
                (true, 'm') => esc = false,
                (true, _) => {}
                (false, c) => out.push(c),
            }
        }
        out
    }

    /// First base64 script blob in the fixture plan line.
    fn fixture_blob() -> &'static str {
        let start = PLAN_LINE.find("rO0AB").unwrap();
        let len = PLAN_LINE[start..].find('\\').unwrap();
        &PLAN_LINE[start..start + len]
    }

    #[test]
    fn test_decode_input_bare_base64() {
        let out = plain(decode_input(&format!("\n{}\n", fixture_blob())).unwrap());
        assert!(out.starts_with("# params not provided"), "{out}");
        assert!(
            out.contains("$SCRIPT_1 {\n  $t0 = CHAR_LENGTH(?1)\n  $t1 = ?0 < $t0\n  return $t1\n}"),
            "{out}"
        );
    }

    #[test]
    fn test_decode_input_tolerates_paste_debris_and_multiple_blobs() {
        let b = fixture_blob();
        let input = format!("\"script\":\\\"{b}\\\"}}\n{b},\nnot-base64");
        let out = plain(decode_input(&input).unwrap());
        assert!(out.contains("$SCRIPT_1 {"), "{out}");
        assert!(out.contains("$SCRIPT_2 {"), "{out}");
        // short junk tokens are ignored rather than reported as failures
        assert!(!out.contains("$SCRIPT_3"), "{out}");
        assert!(!out.contains("could not decode"), "{out}");
    }

    #[test]
    fn test_decode_input_bad_base64_reports_inline() {
        let out = plain(decode_input("QUJDREVGR0hJSktM").unwrap());
        assert!(
            out.contains("# could not decode: not a Java serialization stream"),
            "{out}"
        );
    }

    #[test]
    fn test_decode_input_wrapper() {
        let wrapper =
            serde_json::json!({"langType": "calcite", "script": fixture_blob()}).to_string();
        let out = plain(decode_input(&wrapper).unwrap());
        assert!(out.contains("return $t1"), "{out}");
    }

    #[test]
    fn test_decode_input_explain_response() {
        let physical = PLAN_LINE.trim();
        let resp = serde_json::json!({"calcite": {"logical": "LogicalSystemLimit(fetch=[10000])", "physical": physical}});
        let out = plain(decode_input(&resp.to_string()).unwrap());
        assert!(out.contains("== Physical =="), "{out}");
        assert!(out.contains("$t0 = CHAR_LENGTH(_source[address])"), "{out}");
    }

    #[test]
    fn test_decode_input_pasted_plan_text_and_dsl() {
        let out = plain(decode_input(PLAN_LINE).unwrap());
        assert!(out.contains("$SCRIPT_2 {"), "{out}");
        let dsl = serde_json::json!({"query": {"script": {"script": {
            "source": serde_json::json!({"langType": "calcite", "script": fixture_blob()}).to_string(),
            "lang": COMPOUNDED_LANG,
            "params": {"SOURCES": [2, 1], "DIGESTS": [0, "address"]},
        }}}});
        let out = plain(decode_input(&dsl.to_string()).unwrap());
        assert!(out.contains("$t1 = 0 < $t0"), "{out}");
    }

    #[test]
    fn test_unparseable_source_builder_falls_back_to_inline_refs() {
        // IT fixtures use `"utcTimestamp":*`; truncated logs break the JSON the same way
        let line = PLAN_LINE
            .trim()
            .replace("\"utcTimestamp\": 0", "\"utcTimestamp\":*");
        assert_ne!(
            line,
            PLAN_LINE.trim(),
            "fixture should contain the timestamp param"
        );
        let out = plain(decode_input(&line).unwrap());
        assert!(!out.contains("rO0AB"), "{out}");
        assert!(
            out.contains("\\\"script\\\":\\\"$SCRIPT_1\\\""),
            "blob replaced in place:\n{out}"
        );
        assert!(out.contains("# sourceBuilder JSON didn't parse"), "{out}");
        // params scraped from the raw text, so fields still resolve
        assert!(
            out.contains("  $SCRIPT_1 {\n    # reads: _source[address]"),
            "{out}"
        );
        assert!(out.contains("    $t1 = 0 < $t0"), "{out}");
        assert!(out.contains("  $SCRIPT_2 {"), "{out}");
    }

    #[test]
    fn test_truncated_blob_reports_inline() {
        let line = PLAN_LINE.trim();
        let cut = &line[..line.find("rO0AB").unwrap() + 40];
        let out = plain(decode_input(cut).unwrap());
        assert!(out.contains("$SCRIPT_1 {"), "{out}");
        assert!(out.contains("# could not decode"), "{out}");
    }

    #[test]
    fn test_script_numbering_spans_plan_lines() {
        // e.g. a join: two scans, each with scripts
        let plan = format!("{}\n  {}", PLAN_LINE.trim(), PLAN_LINE.trim());
        let out = plain(process_physical(&plan));
        for n in 1..=4 {
            assert_eq!(out.matches(&format!("$SCRIPT_{n} {{")).count(), 1, "{out}");
        }
    }

    #[test]
    fn test_undecodable_script_keeps_raw() {
        let dsl = serde_json::json!({"script": {
            "source": "{\"langType\":\"v2\",\"script\":\"rO0AB\"}",
            "lang": COMPOUNDED_LANG,
        }});
        let out = pretty_dsl(&dsl);
        assert!(out.contains("# could not decode: langType 'v2'"), "{out}");
        assert!(out.contains("# raw: {\"langType\":\"v2\""), "{out}");
    }

    #[test]
    fn test_source_builder_with_braces_in_strings() {
        let line = r#"X(sourceBuilder={"q":"a}b{"}, done=false)"#;
        let processed = process_physical(line);
        assert!(processed.contains("\"q\": \"a}b{\""), "{processed}");
        assert!(processed.ends_with(", done=false)"));
    }

    #[test]
    fn test_line_without_source_builder() {
        let line = "ProjectExec(fields=[a, b, c])";
        let processed = process_physical(line);
        assert_eq!(processed, line);
    }

    #[test]
    fn test_complex_real_world_case() {
        // Test with actual complex patterns from integration tests
        let sample = r#"= Calcite Plan =
== Logical ==
LogicalSystemLimit(fetch=[10000], type=[QUERY_SIZE_LIMIT])
  LogicalProject(avg_age=[$1], age_range=[$0])
    LogicalAggregate(group=[{0}], avg_age=[AVG($1)])
      LogicalProject(age_range=[CASE(<($10, 30), 'u30':VARCHAR, SEARCH($10, Sarg[[30..40]]), 'u40':VARCHAR, 'u100':VARCHAR)], age=[$10])
        CalciteLogicalIndexScan(table=[[OpenSearch, test_bank]])
"#;

        let highlighted = highlight_calcite_plan(sample);

        // Verify key patterns are present
        assert!(highlighted.contains("LogicalSystemLimit"));
        assert!(highlighted.contains("CASE"));
        assert!(highlighted.contains("$10"));
        assert!(highlighted.contains("u30")); // String content (quotes added separately)
        assert!(highlighted.contains("VARCHAR"));

        // Should handle nested function calls
        assert!(highlighted.contains("AVG"));
        assert!(highlighted.contains("SEARCH"));
    }

    #[test]
    fn test_highlighting_visual() {
        // This test is for manual verification - run it to see colored output
        let sample = r#"= Calcite Plan =
== Logical ==
LogicalSystemLimit(fetch=[10000], type=[QUERY_SIZE_LIMIT])
  LogicalProject(count()=[$1], c1=[$1], gender=[$0], label='hello')
    LogicalAggregate(group=[{0}], count()=[COUNT()])
      LogicalProject(gender=[$4], name="world")
        CalciteLogicalIndexScan(table=[[OpenSearch, test_index]])

== Physical ==
EnumerableCalc(expr#0..1=[{inputs}], count()=[$t1], c1=[$t1], gender=[$t0])
  CalciteEnumerableIndexScan(table=[[OpenSearch, test_index]],
    PushDownContext=[[AGGREGATION->rel#:LogicalAggregate.NONE.[](input=RelSubset#,group={0},count()=COUNT()),
    LIMIT->10000],
    OpenSearchRequestBuilder(sourceBuilder={"from":0,"size":0}, requestedTotalSize=10000, pageSize=null, startFrom=0)])
"#;

        let highlighted = highlight_calcite_plan(sample);

        // Print to stdout for visual inspection
        println!("\n\n===== HIGHLIGHTED OUTPUT =====");
        println!("{}", highlighted);
        println!("===== END =====\n\n");

        // Basic sanity checks - just verify it doesn't crash and produces output
        assert!(!highlighted.is_empty());
        assert!(highlighted.contains("LogicalSystemLimit"));
    }
}
