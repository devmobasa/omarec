//! Redact support-bundle text. Default bundles must not include raw homes or tokens.

pub fn redact_text(source: &str, home: &str, runtime: &str) -> String {
    let mut text = source.to_owned();
    if !home.is_empty() {
        text = text.replace(home, "$HOME");
    }
    if !runtime.is_empty() {
        text = text.replace(runtime, "$XDG_RUNTIME_DIR");
    }
    text
}

pub fn contains_sensitive(source: &str, home: &str) -> bool {
    !home.is_empty() && source.contains(home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_and_runtime_are_replaced() {
        let redacted = redact_text(
            "/home/alice/Videos/out.mp4 /run/user/1000/omarec/control.sock",
            "/home/alice",
            "/run/user/1000",
        );
        assert!(!redacted.contains("/home/alice"));
        assert!(redacted.contains("$HOME/Videos/out.mp4"));
        assert!(redacted.contains("$XDG_RUNTIME_DIR/omarec/control.sock"));
    }

    #[test]
    fn empty_home_does_not_false_positive() {
        assert!(!contains_sensitive("ok", ""));
        assert!(contains_sensitive("/home/alice/secret", "/home/alice"));
    }
}
