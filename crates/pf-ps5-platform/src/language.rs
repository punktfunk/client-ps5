//! The console's language setting as a BCP-47 tag. Pure, so it tests anywhere.

/// The tag for a `SCE_SYSTEM_PARAM_LANG_*` value, or `None` for one this table does not know.
/// The messages pick the closest language they ship from it (`pt-BR` reads `pt` when that is all
/// there is).
pub fn tag(code: i32) -> Option<&'static str> {
    Some(match code {
        0 => "ja",
        1 => "en-US",
        2 => "fr",
        3 => "es-ES",
        4 => "de",
        5 => "it",
        6 => "nl",
        7 => "pt-PT",
        8 => "ru",
        9 => "ko",
        10 => "zh-Hant",
        11 => "zh-Hans",
        12 => "fi",
        13 => "sv",
        14 => "da",
        15 => "nb",
        16 => "pl",
        17 => "pt-BR",
        18 => "en-GB",
        19 => "tr",
        20 => "es-419",
        21 => "ar",
        22 => "fr-CA",
        23 => "cs",
        24 => "hu",
        25 => "el",
        26 => "ro",
        27 => "th",
        28 => "vi",
        29 => "id",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::tag;

    #[test]
    fn the_console_languages_map_to_tags() {
        assert_eq!(tag(1), Some("en-US"));
        assert_eq!(tag(4), Some("de"));
        assert_eq!(tag(17), Some("pt-BR"));
        assert_eq!(tag(29), Some("id"));
    }

    #[test]
    fn an_unknown_value_is_no_language() {
        assert_eq!(tag(-1), None);
        assert_eq!(tag(30), None);
    }
}
