//! Redirect targets: keeping them on-site, and adding query parameters.

/// `next` if it is a path on this site, else `fallback`.
///
/// Only a single leading `/` is accepted. An absolute URL, a scheme-relative
/// `//host`, a backslash (`/\host`, which browsers read as `//host`) or a control
/// character would be an open redirect, so they fall back.
#[must_use]
pub fn safe_next(next: Option<&str>, fallback: &str) -> String {
    match next {
        Some(path)
            if path.starts_with('/')
                && !path.starts_with("//")
                && !path.contains('\\')
                && !path.contains("://")
                && !path.chars().any(char::is_control)
                && path.len() <= 2048 =>
        {
            path.to_owned()
        }
        _ => fallback.to_owned(),
    }
}

/// `path` with `pairs` appended to its query string (before any `#fragment`).
#[must_use]
pub fn with_query<K: AsRef<str>, V: AsRef<str>>(path: &str, pairs: &[(K, V)]) -> String {
    if pairs.is_empty() {
        return path.to_owned();
    }
    let (base, fragment) = match path.split_once('#') {
        Some((base, fragment)) => (base, Some(fragment)),
        None => (path, None),
    };
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs {
        query.append_pair(key.as_ref(), value.as_ref());
    }
    let separator = if base.contains('?') { '&' } else { '?' };
    let mut out = format!("{base}{separator}{}", query.finish());
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_on_site_paths() {
        assert_eq!(safe_next(Some("/games?page=2"), "/"), "/games?page=2");
        assert_eq!(safe_next(Some("/"), "/x"), "/");
    }

    #[test]
    fn refuses_open_redirects() {
        for bad in [
            "https://evil.example",
            "//evil.example",
            "/\\evil.example",
            "evil",
            "/x?u=https://evil",
            "/a\nb",
            "",
        ] {
            assert_eq!(safe_next(Some(bad), "/home"), "/home", "{bad:?}");
        }
        assert_eq!(safe_next(None, "/home"), "/home");
    }

    #[test]
    fn appends_query_parameters() {
        assert_eq!(
            with_query("/settings", &[("ogs", "verified")]),
            "/settings?ogs=verified"
        );
        assert_eq!(with_query("/?next=/a", &[("e", "x y")]), "/?next=/a&e=x+y");
        assert_eq!(with_query("/p#top", &[("a", "1")]), "/p?a=1#top");
        assert_eq!(with_query::<&str, &str>("/p", &[]), "/p");
    }
}
