use colored::Colorize;
use logos::Logos;
use serde_json::Value;

#[derive(Logos, Debug, PartialEq)]
enum CalciteToken {
    // Line starting with = (comment/header)
    #[regex(r"=.*?", priority = 10)]
    Comment,

    // Null, true, false keywords
    #[regex(r"null|true|false", priority = 8)]
    Keyword,

    // Function names (capitalized words)
    #[regex(r"[A-Z][a-z]\w+", priority = 7)]
    Function,

    // Variables like $0, $1, $foo
    #[regex(r"\$[\d\w]+", priority = 7)]
    Variable,

    // Strings in quotes
    #[regex(r#""[^"]*""#, priority = 7)]
    String,

    // Attributes (word followed by =)
    #[regex(r"@?[a-zA-Z_][\w\.#\d]*=", priority = 6)]
    Attribute,

    // Numbers
    #[regex(r"\d+", priority = 5)]
    Number,

    // Words with hyphens
    #[regex(r"[\w\-]+", priority = 3)]
    Text,

    // Whitespace (spaces and tabs) - preserve for indentation
    #[regex(r"[ \t]+", priority = 2)]
    Whitespace,

    // Single non-word characters
    #[regex(r"[^\w\s]", priority = 2)]
    Punctuation,

    // Newlines
    #[regex(r"\n", priority = 1)]
    Newline,
}

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
    let mut result = String::new();
    let mut lexer = CalciteToken::lexer(text);

    while let Some(token_result) = lexer.next() {
        let span = lexer.slice();

        match token_result {
            Ok(CalciteToken::Comment) => {
                result.push_str(&span.bright_black().to_string());
            }
            Ok(CalciteToken::Function) => {
                result.push_str(&span.cyan().to_string());
            }
            Ok(CalciteToken::Variable) => {
                result.push_str(&span.yellow().to_string());
            }
            Ok(CalciteToken::String) => {
                result.push_str(&span.green().to_string());
            }
            Ok(CalciteToken::Number) => {
                result.push_str(&span.blue().to_string());
            }
            Ok(CalciteToken::Attribute) => {
                result.push_str(&span.magenta().to_string());
            }
            Ok(CalciteToken::Keyword) => {
                result.push_str(&span.bright_blue().to_string());
            }
            Ok(CalciteToken::Whitespace) => {
                result.push_str(span);
            }
            Ok(CalciteToken::Newline) => {
                result.push('\n');
            }
            Ok(CalciteToken::Text) | Ok(CalciteToken::Punctuation) => {
                result.push_str(span);
            }
            Err(_) => {
                result.push_str(span);
            }
        }
    }

    result
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
}
