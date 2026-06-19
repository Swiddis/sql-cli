use anyhow::{Context, Result};
use clap::Parser;
use colored_json::{ColoredFormatter, CompactFormatter};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, IsTerminal, Read};
use std::time::Duration;
use tabled::settings::Style;

mod explain;

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

    /// Use explain endpoint for query plan
    #[arg(long)]
    explain: bool,

    #[arg(long)]
    profile: bool,

    /// Limit results per query (default: 10 for glob, unlimited otherwise)
    #[arg(long)]
    limit: Option<usize>,

    /// Endpoint base URL (default: http://localhost:9200)
    #[arg(short, long)]
    endpoint: Option<String>,

    /// HTTP basic auth (format: username:password)
    #[arg(short, long)]
    auth: Option<String>,

    /// Input files or glob pattern (if not provided, reads from stdin)
    #[arg(value_name = "FILE")]
    files: Vec<String>,
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
    profile: bool,
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
    if response.datarows.is_empty() {
        return String::new();
    }

    // Convert each row to a Vec<String> for display
    let rows: Vec<Vec<String>> = response
        .datarows
        .iter()
        .map(|row| {
            row.iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    Value::Null => "null".to_string(),
                    other => other.to_string(),
                })
                .collect()
        })
        .collect();

    // Build table using tabled
    let mut builder = tabled::builder::Builder::default();

    // Add header
    builder.push_record(response.schema.iter().map(|c| &c.name));

    // Add rows
    for row in rows {
        builder.push_record(row);
    }

    builder.build().with(Style::modern()).to_string()
}

fn colorize_json(json_val: &Value) -> String {
    colored_json::to_colored_json_auto(&json_val).unwrap()
}

fn run_query(query: &str, cli: &Cli) -> Result<()> {
    let query = query.trim();
    if query.is_empty() {
        anyhow::bail!("No query provided");
    }

    let base = cli.endpoint.as_deref().unwrap_or("http://localhost:9200");
    let url = if cli.explain {
        format!("{}/_plugins/_ppl/_explain", base)
    } else {
        format!("{}/_plugins/_ppl", base)
    };
    let body = QueryRequest {
        query: query.to_string(),
        profile: cli.profile,
    };

    // Export mode: output curl command
    if cli.export {
        let json_body = serde_json::to_string(&body)?;
        let mut cmd = format!(
            "curl -X POST '{}' -H 'Content-Type: application/json' -d '{}' --max-time 120",
            url, json_body
        );
        if let Some(auth) = &cli.auth {
            cmd.push_str(&format!(" -u '{}'", auth));
        }
        println!("{}", cmd);
        return Ok(());
    }

    // Execute query
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    let mut req = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body);

    if let Some(auth) = &cli.auth {
        let parts: Vec<&str> = auth.splitn(2, ':').collect();
        if parts.len() == 2 {
            req = req.basic_auth(parts[0], Some(parts[1]));
        }
    }

    let response = req.send().context("Failed to send request")?;

    let status = response.status();
    let response_text = response.text()?;

    // Handle explain mode separately
    if cli.explain {
        if !status.is_success() {
            eprintln!("Error: Request failed with status {}", status);
        }

        if let Ok(json_val) = serde_json::from_str::<Value>(&response_text) {
            if let Some(formatted) = explain::format_calcite_explain(&json_val) {
                println!("{}", formatted);
            } else if io::stdout().is_terminal() {
                println!("{}", colorize_json(&json_val));
            } else {
                println!("{}", serde_json::to_string_pretty(&json_val)?);
            }
        } else {
            println!("{}", response_text);
        }
        return Ok(());
    }

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
        anyhow::bail!("Query failed");
    }

    let ppl_response: PplResponse =
        serde_json::from_str(&response_text).context("Failed to parse response as PPL result")?;

    // Output results
    if cli.table {
        println!("{}", format_table(&ppl_response));
    } else {
        let mut results = format_jdbc_to_json(&ppl_response);
        if let Some(limit) = cli.limit {
            results.truncate(limit);
        }

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

fn main() -> Result<()> {
    let mut cli = Cli::parse();

    // Expand globs if needed
    let mut resolved_files = Vec::new();
    for pattern in &cli.files {
        if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
            let matches: Vec<_> = glob::glob(pattern)
                .with_context(|| format!("Invalid glob pattern: {}", pattern))?
                .collect::<Result<Vec<_>, _>>()?;
            resolved_files.extend(matches);
        } else {
            resolved_files.push(std::path::PathBuf::from(pattern));
        }
    }

    // Batch mode: multiple files
    if resolved_files.len() > 1 {
        if !cli.compact && !cli.table {
            cli.compact = true; // ponytail: auto-compact for batch
        }
        if cli.limit.is_none() {
            cli.limit = Some(10); // ponytail: auto-limit for batch
        }

        for path in resolved_files {
            println!("--- {} ---", path.display());
            let query = match std::fs::read_to_string(&path) {
                Ok(q) => q,
                Err(e) => {
                    eprintln!("Error reading file: {}", e);
                    println!();
                    continue;
                }
            };

            if let Err(e) = run_query(&query, &cli) {
                eprintln!("Error: {}", e);
            }
            println!();
        }
        return Ok(());
    }

    // Single file or stdin
    let query = if let Some(file) = resolved_files.first() {
        std::fs::read_to_string(file)
            .with_context(|| format!("Failed to read file: {}", file.display()))?
    } else {
        let mut buffer = String::new();
        io::stdin()
            .read_to_string(&mut buffer)
            .context("Failed to read from stdin")?;
        buffer
    };

    run_query(&query, &cli)
}
