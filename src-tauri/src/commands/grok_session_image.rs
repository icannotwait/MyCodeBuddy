use crate::app_error::AppCommandError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrokImageFormat {
    Png,
    Jpeg,
    WebP,
    Gif,
}

impl GrokImageFormat {
    pub(crate) fn extension_mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::WebP => "image/webp",
            Self::Gif => "image/gif",
        }
    }

    pub(crate) fn image_format(self) -> image::ImageFormat {
        match self {
            Self::Png => image::ImageFormat::Png,
            Self::Jpeg => image::ImageFormat::Jpeg,
            Self::WebP => image::ImageFormat::WebP,
            Self::Gif => image::ImageFormat::Gif,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrokSessionImageRef {
    pub path: String,
    pub filename: String,
    pub extension: String,
    pub format: GrokImageFormat,
}

const INVALID_REF_MESSAGE: &str = "Invalid Grok session image reference";

fn invalid_ref() -> AppCommandError {
    AppCommandError::invalid_input(INVALID_REF_MESSAGE)
}

fn has_only_complete_percent_escapes(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len()
            || !bytes[index + 1].is_ascii_hexdigit()
            || !bytes[index + 2].is_ascii_hexdigit()
        {
            return false;
        }
        index += 3;
    }
    true
}

fn contains_control(value: &str) -> bool {
    value.chars().any(|character| {
        let code = character as u32;
        code <= 0x1f || code == 0x7f
    })
}

fn format_for_extension(extension: &str) -> Option<GrokImageFormat> {
    match extension {
        "png" => Some(GrokImageFormat::Png),
        "jpg" | "jpeg" => Some(GrokImageFormat::Jpeg),
        "webp" => Some(GrokImageFormat::WebP),
        "gif" => Some(GrokImageFormat::Gif),
        _ => None,
    }
}

fn is_windows_device_stem(value: &str) -> bool {
    matches!(
        value.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn has_portable_invalid_character(value: &str) -> bool {
    value
        .chars()
        .any(|character| matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '#'))
}

pub(crate) fn parse_grok_session_image_ref(
    raw: &str,
) -> Result<GrokSessionImageRef, AppCommandError> {
    if raw.len() > 1024 || contains_control(raw) {
        return Err(invalid_ref());
    }

    let trimmed = raw.trim_matches(' ');
    if trimmed.is_empty() {
        return Err(invalid_ref());
    }

    let path_part = trimmed
        .find(['?', '#'])
        .map_or(trimmed, |index| &trimmed[..index]);
    let decoded = if has_only_complete_percent_escapes(path_part) {
        urlencoding::decode(path_part)
            .ok()
            .map(|value| value.into_owned())
    } else {
        None
    };
    let Some(decoded) = decoded else {
        return Err(invalid_ref());
    };

    if decoded.is_empty()
        || decoded.contains('\\')
        || decoded.split('/').any(|component| component == "..")
    {
        return Err(invalid_ref());
    }

    let filename = if let Some(filename) = decoded.strip_prefix("images/") {
        filename
    } else if let Some(filename) = decoded.strip_prefix("./images/") {
        filename
    } else {
        return Err(invalid_ref());
    };

    if filename.is_empty()
        || filename.contains('/')
        || filename.starts_with(' ')
        || filename.ends_with(' ')
        || contains_control(filename)
        || has_portable_invalid_character(filename)
        || filename.len() > 255
    {
        return Err(invalid_ref());
    }

    let Some(last_dot) = filename.rfind('.') else {
        return Err(invalid_ref());
    };
    if last_dot == 0 || last_dot + 1 == filename.len() {
        return Err(invalid_ref());
    }

    let first_dot = filename.find('.').expect("last dot proves a dot exists");
    if is_windows_device_stem(&filename[..first_dot]) {
        return Err(invalid_ref());
    }

    let extension = filename[last_dot + 1..].to_ascii_lowercase();
    let Some(format) = format_for_extension(&extension) else {
        return Err(invalid_ref());
    };

    Ok(GrokSessionImageRef {
        path: format!("images/{filename}"),
        filename: filename.to_owned(),
        extension,
        format,
    })
}

#[cfg(test)]
mod href_parser_tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Cases {
        accepted: Vec<Accepted>,
        rejected: Vec<Rejected>,
    }

    #[derive(Deserialize)]
    struct Accepted {
        input: String,
        expected: Expected,
    }

    #[derive(Deserialize)]
    struct Expected {
        path: String,
        filename: String,
        extension: String,
    }

    #[derive(Deserialize)]
    struct Rejected {
        input: String,
        reason: String,
    }

    fn cases() -> Cases {
        serde_json::from_str(include_str!(
            "../../../fixtures/grok-session-image-href-cases.json"
        ))
        .expect("valid shared Grok image fixture")
    }

    #[test]
    fn shared_accepted_cases_canonicalize_identically() {
        for case in cases().accepted {
            let actual = parse_grok_session_image_ref(&case.input)
                .unwrap_or_else(|error| panic!("{}: {error:?}", case.input));
            assert_eq!(actual.path, case.expected.path, "{}", case.input);
            assert_eq!(actual.filename, case.expected.filename, "{}", case.input);
            assert_eq!(actual.extension, case.expected.extension, "{}", case.input);
        }
    }

    #[test]
    fn shared_rejected_cases_all_fail_invalid_input() {
        for case in cases().rejected {
            let error = parse_grok_session_image_ref(&case.input).unwrap_err();
            assert_eq!(
                error.code,
                crate::app_error::AppErrorCode::InvalidInput,
                "{} should reject",
                case.reason,
            );
        }
    }

    #[test]
    fn generated_byte_boundaries_match_typescript() {
        let suffix = "images/a.png";
        let pass = format!("{}{}", " ".repeat(1024 - suffix.len()), suffix);
        let fail = format!("{}{}", " ".repeat(1025 - suffix.len()), suffix);
        assert!(parse_grok_session_image_ref(&pass).is_ok());
        assert!(parse_grok_session_image_ref(&fail).is_err());

        let pass_name = format!("{}.png", "a".repeat(251));
        let fail_name = format!("{}.png", "a".repeat(252));
        assert!(parse_grok_session_image_ref(&format!("images/{pass_name}")).is_ok());
        assert!(parse_grok_session_image_ref(&format!("images/{fail_name}")).is_err());

        let utf8_pass = format!("{}.png", "界".repeat(83));
        let utf8_fail = format!("{}.png", "界".repeat(84));
        assert_eq!(utf8_pass.len(), 253);
        assert_eq!(utf8_fail.len(), 256);
        assert!(parse_grok_session_image_ref(&format!("images/{utf8_pass}")).is_ok());
        assert!(parse_grok_session_image_ref(&format!("images/{utf8_fail}")).is_err());
    }

    #[test]
    fn image_formats_own_raster_decoder_and_mime_mappings() {
        assert_eq!(GrokImageFormat::Png.extension_mime(), "image/png");
        assert_eq!(GrokImageFormat::Png.image_format(), image::ImageFormat::Png);
        assert_eq!(GrokImageFormat::Jpeg.extension_mime(), "image/jpeg");
        assert_eq!(
            GrokImageFormat::Jpeg.image_format(),
            image::ImageFormat::Jpeg
        );
        assert_eq!(GrokImageFormat::WebP.extension_mime(), "image/webp");
        assert_eq!(
            GrokImageFormat::WebP.image_format(),
            image::ImageFormat::WebP
        );
        assert_eq!(GrokImageFormat::Gif.extension_mime(), "image/gif");
        assert_eq!(GrokImageFormat::Gif.image_format(), image::ImageFormat::Gif);
    }
}
