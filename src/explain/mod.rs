use serde_json::Value;

mod parser;
use parser::CalciteHighlighter;

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

    // Process physical plan lines for sourceBuilder JSON
    let processed_physical = physical
        .lines()
        .map(process_physical_calcite_line)
        .collect::<Vec<_>>()
        .join("\n");

    report.push_str(&processed_physical);

    Some(highlight_calcite_plan(&report))
}

fn process_physical_calcite_line(line: &str) -> String {
    const SOURCE_BUILDER: &str = "sourceBuilder=";

    if let Some(start_idx) = line.find(SOURCE_BUILDER) {
        let json_start = start_idx + SOURCE_BUILDER.len();
        let rest = &line[json_start..];

        // Find matching braces
        let mut stack = 0;
        let mut end_idx = 0;

        for (i, ch) in rest.chars().enumerate() {
            match ch {
                '{' => stack += 1,
                '}' => {
                    stack -= 1;
                    if stack == 0 {
                        end_idx = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }

        if end_idx > 0 {
            let json_str = &rest[..end_idx];

            // Try to parse and pretty-print the JSON
            if let Ok(parsed) = serde_json::from_str::<Value>(json_str)
                && let Ok(pretty) = serde_json::to_string_pretty(&parsed)
            {
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
    }

    line.to_string()
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
        let processed = process_physical_calcite_line(line);
        assert!(processed.contains("\"from\": 0"));
        assert!(processed.contains("\"size\": 150"));
    }

    #[test]
    fn test_line_without_source_builder() {
        let line = "ProjectExec(fields=[a, b, c])";
        let processed = process_physical_calcite_line(line);
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
