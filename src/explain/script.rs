//! Decoder for Calcite pushdown scripts (`lang: opensearch_compounded_script`).
//!
//! Wire format (opensearch-project/sql, 3.4+):
//! ```text
//! source = {"langType":"calcite","script": B64}
//! B64    = base64(Java ObjectOutputStream of one String)
//! String = Calcite RelJson of a RexNode, fields/literals replaced by `dynamicParam` i
//! params = {"SOURCES":[...], "DIGESTS":[...]}  // SOURCES[i]: 0 doc, 1 _source, 2 literal, 3 flat_object
//! ```
//! Under `explain extended` the script is the raw RexNode JSON (no base64/Java wrapping).
//!
//! Output is three-address code: leaves (fields, literals) inline, one `$tN` line per call,
//! identical subtrees shared (so repeated work shows up as one line used twice).

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Ops that `ScriptDetector` (sql repo) classes as expensive per-document UDFs.
const EXPENSIVE_UDFS: &[&str] = &[
    "REX_EXTRACT",
    "REX_EXTRACT_MULTI",
    "PARSE",
    "PATTERN_PARSER",
];

/// Decode one compounded-script `source` string into display lines (unindented).
pub fn decode(source: &str, params: Option<&Map<String, Value>>) -> Result<Vec<String>> {
    let wrapper: Value =
        serde_json::from_str(source).context("source is not a langType wrapper")?;
    let lang = wrapper
        .get("langType")
        .and_then(Value::as_str)
        .unwrap_or("?");
    if lang != "calcite" {
        bail!("langType '{lang}' is not decodable (v2 scripts are Java-serialized objects)");
    }
    let script = wrapper
        .get("script")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("wrapper has no script"))?;
    decode_script(script, params)
}

/// Decode the inner `script` value: base64 Java-serialized RexNode JSON, or raw RexNode JSON.
pub fn decode_script(script: &str, params: Option<&Map<String, Value>>) -> Result<Vec<String>> {
    let rex_json = if script.trim_start().starts_with('{') {
        script.to_string() // explain extended: skipEncoding
    } else {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(script)
            .context("script is not valid base64")?;
        java_serialized_string(&bytes)?
    };
    let rex: Value = serde_json::from_str(&rex_json).context("script is not RexNode JSON")?;

    let ints = |k: &str| -> Vec<i64> {
        params
            .and_then(|p| p.get(k))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default()
    };
    let sources = ints("SOURCES");
    let digests: Vec<Value> = params
        .and_then(|p| p.get("DIGESTS"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut r = Renderer {
        sources: &sources,
        digests: &digests,
        lines: Vec::new(),
        memo: HashMap::new(),
        reads: Vec::new(),
    };
    let result = r.expr(&rex);

    let mut out = Vec::new();
    if !r.reads.is_empty() {
        let mut header = format!("# reads: {}", r.reads.join(", "));
        if r.reads.iter().any(|f| f.starts_with("_source[")) {
            header.push_str("  (_source reads parse the document per hit)");
        }
        out.push(header);
    }
    out.extend(r.lines);
    out.push(format!("return {result}"));
    Ok(out)
}

/// Read the single `String` in a Java serialization stream (TC_STRING / TC_LONGSTRING).
fn java_serialized_string(b: &[u8]) -> Result<String> {
    if b.len() < 5 || b[..4] != [0xAC, 0xED, 0x00, 0x05] {
        bail!("not a Java serialization stream");
    }
    let (len, start) = match b[4] {
        0x74 if b.len() >= 7 => (u16::from_be_bytes([b[5], b[6]]) as usize, 7),
        0x7C if b.len() >= 13 => (u64::from_be_bytes(b[5..13].try_into()?) as usize, 13),
        0x73 => bail!("legacy map-format script (sql 3.2-3.3) is not supported yet"),
        tc => bail!("unexpected Java stream tag {tc:#04x}"),
    };
    let body = b
        .get(start..start + len)
        .ok_or_else(|| anyhow!("truncated Java string"))?;
    Ok(modified_utf8(body))
}

/// Java "modified UTF-8": NUL as C0 80, supplementary chars as surrogate pairs (CESU-8).
fn modified_utf8(b: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as u16;
        let (unit, n) = if c < 0x80 {
            (c, 1)
        } else if c & 0xE0 == 0xC0 && i + 1 < b.len() {
            (((c & 0x1F) << 6) | (b[i + 1] as u16 & 0x3F), 2)
        } else if c & 0xF0 == 0xE0 && i + 2 < b.len() {
            (
                ((c & 0x0F) << 12) | ((b[i + 1] as u16 & 0x3F) << 6) | (b[i + 2] as u16 & 0x3F),
                3,
            )
        } else {
            (0xFFFD, 1)
        };
        units.push(unit);
        i += n;
    }
    String::from_utf16_lossy(&units)
}

struct Renderer<'a> {
    sources: &'a [i64],
    digests: &'a [Value],
    lines: Vec<String>,
    /// call text (with operands already reduced to refs/leaves) -> `$tN`
    memo: HashMap<String, String>,
    /// distinct field reads, in first-use order
    reads: Vec<String>,
}

impl Renderer<'_> {
    /// Render a node to inline text: a leaf, or the `$tN` ref of a hoisted call.
    fn expr(&mut self, n: &Value) -> String {
        let Some(obj) = n.as_object() else {
            return compact(n);
        };
        if let Some(op) = obj.get("op") {
            return self.call(op, obj);
        }
        if let Some(i) = obj.get("dynamicParam").and_then(Value::as_u64) {
            return self.param(i as usize);
        }
        if let Some(lit) = obj.get("literal") {
            return literal(lit, obj.get("type"));
        }
        if let (Some(expr), Some(field)) = (obj.get("expr"), obj.get("field")) {
            return format!("{}.{}", self.expr(expr), field.as_str().unwrap_or("?"));
        }
        if let Some(input) = obj.get("input") {
            return match obj.get("name").and_then(Value::as_str) {
                Some(name) => name.to_string(),
                None => format!("${input}"),
            };
        }
        if let Some(c) = obj.get("correl").and_then(Value::as_str) {
            return c.to_string();
        }
        compact(n)
    }

    fn param(&mut self, i: usize) -> String {
        let (Some(src), Some(digest)) = (self.sources.get(i), self.digests.get(i)) else {
            return format!("?{i}"); // params block missing (e.g. bare script blob)
        };
        let name = digest
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| compact(digest));
        let field = match src {
            2 => return compact(digest), // literal
            0 => format!("doc[{name}]"),
            1 => format!("_source[{name}]"),
            3 => format!("flat[{name}]"),
            other => format!("src{other}[{name}]"),
        };
        if !self.reads.contains(&field) {
            self.reads.push(field.clone());
        }
        field
    }

    fn call(&mut self, op: &Value, obj: &Map<String, Value>) -> String {
        let name = op.get("name").and_then(Value::as_str).unwrap_or("?");
        let kind = op.get("kind").and_then(Value::as_str).unwrap_or("");
        let syntax = op
            .get("syntax")
            .and_then(Value::as_str)
            .unwrap_or("FUNCTION");
        let args: Vec<String> = obj
            .get("operands")
            .and_then(Value::as_array)
            .map(|ops| ops.iter().map(|o| self.expr(o)).collect())
            .unwrap_or_default();
        let ty = obj.get("type").map(type_name);

        let text = match (kind, syntax) {
            ("CAST" | "SAFE_CAST", _) if args.len() == 1 => {
                format!("{name}({} AS {})", args[0], ty.as_deref().unwrap_or("?"))
            }
            ("CASE", _) => {
                let mut parts: Vec<String> = args
                    .chunks(2)
                    .filter(|c| c.len() == 2)
                    .map(|c| format!("{} => {}", c[0], c[1]))
                    .collect();
                if args.len() % 2 == 1 {
                    parts.push(format!("else {}", args[args.len() - 1]));
                }
                format!("CASE({})", parts.join(", "))
            }
            (_, "BINARY") if args.len() >= 2 => args.join(&format!(" {name} ")),
            (_, "PREFIX") if args.len() == 1 => format!("{name} {}", args[0]),
            (_, "POSTFIX") if args.len() == 1 => format!("{} {name}", args[0]),
            _ => format!("{name}({})", args.join(", ")),
        };

        if let Some(r) = self.memo.get(&text) {
            return r.clone();
        }
        let r = format!("$t{}", self.lines.len());

        let mut notes = Vec::new();
        // CAST already shows its target type inline
        if let Some(t) = &ty
            && !matches!(kind, "CAST" | "SAFE_CAST")
        {
            notes.push(t.clone());
        }
        if EXPENSIVE_UDFS.contains(&name) {
            notes.push("expensive UDF".into());
        } else if name.starts_with("REGEXP") || name == "RLIKE" {
            notes.push("regex".into());
        }
        let line = if notes.is_empty() {
            format!("{r} = {text}")
        } else {
            format!("{r} = {text}  # {}", notes.join(", "))
        };
        self.lines.push(line);
        self.memo.insert(text, r.clone());
        r
    }
}

fn literal(lit: &Value, ty: Option<&Value>) -> String {
    let sym = ty.and_then(|t| t.get("type")).and_then(Value::as_str) == Some("SYMBOL");
    match lit {
        Value::String(s) if sym => s.clone(),
        _ => compact(lit),
    }
}

/// `{"type":"VARCHAR","nullable":true}` -> `VARCHAR?`; UDTs show their UDT name.
fn type_name(t: &Value) -> String {
    let base = t
        .get("udt")
        .or_else(|| t.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let base = match t.get("component") {
        Some(c) => format!("{base}<{}>", type_name(c)),
        None => base,
    };
    if t.get("nullable").and_then(Value::as_bool) == Some(true) {
        format!("{base}?")
    } else {
        base
    }
}

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "?".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // From sql integ-test expectedOutput/calcite/explain_script_push_on_text.yaml
    const LT_CHAR_LENGTH: &str = "rO0ABXQCHHsKICAib3AiOiB7CiAgICAibmFtZSI6ICI8IiwKICAgICJraW5kIjogIkxFU1NfVEhBTiIsCiAgICAic3ludGF4IjogIkJJTkFSWSIKICB9LAogICJvcGVyYW5kcyI6IFsKICAgIHsKICAgICAgImR5bmFtaWNQYXJhbSI6IDAsCiAgICAgICJ0eXBlIjogewogICAgICAgICJ0eXBlIjogIkJJR0lOVCIsCiAgICAgICAgIm51bGxhYmxlIjogdHJ1ZQogICAgICB9CiAgICB9LAogICAgewogICAgICAib3AiOiB7CiAgICAgICAgIm5hbWUiOiAiQ0hBUl9MRU5HVEgiLAogICAgICAgICJraW5kIjogIkNIQVJfTEVOR1RIIiwKICAgICAgICAic3ludGF4IjogIkZVTkNUSU9OIgogICAgICB9LAogICAgICAib3BlcmFuZHMiOiBbCiAgICAgICAgewogICAgICAgICAgImR5bmFtaWNQYXJhbSI6IDEsCiAgICAgICAgICAidHlwZSI6IHsKICAgICAgICAgICAgInR5cGUiOiAiVkFSQ0hBUiIsCiAgICAgICAgICAgICJudWxsYWJsZSI6IHRydWUsCiAgICAgICAgICAgICJwcmVjaXNpb24iOiAtMQogICAgICAgICAgfQogICAgICAgIH0KICAgICAgXQogICAgfQogIF0KfQ==";

    fn wrap(script: &str) -> String {
        json!({"langType": "calcite", "script": script}).to_string()
    }

    fn params(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn decodes_real_fixture_script() {
        let p = params(json!({"utcTimestamp": 0, "SOURCES": [2, 1], "DIGESTS": [0, "address"]}));
        let lines = decode(&wrap(LT_CHAR_LENGTH), Some(&p)).unwrap();
        assert_eq!(
            lines,
            vec![
                "# reads: _source[address]  (_source reads parse the document per hit)",
                "$t0 = CHAR_LENGTH(_source[address])",
                "$t1 = 0 < $t0",
                "return $t1",
            ]
        );
    }

    #[test]
    fn missing_params_render_as_placeholders() {
        let lines = decode(&wrap(LT_CHAR_LENGTH), None).unwrap();
        assert_eq!(
            lines,
            vec!["$t0 = CHAR_LENGTH(?1)", "$t1 = ?0 < $t0", "return $t1"]
        );
    }

    fn call(name: &str, kind: &str, syntax: &str, operands: Vec<Value>) -> Value {
        json!({"op": {"name": name, "kind": kind, "syntax": syntax}, "operands": operands})
    }

    fn p(i: u64) -> Value {
        json!({"dynamicParam": i, "type": {"type": "VARCHAR", "nullable": true}})
    }

    #[test]
    fn raw_json_script_and_common_subexpressions() {
        // UPPER(x) used twice -> one line, two uses
        let upper = call("UPPER", "OTHER_FUNCTION", "FUNCTION", vec![p(0)]);
        let rex = call(
            "=",
            "EQUALS",
            "BINARY",
            vec![
                upper.clone(),
                call("CONCAT", "OTHER_FUNCTION", "FUNCTION", vec![upper, p(1)]),
            ],
        );
        let pr = params(json!({"SOURCES": [0, 2], "DIGESTS": ["name.keyword", "!"]}));
        let lines = decode(&wrap(&rex.to_string()), Some(&pr)).unwrap();
        assert_eq!(
            lines,
            vec![
                "# reads: doc[name.keyword]",
                "$t0 = UPPER(doc[name.keyword])",
                "$t1 = CONCAT($t0, \"!\")",
                "$t2 = $t0 = $t1",
                "return $t2",
            ]
        );
    }

    #[test]
    fn cast_case_and_expensive_udf() {
        let mut cast = call("CAST", "CAST", "SPECIAL", vec![p(0)]);
        cast["type"] = json!({"type": "INTEGER", "nullable": true});
        let mut rex_extract = call("REX_EXTRACT", "OTHER_FUNCTION", "FUNCTION", vec![p(1)]);
        rex_extract["type"] = json!({"type": "VARCHAR", "nullable": true});
        let rex = call(
            "CASE",
            "CASE",
            "SPECIAL",
            vec![
                call("IS NULL", "IS_NULL", "POSTFIX", vec![cast]),
                rex_extract,
                json!({"literal": null, "type": {"type": "NULL", "nullable": true}}),
            ],
        );
        let pr = params(json!({"SOURCES": [0, 1], "DIGESTS": ["code", "msg"]}));
        let lines = decode(&wrap(&rex.to_string()), Some(&pr)).unwrap();
        assert_eq!(
            lines[1..],
            [
                "$t0 = CAST(doc[code] AS INTEGER?)",
                "$t1 = $t0 IS NULL",
                "$t2 = REX_EXTRACT(_source[msg])  # VARCHAR?, expensive UDF",
                "$t3 = CASE($t1 => $t2, else null)",
                "return $t3",
            ]
        );
    }

    #[test]
    fn rejects_v2_and_legacy_formats() {
        let v2 = json!({"langType": "v2", "script": "rO0AB"}).to_string();
        assert!(decode(&v2, None).unwrap_err().to_string().contains("v2"));
        let legacy =
            base64::engine::general_purpose::STANDARD.encode([0xAC, 0xED, 0, 5, 0x73, 0x72]);
        assert!(
            decode(&wrap(&legacy), None)
                .unwrap_err()
                .to_string()
                .contains("legacy")
        );
    }

    #[test]
    fn modified_utf8_nul_and_surrogates() {
        // "a\0" + U+1F600 as CESU-8 surrogate pair
        let b = [0x61, 0xC0, 0x80, 0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80];
        assert_eq!(modified_utf8(&b), "a\0\u{1F600}");
    }
}
