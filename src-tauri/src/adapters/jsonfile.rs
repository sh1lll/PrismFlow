//! Editing another app's JSON file: add or remove PrismFlow's own entries
//! and keep everything else exactly as it was (key order included).
//!
//! A block of comment lines at the top (Zed's default settings file starts
//! with one) is kept exactly as it was. Comments or trailing commas inside
//! the settings are refused rather than rewritten, because rewriting would
//! drop them.

use serde_json::{Map, Value};
use std::path::Path;

pub fn read_object(file: &Path) -> Result<(String, Map<String, Value>), String> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((String::new(), Map::new())),
        Err(e) => return Err(format!("Couldn't read {}: {e}", file.display())),
    };
    if text.trim().is_empty() {
        return Ok((text, Map::new()));
    }
    match serde_json::from_str::<Value>(&text[header(&text).len()..]) {
        Ok(Value::Object(m)) => Ok((text, m)),
        Ok(_) => Err(format!("{} doesn't hold a JSON object, so PrismFlow won't change it.", file.display())),
        Err(_) => Err(format!(
            "{} has comments or a mistake inside its settings, so PrismFlow won't rewrite it. Set it up by hand (on Home, ⋯ → Finish in the app), or remove those comments.",
            file.display()
        )),
    }
}

/// The comment lines (and blank lines) before the opening `{`.
pub fn header(text: &str) -> &str {
    let mut end = 0;
    for line in text.split_inclusive('\n') {
        let t = line.trim();
        if t.is_empty() || t.starts_with("//") {
            end += line.len();
        } else {
            break;
        }
    }
    &text[..end]
}

/// `render`, keeping the original file's leading comment block.
pub fn render_like(before: &str, obj: &Map<String, Value>) -> String {
    format!("{}{}", header(before), render(obj))
}

/// `obj[path[0]][path[1]]… = value`, creating objects on the way.
pub fn set(obj: &mut Map<String, Value>, path: &[&str], value: Value) -> Result<(), String> {
    let (last, parents) = path.split_last().ok_or("empty path")?;
    let mut cur = obj;
    for p in parents {
        let next = cur.entry(p.to_string()).or_insert_with(|| Value::Object(Map::new()));
        cur = next.as_object_mut().ok_or_else(|| format!("\"{p}\" isn't an object, so PrismFlow won't change it."))?;
    }
    cur.insert(last.to_string(), value);
    Ok(())
}

pub fn get<'a>(obj: &'a Map<String, Value>, path: &[&str]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    rest.iter().try_fold(obj.get(*first)?, |v, p| v.get(*p))
}

/// Removes `path` if `keep` says it's ours. Returns whether something was removed.
pub fn remove_if(obj: &mut Map<String, Value>, path: &[&str], ours: impl Fn(&Value) -> bool) -> bool {
    let Some((last, parents)) = path.split_last() else { return false };
    let mut cur = obj;
    for p in parents {
        match cur.get_mut(*p).and_then(Value::as_object_mut) {
            Some(n) => cur = n,
            None => return false,
        }
    }
    if cur.get(*last).is_some_and(&ours) {
        cur.remove(*last);
        true
    } else {
        false
    }
}

pub fn render(obj: &Map<String, Value>) -> String {
    serde_json::to_string_pretty(&Value::Object(obj.clone())).unwrap_or_default() + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_other_entries_and_refuses_comments() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("acp.json");
        std::fs::write(&f, r#"{"zeta": 1, "agent_servers": {"Mine": {"command": "x"}}, "alpha": [1]}"#).unwrap();
        let (_, mut m) = read_object(&f).unwrap();
        set(&mut m, &["agent_servers", "PF"], json!({"command": "node"})).unwrap();
        let out = render(&m);
        assert!(out.find("zeta").unwrap() < out.find("alpha").unwrap(), "order kept: {out}");
        assert_eq!(get(&m, &["agent_servers", "Mine", "command"]), Some(&json!("x")));
        assert!(remove_if(&mut m, &["agent_servers", "PF"], |v| v["command"] == "node"));
        assert!(!remove_if(&mut m, &["agent_servers", "Mine"], |v| v["command"] == "node"), "never removes others");

        std::fs::write(&f, "{\n  // my comment\n  \"a\": 1,\n}").unwrap();
        assert!(read_object(&f).unwrap_err().contains("comments"));

        // Zed's default file: a comment block on top, plain JSON below.
        let zed = "// Zed settings\n//\n// For information on how to configure Zed, see the docs.\n{\n  \"theme\": \"One Dark\"\n}\n";
        std::fs::write(&f, zed).unwrap();
        let (before, mut m) = read_object(&f).unwrap();
        set(&mut m, &["language_models", "x"], json!(1)).unwrap();
        let out = render_like(&before, &m);
        assert!(out.starts_with("// Zed settings\n//\n// For information"), "{out}");
        assert!(out.contains("\"theme\": \"One Dark\"") && out.contains("language_models"));
        assert!(read_object(&dir.path().join("missing.json")).unwrap().1.is_empty());
    }
}
