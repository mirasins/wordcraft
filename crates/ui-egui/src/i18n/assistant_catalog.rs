//! Merge the fork's assistant-only translations over the external app catalog at build time.

use std::collections::HashSet;

fn key(line: &str) -> Option<(&str, &str)> {
    if line.starts_with('#') || line.trim().is_empty() {
        return None;
    }
    let mut fields = line.splitn(3, '\t');
    let context = fields.next()?;
    let source = fields.next()?;
    fields.next()?;
    Some((context, source))
}

pub fn merge(base: &str, overlay: &str) -> String {
    let overrides: HashSet<_> = overlay.lines().filter_map(key).collect();
    let mut merged = String::new();
    for line in base.lines().filter(|line| key(line).is_none_or(|key| !overrides.contains(&key))) {
        merged.push_str(line);
        merged.push('\n');
    }
    merged.push_str(overlay);
    merged
}

#[cfg(test)]
mod tests {
    use super::merge;

    #[test]
    fn preserves_upstream_and_overrides_only_matching_keys() {
        let base = "# upstream\r\n\tFile\tArchivo\r\n\tAssistant\tOld\r\npreview\tAssistant\tPreview\r\n";
        let overlay = "# fork\n\tAssistant\tAsistente\n\tWaiting…\tEsperando…\n";
        let merged = merge(base, overlay);
        assert!(merged.contains("\tFile\tArchivo\n"));
        assert!(merged.contains("preview\tAssistant\tPreview\n"));
        assert!(merged.contains("\tAssistant\tAsistente\n"));
        assert!(merged.contains("\tWaiting…\tEsperando…\n"));
        assert!(!merged.contains("\tAssistant\tOld"));
        assert_eq!(merged.lines().filter(|line| line.starts_with("\tAssistant\t")).count(), 1);
    }

    #[test]
    fn handles_empty_catalog_and_missing_trailing_newline() {
        assert_eq!(merge("", "\tModel\tModelo"), "\tModel\tModelo");
        assert_eq!(merge("\tFile\tArchivo", ""), "\tFile\tArchivo\n");
    }
}
