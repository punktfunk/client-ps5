//! The console's own fonts for what Geist lacks: Han, kana and Hangul. Pure, so it tests anywhere.

use std::path::{Path, PathBuf};

/// Kana and JIS kanji (`SST-JPPRO-R`).
const JAPANESE: &str = "SSTJpPro-Regular.otf";
/// Simplified Chinese Hei (`SST-CNGB-HEI-R`).
const CHINESE: &str = "DFHEI5-SONY.ttf";
/// Hangul only (`SCE-EXT-HANGUL-R`).
const KOREAN: &str = "SCEPS4Yoongd-Medium.otf";

/// The system font directory under the sandbox's word, else where it lives outside a sandbox.
pub fn dirs(word: Option<&str>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = word
        .map(|w| Path::new("/").join(w).join("common/font"))
        .into_iter()
        .collect();
    dirs.push(PathBuf::from("/preinst/common/font"));
    dirs
}

/// The CJK faces in `dir`, the system language's own first. The kit draws a character from the
/// first face that has it, so a Han character takes that region's shape; Japanese leads otherwise.
pub fn faces(dir: &Path, language: Option<&str>) -> Vec<PathBuf> {
    let lang = language.unwrap_or_default();
    let order = if lang.starts_with("zh") {
        [CHINESE, JAPANESE, KOREAN]
    } else if lang.starts_with("ko") {
        [KOREAN, JAPANESE, CHINESE]
    } else {
        [JAPANESE, CHINESE, KOREAN]
    };
    order.iter().map(|name| dir.join(name)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(language: Option<&str>) -> Vec<String> {
        faces(Path::new("/f"), language)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn the_system_language_leads() {
        assert_eq!(names(Some("ja")), [JAPANESE, CHINESE, KOREAN]);
        assert_eq!(names(Some("zh-Hans")), [CHINESE, JAPANESE, KOREAN]);
        assert_eq!(names(Some("zh-Hant")), [CHINESE, JAPANESE, KOREAN]);
        assert_eq!(names(Some("ko")), [KOREAN, JAPANESE, CHINESE]);
    }

    #[test]
    fn any_other_language_reads_japanese_first() {
        assert_eq!(names(Some("en-US")), [JAPANESE, CHINESE, KOREAN]);
        assert_eq!(names(None), [JAPANESE, CHINESE, KOREAN]);
    }

    #[test]
    fn the_sandbox_directory_comes_first() {
        assert_eq!(
            dirs(Some("abcdefghij")),
            [
                PathBuf::from("/abcdefghij/common/font"),
                PathBuf::from("/preinst/common/font")
            ]
        );
        assert_eq!(dirs(None), [PathBuf::from("/preinst/common/font")]);
    }
}
