use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app_error::AppCommandError;
use crate::commands::confined_file::{
    read_confined_regular_file, ConfinedRead, FILE_BASE64_DEFAULT_MAX_BYTES,
};

pub(crate) const GROK_IMAGE_MAX_BYTES: usize = FILE_BASE64_DEFAULT_MAX_BYTES;
pub(crate) const GROK_IMAGE_MAX_PIXELS: u64 = 40_000_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveGrokSessionImageRequest {
    pub conversation_id: i32,
    pub href: String,
    #[serde(default)]
    pub include_data: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GrokSessionImageOrigin {
    Session,
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveGrokSessionImageResponse {
    pub path: String,
    pub origin: GrokSessionImageOrigin,
    pub mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
}

#[derive(Debug)]
enum CandidateOutcome {
    Found(ResolvedCandidate),
    Absent,
    NotReady,
}

#[derive(Debug)]
struct ResolvedCandidate {
    canonical_path: PathBuf,
    mime_type: &'static str,
    bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RasterHeaderOutcome {
    Ready,
    NotReady,
}

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

fn inspect_raster_header<R: std::io::BufRead + std::io::Seek>(
    mut reader: R,
    image_ref: &GrokSessionImageRef,
) -> Result<RasterHeaderOutcome, AppCommandError> {
    use std::io::SeekFrom;

    reader
        .seek(SeekFrom::Start(0))
        .map_err(AppCommandError::io)?;
    let mut prefix = [0_u8; 32];
    let prefix_len = reader.read(&mut prefix).map_err(AppCommandError::io)?;
    let sniffed = match image::guess_format(&prefix[..prefix_len]) {
        Ok(format) => format,
        Err(_) => return Ok(RasterHeaderOutcome::NotReady),
    };
    if !matches!(
        sniffed,
        image::ImageFormat::Png
            | image::ImageFormat::Jpeg
            | image::ImageFormat::WebP
            | image::ImageFormat::Gif
    ) {
        return Err(AppCommandError::invalid_input(
            "Unsupported raster image header",
        ));
    }
    if sniffed != image_ref.format.image_format() {
        return Err(AppCommandError::invalid_input(
            "Image header does not match its filename extension",
        ));
    }

    reader
        .seek(SeekFrom::Start(0))
        .map_err(AppCommandError::io)?;
    let (width, height) = match image::ImageReader::with_format(reader, sniffed).into_dimensions() {
        Ok(dimensions) => dimensions,
        Err(_) => return Ok(RasterHeaderOutcome::NotReady),
    };
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| AppCommandError::invalid_input("Image dimensions overflow"))?;
    if pixels > GROK_IMAGE_MAX_PIXELS {
        return Err(AppCommandError::invalid_input(
            "Image dimensions are too large",
        ));
    }
    Ok(RasterHeaderOutcome::Ready)
}

fn inspect_image_candidate(
    authority_root: &Path,
    image_ref: &GrokSessionImageRef,
    include_data: bool,
) -> Result<CandidateOutcome, AppCommandError> {
    use std::io::{BufReader, Cursor};

    let confined = read_confined_regular_file(
        authority_root,
        Path::new(&image_ref.path),
        Some(Path::new("images")),
        GROK_IMAGE_MAX_BYTES,
        include_data,
    )?;
    let ConfinedRead::Found(mut found) = confined else {
        return Ok(CandidateOutcome::Absent);
    };
    if found.metadata.len() == 0 {
        return Ok(CandidateOutcome::NotReady);
    }

    let header = if include_data {
        let bytes = found.bytes.as_deref().ok_or_else(|| {
            AppCommandError::task_execution_failed("Captured image data was unavailable")
        })?;
        inspect_raster_header(Cursor::new(bytes), image_ref)?
    } else {
        inspect_raster_header(BufReader::new(&mut found.file), image_ref)?
    };
    if header == RasterHeaderOutcome::NotReady {
        return Ok(CandidateOutcome::NotReady);
    }

    let bytes = if include_data {
        let bytes = found.bytes.take().ok_or_else(|| {
            AppCommandError::task_execution_failed("Captured image data was unavailable")
        })?;
        if bytes.is_empty() {
            return Ok(CandidateOutcome::NotReady);
        }
        Some(bytes)
    } else {
        None
    };
    Ok(CandidateOutcome::Found(ResolvedCandidate {
        canonical_path: found.canonical_path,
        mime_type: image_ref.format.extension_mime(),
        bytes,
    }))
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

#[cfg(test)]
mod candidate_tests {
    use super::*;
    use crate::app_error::AppErrorCode;
    use std::io::Cursor;

    fn crc32_ieee(bytes: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }

    pub(super) fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        let chunks: [(&[u8], &[u8]); 2] = [(b"IHDR", ihdr.as_slice()), (b"IDAT", &[])];
        for (kind, data) in chunks {
            bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let crc_start = bytes.len();
            bytes.extend_from_slice(kind);
            bytes.extend_from_slice(data);
            bytes.extend_from_slice(&crc32_ieee(&bytes[crc_start..]).to_be_bytes());
        }
        bytes
    }

    fn encoded_image(format: image::ImageFormat) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgba8(2, 3)
            .write_to(Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }

    fn write_candidate(root: &Path, name: &str, bytes: &[u8]) -> GrokSessionImageRef {
        std::fs::create_dir_all(root.join("images")).unwrap();
        std::fs::write(root.join("images").join(name), bytes).unwrap();
        parse_grok_session_image_ref(&format!("images/{name}")).unwrap()
    }

    fn assert_found_with_raw_bytes(
        extension: &str,
        expected_mime: &str,
        bytes: &[u8],
    ) -> ResolvedCandidate {
        let root = tempfile::tempdir().unwrap();
        let name = format!("a.{extension}");
        let image_ref = write_candidate(root.path(), &name, bytes);
        let CandidateOutcome::Found(found) =
            inspect_image_candidate(root.path(), &image_ref, true).unwrap()
        else {
            panic!("expected found")
        };
        assert_eq!(found.mime_type, expected_mime);
        assert_eq!(found.bytes.as_deref(), Some(bytes));
        found
    }

    #[test]
    fn response_serializes_exact_camel_case_and_omits_absent_data() {
        let without = ResolveGrokSessionImageResponse {
            path: "/tmp/images/a.png".into(),
            origin: GrokSessionImageOrigin::Session,
            mime_type: "image/png".into(),
            data_base64: None,
        };
        assert_eq!(
            serde_json::to_value(without).unwrap(),
            serde_json::json!({
                "path": "/tmp/images/a.png",
                "origin": "session",
                "mimeType": "image/png"
            })
        );

        let with = ResolveGrokSessionImageResponse {
            path: "/tmp/images/a.png".into(),
            origin: GrokSessionImageOrigin::Workspace,
            mime_type: "image/png".into(),
            data_base64: Some("YWJj".into()),
        };
        assert_eq!(serde_json::to_value(with).unwrap()["dataBase64"], "YWJj");
    }

    #[test]
    fn request_deserializes_camel_case_and_defaults_include_data_false() {
        let request: ResolveGrokSessionImageRequest = serde_json::from_value(serde_json::json!({
            "conversationId": 42,
            "href": "images/a.png"
        }))
        .unwrap();
        assert_eq!(request.conversation_id, 42);
        assert_eq!(request.href, "images/a.png");
        assert!(!request.include_data);
        assert!(
            serde_json::from_value::<ResolveGrokSessionImageRequest>(serde_json::json!({
                "conversation_id": 42,
                "href": "images/a.png"
            }))
            .is_err()
        );
    }

    #[test]
    fn valid_png_header_returns_same_handle_bytes_only_when_requested() {
        let root = tempfile::tempdir().unwrap();
        let bytes = png_header(2, 3);
        let image_ref = write_candidate(root.path(), "a.PNG", &bytes);
        let CandidateOutcome::Found(with_data) =
            inspect_image_candidate(root.path(), &image_ref, true).unwrap()
        else {
            panic!("expected found")
        };
        assert_eq!(with_data.mime_type, "image/png");
        assert_eq!(with_data.bytes.as_deref(), Some(bytes.as_slice()));

        let CandidateOutcome::Found(without_data) =
            inspect_image_candidate(root.path(), &image_ref, false).unwrap()
        else {
            panic!("expected found")
        };
        assert_eq!(without_data.mime_type, "image/png");
        assert!(without_data.bytes.is_none());
    }

    #[test]
    fn jpeg_header_maps_jpg_and_jpeg_to_image_jpeg() {
        let bytes = encoded_image(image::ImageFormat::Jpeg);
        assert_found_with_raw_bytes("jpg", "image/jpeg", &bytes);
        assert_found_with_raw_bytes("jpeg", "image/jpeg", &bytes);
    }

    #[test]
    fn webp_header_maps_to_image_webp() {
        let bytes = encoded_image(image::ImageFormat::WebP);
        assert_found_with_raw_bytes("webp", "image/webp", &bytes);
    }

    #[test]
    fn gif87a_and_gif89a_map_to_image_gif() {
        let encoded = encoded_image(image::ImageFormat::Gif);
        for signature in [b"GIF87a", b"GIF89a"] {
            let mut bytes = encoded.clone();
            bytes[..6].copy_from_slice(signature);
            assert_found_with_raw_bytes("gif", "image/gif", &bytes);
        }
    }

    #[test]
    fn recognized_bmp_header_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let mut bytes = vec![0_u8; 54];
        bytes[..2].copy_from_slice(b"BM");
        bytes[2..6].copy_from_slice(&54_u32.to_le_bytes());
        bytes[10..14].copy_from_slice(&54_u32.to_le_bytes());
        bytes[14..18].copy_from_slice(&40_u32.to_le_bytes());
        bytes[18..22].copy_from_slice(&1_i32.to_le_bytes());
        bytes[22..26].copy_from_slice(&1_i32.to_le_bytes());
        bytes[26..28].copy_from_slice(&1_u16.to_le_bytes());
        bytes[28..30].copy_from_slice(&24_u16.to_le_bytes());
        let image_ref = write_candidate(root.path(), "a.png", &bytes);
        let error = inspect_image_candidate(root.path(), &image_ref, false).unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);
    }

    #[test]
    fn supported_header_mismatch_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let image_ref = write_candidate(root.path(), "a.jpg", &png_header(2, 3));
        let error = inspect_image_candidate(root.path(), &image_ref, false).unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);
    }

    #[test]
    fn empty_short_and_truncated_allowed_headers_are_not_ready() {
        for (name, bytes) in [
            ("empty.png", Vec::new()),
            ("short.png", b"\x89PNG".to_vec()),
            ("truncated.png", b"\x89PNG\r\n\x1a\n\0\0".to_vec()),
        ] {
            let root = tempfile::tempdir().unwrap();
            let image_ref = write_candidate(root.path(), name, &bytes);
            assert!(matches!(
                inspect_image_candidate(root.path(), &image_ref, false).unwrap(),
                CandidateOutcome::NotReady
            ));
        }
    }

    #[test]
    fn exact_pixel_boundary_passes_and_one_pixel_over_rejects() {
        let root = tempfile::tempdir().unwrap();
        let exact = write_candidate(root.path(), "exact.png", &png_header(8_000, 5_000));
        assert!(matches!(
            inspect_image_candidate(root.path(), &exact, false).unwrap(),
            CandidateOutcome::Found(_)
        ));
        let over = write_candidate(root.path(), "over.png", &png_header(40_000_001, 1));
        let error = inspect_image_candidate(root.path(), &over, false).unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);
    }

    #[test]
    fn exact_byte_boundary_passes_and_one_byte_over_rejects() {
        let root = tempfile::tempdir().unwrap();
        let mut exact_bytes = png_header(1, 1);
        exact_bytes.resize(GROK_IMAGE_MAX_BYTES, 0);
        let exact = write_candidate(root.path(), "exact.png", &exact_bytes);
        assert!(matches!(
            inspect_image_candidate(root.path(), &exact, false).unwrap(),
            CandidateOutcome::Found(_)
        ));

        let mut over_bytes = png_header(1, 1);
        over_bytes.resize(GROK_IMAGE_MAX_BYTES + 1, 0);
        let over = write_candidate(root.path(), "over.png", &over_bytes);
        let error = inspect_image_candidate(root.path(), &over, false).unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);
        assert_eq!(error.detail.as_deref(), Some("max_bytes=20000000"));
    }

    #[test]
    fn missing_candidate_is_absent() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("images")).unwrap();
        let image_ref = parse_grok_session_image_ref("images/missing.png").unwrap();
        assert!(matches!(
            inspect_image_candidate(root.path(), &image_ref, false).unwrap(),
            CandidateOutcome::Absent
        ));
    }

    #[test]
    fn directory_candidate_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("images/a.png")).unwrap();
        let image_ref = parse_grok_session_image_ref("images/a.png").unwrap();
        let error = inspect_image_candidate(root.path(), &image_ref, false).unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);
    }

    #[test]
    fn captured_bytes_are_the_header_validation_source() {
        let root = tempfile::tempdir().unwrap();
        let captured = png_header(2, 3);
        let image_ref = write_candidate(root.path(), "a.png", &captured);
        std::fs::write(
            root.path().join("images/a.png"),
            encoded_image(image::ImageFormat::Jpeg),
        )
        .unwrap();
        assert_eq!(
            inspect_raster_header(Cursor::new(&captured), &image_ref).unwrap(),
            RasterHeaderOutcome::Ready
        );
    }
}
