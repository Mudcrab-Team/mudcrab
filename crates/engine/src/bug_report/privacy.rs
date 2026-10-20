//! Shared text redaction for every bug-report export surface.

use std::{path::PathBuf, sync::LazyLock};

use regex::Regex;
use serde_json::Value;

#[derive(Debug)]
struct Root {
    pattern: Regex,
    replacement: String,
}

/// Replaces known local roots, credentials, and unrecognized absolute paths.
/// This is a defensive filter, not proof that arbitrary text is secret-free.
#[derive(Debug)]
pub struct Sanitizer {
    roots: Vec<Root>,
}

impl Sanitizer {
    pub fn new(mut roots: Vec<(PathBuf, String)>) -> Self {
        // Prefer the asset/report marker over a containing home-directory marker.
        roots.sort_by_key(|(path, _)| std::cmp::Reverse(path.as_os_str().len()));
        let roots = roots
            .into_iter()
            .filter_map(|(path, replacement)| {
                let path = path.to_string_lossy();
                let path = path.trim_end_matches(['/', '\\']);
                if path.is_empty() || path.len() == 2 && path.ends_with(':') {
                    return None;
                }
                let windows = path.contains('\\') || path.as_bytes().get(1) == Some(&b':');
                let escaped = path
                    .split(['/', '\\'])
                    .map(regex::escape)
                    .collect::<Vec<_>>()
                    .join(r"[\\/]");
                let pattern =
                    Regex::new(&format!("{}{}", if windows { "(?i)" } else { "" }, escaped))
                        .expect("escaped path is a valid regex");
                Some(Root {
                    pattern,
                    replacement,
                })
            })
            .collect();
        Self { roots }
    }

    pub fn sanitize(&self, input: &str) -> String {
        let mut text = strip_controls(input);
        text = PRIVATE_KEY
            .replace_all(&text, "[redacted private key]")
            .into_owned();
        text = URL_USERINFO
            .replace_all(&text, "$1[redacted]@")
            .into_owned();
        // A generic drive-path matcher can consume the final `e:/` of a file URI
        // before its boundary check rejects it. Redact the complete URI first.
        text = FILE_URI.replace_all(&text, "$1<path>").into_owned();
        text = BEARER.replace_all(&text, "$1[redacted]").into_owned();
        text = SECRET_HEADER
            .replace_all(&text, "$1[redacted]")
            .into_owned();
        text = SECRET_ASSIGNMENT
            .replace_all(&text, "$1[redacted]")
            .into_owned();
        text = TOKEN.replace_all(&text, "[redacted]").into_owned();
        for root in &self.roots {
            text = replace_matches(
                &text,
                &root.pattern,
                |start, end| {
                    path_boundary(&text, start)
                        && text[end..]
                            .chars()
                            .next()
                            .is_none_or(|c| c == '/' || c == '\\' || path_end(c))
                },
                &root.replacement,
            );
        }
        text = QUOTED_ABSOLUTE_PATH
            .replace_all(&text, "<path>")
            .into_owned();
        replace_matches(
            &text,
            &ABSOLUTE_PATH,
            |start, _| path_boundary(&text, start),
            "<path>",
        )
    }

    /// Redacts string values and sensitive keys without cloning the JSON tree.
    pub fn sanitize_value(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.sanitize(text),
            Value::Array(values) => {
                for value in values {
                    self.sanitize_value(value);
                }
            }
            Value::Object(values) => {
                // Object keys can also originate in diagnostic input. Moving entries avoids
                // leaving paths or credential values in an otherwise sanitized document.
                let original = std::mem::take(values);
                for (key, mut value) in original {
                    if sensitive_key(&key) {
                        value = Value::String("[redacted]".into());
                    } else {
                        self.sanitize_value(&mut value);
                    }
                    values.insert(self.sanitize(&key), value);
                }
            }
            _ => {}
        }
    }
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace('-', "_");
    matches!(
        key.as_str(),
        "password"
            | "passwd"
            | "pwd"
            | "secret"
            | "token"
            | "api_key"
            | "apikey"
            | "authorization"
            | "cookie"
            | "cookies"
            | "set_cookie"
            | "credentials"
            | "private_key"
            | "session_key"
            | "privatekey"
            | "sessionkey"
            | "accesstoken"
            | "refreshtoken"
            | "authtoken"
            | "clientsecret"
            | "setcookie"
    ) || key.contains("password")
        || key.contains("passwd")
        || key.contains("secret")
        || key.ends_with("token")
        || key.ends_with("_pwd")
        || [
            "api_key",
            "apikey",
            "access_key",
            "accesskey",
            "private_key",
            "privatekey",
            "session_key",
            "sessionkey",
        ]
        .iter()
        .any(|suffix| key.ends_with(suffix))
}

fn replace_matches(
    input: &str,
    pattern: &Regex,
    accept: impl Fn(usize, usize) -> bool,
    replacement: &str,
) -> String {
    let mut output = String::with_capacity(input.len());
    let mut copied = 0;
    for found in pattern.find_iter(input) {
        if accept(found.start(), found.end()) {
            output.push_str(&input[copied..found.start()]);
            output.push_str(replacement);
            copied = found.end();
        }
    }
    output.push_str(&input[copied..]);
    output
}

fn path_end(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '\'' | '"' | '`' | '<' | '>' | ',' | ';' | ')' | ']' | '}' | ':'
        )
}

fn path_boundary(input: &str, start: usize) -> bool {
    if start == 0 {
        return true;
    }
    let before = &input[..start];
    let previous = before.chars().next_back().unwrap();
    // Preserve URLs rather than mistaking their authority slash for a machine path.
    if previous == ':' {
        let scheme = before[..before.len() - 1]
            .rsplit(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
            .next()
            .unwrap_or("");
        if matches!(
            scheme.to_ascii_lowercase().as_str(),
            "https" | "http" | "ftp" | "file"
        ) {
            return scheme.eq_ignore_ascii_case("file");
        }
    }
    previous.is_whitespace()
        || matches!(
            previous,
            '=' | ':' | '(' | '[' | '{' | '"' | '\'' | '`' | ',' | ';'
        )
}

/// Removes terminal escape sequences and nonprinting controls, preserving UTF-8,
/// line breaks, and tabs. OSC payloads are omitted, not exported as ordinary text.
fn strip_controls(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if c == '\n' || c == '\t' || !c.is_control() {
            output.push(c);
        }
    }
    output
}

static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)-----BEGIN (?:[A-Z ]*PRIVATE KEY)-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)",
    )
    .unwrap()
});
static URL_USERINFO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(\b(?:https?|ftp)://)[^\s/@]+@").unwrap());
static FILE_URI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)(\bfile:)[\\/]{1,3}[^\s\x00-\x1f'"`<>;,\)\]\}]+"#).unwrap());
static SECRET_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    // Names may carry a prefix (`DB_PASSWORD`, `GITLAB_TOKEN`) and a separated or camel-case
    // suffix (`AWS_SECRET_ACCESS_KEY`, `secretKey`) without matching words like `tokens`.
    Regex::new(r#"(?i)(\b[a-z0-9_-]*?(?:password|passwd|pwd|secret|token|(?:api|session|access|private)[_-]?key)(?:[_-][a-z0-9_-]*|(?-i:[A-Z][A-Za-z0-9_]*))?\b["']?[\t ]*[=:][\t ]*)(?:"[^"\r\n]*"|'[^'\r\n]*'|[^\s,;&\r\n]+)"#).unwrap()
});
static SECRET_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(\b(?:authorization|(?:set[_-]?)?cookie)\b["']?[\t ]*[=:][\t ]*)[^\r\n]*"#)
        .unwrap()
});
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(\b(?:Bearer|Basic)\s+)[A-Za-z0-9._~+/-]+=*").unwrap());
static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:gh[pousr]_[A-Za-z0-9_]{10,}|github_pat_[A-Za-z0-9_]{10,}|AKIA[A-Z0-9]{16}|sk-[A-Za-z0-9_-]{16,}|eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,})\b").unwrap()
});
static ABSOLUTE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:\b[A-Za-z]:[\\/]|\\\\|//|/)[^\s\x00-\x1f'"`<>;,\)\]\}]+"#).unwrap()
});
static QUOTED_ABSOLUTE_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:"(?:[A-Za-z]:[\\/]|\\\\|//|/)[^"\r\n]*"|'(?:[A-Za-z]:[\\/]|\\\\|//|/)[^'\r\n]*')"#,
    )
    .unwrap()
});

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sanitizer() -> Sanitizer {
        Sanitizer::new(vec![
            (PathBuf::from("/Users/player"), "~".into()),
            (
                PathBuf::from("/Volumes/Game Assets/Skyrim"),
                "<assets>".into(),
            ),
            (PathBuf::from(r"C:\Users\Player"), "~".into()),
            (
                PathBuf::from(r"\\fileserver\share\Skyrim"),
                "<assets>".into(),
            ),
        ])
    }

    #[test]
    fn roots_and_unknown_absolute_paths_are_redacted_without_hiding_relative_assets() {
        let actual = sanitizer().sanitize(
            r#"/Users/player/logs/x.txt /Volumes/Game Assets/Skyrim/meshes/桶.nif C:/users/PLAYER/a.log \\fileserver\share\Skyrim\textures\a.dds /Users/player2/private.dat Z:\other\secret.log \\other\private\file.txt meshes/armor/iron.nif 0x000123AB"#,
        );
        assert_eq!(
            actual,
            r#"~/logs/x.txt <assets>/meshes/桶.nif ~/a.log <assets>\textures\a.dds <path> <path> <path> meshes/armor/iron.nif 0x000123AB"#
        );
    }

    #[test]
    fn secrets_in_strings_and_nested_json_never_survive() {
        let mut value = json!({
            "error": "Authorization: Bearer SECRET_SENTINEL\npassword=PASS_SENTINEL\nhttps://user:URL_SECRET@example.test/foo?token=QUERY_SECRET&ok=1\nghp_abcdefghijklmnopqrstuvwxyz",
            "nested": [{"access_token": "NESTED_SECRET", "Cookie": "COOKIE_SECRET", "path": "/Users/player/a.log"}],
            "password": {"unusual": ["VALUE_SECRET"]},
            "/external/private/key": "safe",
        });
        sanitizer().sanitize_value(&mut value);
        let text = value.to_string();
        for sentinel in [
            "SECRET_SENTINEL",
            "PASS_SENTINEL",
            "URL_SECRET",
            "QUERY_SECRET",
            "NESTED_SECRET",
            "COOKIE_SECRET",
            "VALUE_SECRET",
            "ghp_abcdefghijklmnopqrstuvwxyz",
            "/external/private/key",
        ] {
            assert!(!text.contains(sentinel), "leaked {sentinel}: {text}");
        }
        assert_eq!(value["nested"][0]["path"], "~/a.log");
        assert!(text.contains("https://[redacted]@example.test/foo"));
    }

    #[test]
    fn control_sequences_and_private_key_payloads_are_removed() {
        let input = "\u{1b}[31merror\u{1b}[0m\u{1b}]0;OSC_SECRET\u{7} café\u{0}\n-----BEGIN PRIVATE KEY-----\nKEY_SECRET\n-----END PRIVATE KEY-----";
        assert_eq!(
            sanitizer().sanitize(input),
            "error café\n[redacted private key]"
        );
    }

    #[test]
    fn web_urls_remain_data_and_file_urls_hide_the_machine_path() {
        let text = sanitizer().sanitize("https://example.test/a/b file:///private/secret.log");
        assert_eq!(text, "https://example.test/a/b file:<path>");
        assert_eq!(
            sanitizer().sanitize(
                "FILE://server/private/file.txt file:/private/test.log textures/safe.dds"
            ),
            "FILE:<path> file:<path> textures/safe.dds"
        );
    }

    #[test]
    fn independent_paths_and_following_relative_assets_keep_their_boundaries() {
        let text = sanitizer().sanitize(r#""C:\Unknown User\private.txt" '/mnt/Unknown User/private.txt' meshes/armor/iron.nif /etc/engine.log textures/stone.dds"#);
        assert_eq!(
            text,
            "<path> <path> meshes/armor/iron.nif <path> textures/stone.dds"
        );
        assert_eq!(
            sanitizer().sanitize("error:/private/sentinel.log texture=meshes/safe.nif"),
            "error:<path> texture=meshes/safe.nif"
        );
    }

    #[test]
    fn quoted_json_credentials_in_log_text_and_camel_case_keys_are_redacted() {
        let input =
            r#"{"password":"PASSWORD_SENTINEL","accessToken":"TOKEN_SENTINEL","ok":"safe"}"#;
        let text = sanitizer().sanitize(input);
        assert!(!text.contains("PASSWORD_SENTINEL"));
        assert!(!text.contains("TOKEN_SENTINEL"));
        assert!(text.contains("safe"));
        let mut value = json!({"accessToken":"CAMEL_SENTINEL", "clientSecret":"CLIENT_SENTINEL", "privateKey":"PRIVATE_SENTINEL", "DB_PASSWORD":"DB_SENTINEL", "aws_secret_access_key":"AWS_SENTINEL", "GITLAB_TOKEN":"GITLAB_SENTINEL", "stripe_api_key":"STRIPE_SENTINEL"});
        sanitizer().sanitize_value(&mut value);
        let text = value.to_string();
        assert!(!text.contains("SENTINEL"));
    }

    #[test]
    fn prefixed_and_compound_credential_assignments_are_redacted() {
        let text = sanitizer().sanitize(
            "DB_PASSWORD=hunter2 MY_SECRET=MINE GITLAB_TOKEN=glpat-abc AWS_SECRET_ACCESS_KEY=wJalr/K7MDENG secretKey: CAMEL max_tokens=4096 tokenizer=bpe",
        );
        for sentinel in ["hunter2", "MINE", "glpat-abc", "wJalr", "CAMEL"] {
            assert!(!text.contains(sentinel), "leaked {sentinel}: {text}");
        }
        assert!(text.contains("max_tokens=4096"), "{text}");
        assert!(text.contains("tokenizer=bpe"), "{text}");
    }
}
