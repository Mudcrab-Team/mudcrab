//! Guard: a SQL NULL column must stay absent in the engine's SQLite readers (#1095).
//!
//! The VMAD property reader once read every value column as `Option<_>` and then replaced
//! NULL with `0`, `""` or `false` (`object.unwrap_or(0)`, `string.unwrap_or_default()`). A
//! corrupt row then looked like a real zero value. This test scans the production code of
//! `crates/engine/src` for the same shape: a value read with `row.get` as `Option<_>` whose
//! result is turned into a literal default by `unwrap_or(<literal>)`, `unwrap_or_default()`
//! or `map_or(<literal>, ..)`.
//!
//! A site is allowed when the line, or the line above it, carries `// null-ok: <reason>`.
//! The reason must say why the default is retail data, not an invention. Sites that exist
//! today and are not fixed yet are listed in [`KNOWN`]. The count per file may not rise, and
//! it must be lowered here when a site is fixed.

use std::path::{Path, PathBuf};

/// `(file, number of existing unlabelled sites)`. `world-inspect` prints an interior cell's
/// missing grid as `[0, 0]`; a report that shows `null` instead is the fix.
const KNOWN: &[(&str, usize)] = &[("crates/engine/src/bin/world-inspect.rs", 2)];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
            out.push(path);
        }
    }
}

/// The production part of a file: everything before the first `#[cfg(test)]` line.
fn production(source: &str) -> &str {
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        if line.trim_start().starts_with("#[cfg(test)]") {
            return &source[..offset];
        }
        offset += line.len();
    }
    source
}

fn is_identifier_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether an `unwrap_or`/`map_or` argument is a literal that invents data from NULL.
fn is_default_literal(argument: &str) -> bool {
    let argument = argument.trim();
    argument.parse::<f64>().is_ok()
        || matches!(
            argument,
            "false" | "true" | "\"\"" | "String::new()" | "Default::default()"
        )
}

/// The text of the call that opens at `open` (the index of its `(`), without the parens.
fn call_arguments(text: &str, open: usize) -> &str {
    let mut depth = 0_i32;
    for (index, c) in text[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open + 1..open + index];
                }
            }
            _ => {}
        }
    }
    &text[open + 1..]
}

/// Whether `expression` ends in a NULL-to-literal coercion.
fn coerces_to_literal(expression: &str) -> bool {
    if expression.contains(".unwrap_or_default(") {
        return true;
    }
    for call in [".unwrap_or(", ".map_or("] {
        let mut from = 0;
        while let Some(at) = expression[from..].find(call) {
            let open = from + at + call.len() - 1;
            let arguments = call_arguments(expression, open);
            let first = arguments.split(',').next().unwrap_or(arguments);
            if is_default_literal(first) {
                return true;
            }
            from = open + 1;
        }
    }
    false
}

/// The expression that starts at `start`: up to the first `;`, `,` or closing bracket that
/// is not inside a call.
fn expression_from(text: &str, start: usize) -> &str {
    let mut depth = 0_i32;
    for (index, c) in text[start..].char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth < 0 {
                    return &text[start..start + index];
                }
            }
            ';' | ',' if depth == 0 => return &text[start..start + index],
            _ => {}
        }
    }
    &text[start..]
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

fn exempt(source: &str, line: usize) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    let here = lines.get(line - 1).copied().unwrap_or_default();
    let above = line
        .checked_sub(2)
        .and_then(|index| lines.get(index))
        .copied()
        .unwrap_or_default();
    [here, above].iter().any(|text| {
        text.split_once("// null-ok:")
            .is_some_and(|(_, reason)| !reason.trim().is_empty())
    })
}

/// Line numbers of every unlabelled NULL-to-literal coercion in `source`.
fn coercions(source: &str) -> Vec<usize> {
    let code = production(source);
    let mut lines = Vec::new();
    // `row.get::<_, Option<T>>(N)?.unwrap_or(0)` and `row.get::<_, Option<T>>(N)?.map(..).unwrap_or(0)`.
    let mut from = 0;
    while let Some(at) = code[from..].find("row.get") {
        let start = from + at;
        from = start + "row.get".len();
        let call_end = code[start..].find('(').map(|open| start + open);
        let Some(turbofish_end) = call_end else {
            continue;
        };
        if !code[start..turbofish_end].contains("Option<") {
            continue;
        }
        if coerces_to_literal(expression_from(code, turbofish_end)) {
            lines.push(line_of(code, start));
        }
    }
    // `let name: Option<T> = row.get(N)?;` followed by `name.unwrap_or(0)`.
    for (number, line) in code.lines().enumerate() {
        let Some(rest) = line.trim_start().strip_prefix("let ") else {
            continue;
        };
        let Some((name, after)) = rest.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || !name.chars().all(is_identifier_char) {
            continue;
        }
        if !(after.trim_start().starts_with("Option<") && after.contains("row.get")) {
            continue;
        }
        for (use_number, use_line) in code.lines().enumerate().skip(number + 1) {
            let mut search = 0;
            while let Some(found) = use_line[search..].find(name) {
                let begin = search + found;
                let end = begin + name.len();
                search = end;
                let before = use_line[..begin].chars().next_back();
                if before.is_some_and(is_identifier_char) {
                    continue;
                }
                let tail = &use_line[end..];
                if tail.starts_with(".unwrap_or_default()")
                    || tail
                        .strip_prefix(".unwrap_or(")
                        .or_else(|| tail.strip_prefix(".map_or("))
                        .is_some_and(|arguments| {
                            is_default_literal(arguments.split([',', ')']).next().unwrap_or(""))
                        })
                {
                    lines.push(use_number + 1);
                }
            }
        }
    }
    lines.sort_unstable();
    lines.dedup();
    lines.retain(|line| !exempt(code, *line));
    lines
}

#[test]
fn the_scan_finds_the_shapes_that_caused_1095() {
    // The reader as it was written before the fix, reduced to its coercions.
    let old = r#"
        let object: Option<u32> = row.get(0)?;
        let string: Option<String> = row.get(2)?;
        let boolean: Option<i64> = row.get(5)?;
        Ok(match kind {
            VMAD_OBJECT => form_id: object.unwrap_or(0),
            VMAD_STRING => PropertyValue::Str(string.unwrap_or_default()),
            VMAD_BOOL => PropertyValue::Bool(boolean.unwrap_or(0) != 0),
        })
    "#;
    assert_eq!(coercions(old).len(), 3, "{:?}", coercions(old));
    let direct = "let x = row.get::<_, Option<i32>>(3)?.unwrap_or(0);\n";
    assert_eq!(coercions(direct), vec![1]);
    let mapped = "let x = row.get::<_, Option<u32>>(2)?.map(f).unwrap_or(false);\n";
    assert_eq!(coercions(mapped), vec![1]);
}

#[test]
fn the_scan_leaves_honest_handling_alone() {
    // A NULL that stays absent, an explicit sentinel, a non-Option read and a labelled site.
    let clean = r#"
        let object: Option<u32> = row.get(0)?;
        let value = object.ok_or_else(|| "object_form_id is NULL".to_string())?;
        let side = row.get::<_, Option<u32>>(2)?.map(f).unwrap_or(PortalSide::Unknown);
        let total: i64 = row.get(0)?;
        let count = total.unwrap_or(0);
    "#;
    assert!(coercions(clean).is_empty(), "{:?}", coercions(clean));
    let labelled = "// null-ok: xEdit says an absent flags field is 0\n\
                    let flags = row.get::<_, Option<u32>>(1)?.unwrap_or(0);\n";
    assert!(coercions(labelled).is_empty());
    let empty_reason = "// null-ok:\nlet flags = row.get::<_, Option<u32>>(1)?.unwrap_or(0);\n";
    assert_eq!(coercions(empty_reason), vec![2]);
    // Test-only code after `#[cfg(test)]` is out of scope.
    let test_code =
        "fn f() {}\n#[cfg(test)]\nmod t { let x = row.get::<_, Option<u32>>(1)?.unwrap_or(0); }\n";
    assert!(coercions(test_code).is_empty());
}

#[test]
fn the_engine_readers_keep_null_absent() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = root.parent().unwrap().parent().unwrap();
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    files.sort();
    let mut problems = Vec::new();
    for file in &files {
        let relative = file
            .strip_prefix(workspace)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let source = std::fs::read_to_string(file).unwrap();
        let found = coercions(&source);
        let allowed = KNOWN
            .iter()
            .find(|(name, _)| *name == relative)
            .map_or(0, |(_, count)| *count);
        if found.len() != allowed {
            problems.push(format!(
                "{relative}: {} NULL-to-literal coercions at lines {found:?}, {allowed} allowed. \
                 Keep NULL absent, or label a retail default with `// null-ok: <reason>`. \
                 Lower KNOWN in this test when a known site is fixed.",
                found.len()
            ));
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}
