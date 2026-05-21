use anyhow::{Context, Result};
use clap::Parser;
use colored_json::{ColoredFormatter, CompactFormatter};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, IsTerminal, Read};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "ppl")]
#[command(about = "Run PPL queries on localhost:9200", long_about = None)]
struct Cli {
    /// Pretty-print results as a table
    #[arg(long)]
    table: bool,

    /// Compact JSON output (one object per line)
    #[arg(long)]
    compact: bool,

    /// Output curl command instead of executing query
    #[arg(long)]
    export: bool,

    /// Input file (if not provided, reads from stdin)
    #[arg(value_name = "FILE")]
    file: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PplResponse {
    schema: Vec<SchemaColumn>,
    datarows: Vec<Vec<Value>>,
}

#[derive(Debug, Deserialize)]
struct SchemaColumn {
    name: String,
}

#[derive(Debug, Serialize)]
struct QueryRequest {
    query: String,
}

fn format_jdbc_to_json(response: &PplResponse) -> Vec<Value> {
    let mut results = Vec::new();

    for row in &response.datarows {
        let mut obj = serde_json::Map::new();
        for (value, column) in row.iter().zip(&response.schema) {
            obj.insert(column.name.clone(), value.clone());
        }
        results.push(Value::Object(obj));
    }

    results
}

fn format_table(response: &PplResponse) -> String {
    let mut output = String::new();

    // Header
    let headers: Vec<&str> = response.schema.iter().map(|c| c.name.as_str()).collect();
    output.push_str(&headers.join("\t"));
    output.push('\n');

    // Separator
    output.push_str(
        &headers
            .iter()
            .map(|h| "-".repeat(h.len()))
            .collect::<Vec<_>>()
            .join("\t"),
    );
    output.push('\n');

    // Rows
    for row in &response.datarows {
        let row_str: Vec<String> = row
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                Value::Null => "null".to_string(),
                other => other.to_string(),
            })
            .collect();
        output.push_str(&row_str.join("\t"));
        output.push('\n');
    }

    output
}

fn colorize_json(json_val: &Value) -> String {
    colored_json::to_colored_json_auto(&json_val).unwrap()
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Read query from file or stdin
    let query = if let Some(file_path) = &cli.file {
        std::fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path))?
    } else {
        let mut buffer = String::new();
        io::stdin()
            .read_to_string(&mut buffer)
            .context("Failed to read from stdin")?;
        buffer
    };

    let query = query.trim();
    if query.is_empty() {
        anyhow::bail!("No query provided");
    }

    let url = "http://localhost:9200/_plugins/_ppl";
    let body = QueryRequest {
        query: query.to_string(),
    };

    // Export mode: output curl command
    if cli.export {
        let json_body = serde_json::to_string(&body)?;
        println!(
            "curl -X POST '{}' -H 'Content-Type: application/json' -d '{}' --max-time 120",
            url, json_body
        );
        return Ok(());
    }

    // Execute query
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .context("Failed to send request")?;

    let status = response.status();
    let response_text = response.text()?;

    if !status.is_success() {
        eprintln!("Error: Request failed with status {}", status);
        if let Ok(json_val) = serde_json::from_str::<Value>(&response_text) {
            if io::stdout().is_terminal() {
                println!("{}", colorize_json(&json_val));
            } else {
                println!("{}", serde_json::to_string_pretty(&json_val)?);
            }
        } else {
            println!("{}", response_text);
        }
        std::process::exit(1);
    }

    let ppl_response: PplResponse =
        serde_json::from_str(&response_text).context("Failed to parse response as PPL result")?;

    // Output results
    if cli.table {
        println!("{}", format_table(&ppl_response));
    } else {
        let results = format_jdbc_to_json(&ppl_response);

        if cli.compact {
            let fmt = ColoredFormatter::new(CompactFormatter {});
            for obj in results {
                println!("{}", fmt.clone().to_colored_json_auto(&obj).unwrap());
            }
        } else {
            let array = Value::Array(results);
            println!("{}", colored_json::to_colored_json_auto(&array).unwrap());
        }
    }

    Ok(())
}
