use icu_locid::Locale;

const MAX_LOCALE_TAG_BYTES: usize = 64;

/// Parse a BCP 47 tag and normalize its case without applying alias migrations.
/// Keeping aliases such as `iw` intact avoids silently changing a persisted catalog key.
pub(crate) fn canonical_bcp47_tag(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > MAX_LOCALE_TAG_BYTES || value.contains('_') {
        return None;
    }
    let locale = value.parse::<Locale>().ok()?;
    let canonical = locale.to_string();
    (canonical.len() <= MAX_LOCALE_TAG_BYTES).then_some(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case_without_alias_migration() {
        assert_eq!(canonical_bcp47_tag("en-us").as_deref(), Some("en-US"));
        assert_eq!(
            canonical_bcp47_tag("zh-hant-tw-u-nu-hanidec").as_deref(),
            Some("zh-Hant-TW-u-nu-hanidec")
        );
        assert_eq!(canonical_bcp47_tag("IW").as_deref(), Some("iw"));
    }

    #[test]
    fn rejects_invalid_or_oversized_tags() {
        for tag in ["", "lo_LA", "en--US", "en-u", "abcdefghi"] {
            assert_eq!(canonical_bcp47_tag(tag), None, "accepted {tag}");
        }
        assert_eq!(canonical_bcp47_tag(&"a".repeat(65)), None);
    }
}
