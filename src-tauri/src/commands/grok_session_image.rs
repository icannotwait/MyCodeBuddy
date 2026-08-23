use std::path::{Path, PathBuf};

use base64::Engine as _;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};

use crate::app_error::{AppCommandError, AppErrorCode};
#[cfg(feature = "tauri-runtime")]
use crate::app_state::AppState;
use crate::commands::confined_file::{
    has_dangling_alias_component, metadata_is_symlink_or_reparse, read_confined_regular_file,
    run_file_io, ConfinedRead, FILE_BASE64_DEFAULT_MAX_BYTES,
};
use crate::db::entities::{conversation, folder};
use crate::db::AppDatabase;
use crate::models::agent::AgentType;
#[cfg(feature = "tauri-runtime")]
use crate::parsers::grok::resolve_grok_home_dir;
use crate::parsers::grok::{locate_grok_session_dir, GrokSessionLocatorError};

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
        Err(image::ImageError::IoError(error))
            if error.kind() != std::io::ErrorKind::UnexpectedEof =>
        {
            return Err(AppCommandError::io(error));
        }
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

const GROK_IMAGE_SOURCE_NOT_FOUND: &str = "Grok session image source was not found";

fn source_not_found() -> AppCommandError {
    AppCommandError::not_found(GROK_IMAGE_SOURCE_NOT_FOUND)
}

fn validate_external_session_id(value: &str) -> Result<(), AppCommandError> {
    let bytes = value.as_bytes();
    let valid_shape = (1..=255).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    let stem = value.split('.').next().unwrap_or(value);
    if !valid_shape
        || matches!(value, "." | "..")
        || value.ends_with('.')
        || is_windows_device_stem(stem)
    {
        return Err(AppCommandError::invalid_input(
            "Invalid Grok external session id",
        ));
    }
    Ok(())
}

fn invalid_session_authority() -> AppCommandError {
    AppCommandError::invalid_input("Invalid Grok session authority")
}

fn map_session_authority_io(error: std::io::Error) -> AppCommandError {
    if error.kind() == std::io::ErrorKind::NotFound {
        return AppCommandError::new(
            AppErrorCode::IoError,
            "Grok session authority changed during resolution",
        )
        .with_detail(error.to_string());
    }
    AppCommandError::io(error)
}

fn validate_session_authority(
    sessions_root: &Path,
    session_dir: &Path,
    external_id: &str,
) -> Result<PathBuf, AppCommandError> {
    use std::ffi::OsStr;
    use std::path::Component;

    let canonical_root = std::fs::canonicalize(sessions_root).map_err(map_session_authority_io)?;
    if !std::fs::metadata(&canonical_root)
        .map_err(map_session_authority_io)?
        .is_dir()
    {
        return Err(invalid_session_authority());
    }
    let relative = session_dir
        .strip_prefix(sessions_root)
        .map_err(|_| invalid_session_authority())?;
    let mut components = relative.components();
    let Some(Component::Normal(group)) = components.next() else {
        return Err(invalid_session_authority());
    };
    let Some(Component::Normal(session)) = components.next() else {
        return Err(invalid_session_authority());
    };
    if components.next().is_some() || session != OsStr::new(external_id) {
        return Err(invalid_session_authority());
    }

    let lexical_group = sessions_root.join(group);
    for path in [&lexical_group, session_dir] {
        let metadata = std::fs::symlink_metadata(path).map_err(map_session_authority_io)?;
        if metadata_is_symlink_or_reparse(&metadata) || !metadata.is_dir() {
            return Err(invalid_session_authority());
        }
    }

    let canonical_session = std::fs::canonicalize(session_dir).map_err(map_session_authority_io)?;
    let canonical_relative = canonical_session
        .strip_prefix(&canonical_root)
        .map_err(|_| invalid_session_authority())?;
    if canonical_relative.components().count() != 2 {
        return Err(invalid_session_authority());
    }
    Ok(canonical_session)
}

fn inspect_workspace_root(path: &Path) -> Result<Option<PathBuf>, AppCommandError> {
    if !path.is_absolute() {
        return Err(AppCommandError::invalid_input(
            "Workspace root must be absolute",
        ));
    }
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(Some(path.to_path_buf())),
        Ok(_) => Err(AppCommandError::invalid_input(
            "Workspace root must be a directory",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if has_dangling_alias_component(path).map_err(AppCommandError::io)? {
                Err(AppCommandError::invalid_input("Workspace root is dangling"))
            } else {
                Ok(None)
            }
        }
        Err(error) => Err(AppCommandError::io(error)),
    }
}

async fn select_workspace_root(
    db: &AppDatabase,
    conversation: &conversation::Model,
) -> Result<Option<PathBuf>, AppCommandError> {
    if let Some(origin_cwd) = conversation
        .origin_cwd
        .as_deref()
        .filter(|origin_cwd| !origin_cwd.is_empty())
    {
        let origin_cwd = PathBuf::from(origin_cwd);
        if let Some(root) = run_file_io(move || inspect_workspace_root(&origin_cwd)).await? {
            return Ok(Some(root));
        }
    }

    let folder = folder::Entity::find_by_id(conversation.folder_id)
        .filter(folder::Column::DeletedAt.is_null())
        .one(&db.conn)
        .await
        .map_err(|error| AppCommandError::db(error.into()))?;
    let Some(folder) = folder else {
        return Ok(None);
    };
    let folder_path = PathBuf::from(folder.path);
    run_file_io(move || inspect_workspace_root(&folder_path)).await
}

fn response_from_candidate(
    candidate: ResolvedCandidate,
    origin: GrokSessionImageOrigin,
    include_data: bool,
) -> Result<ResolveGrokSessionImageResponse, AppCommandError> {
    let ResolvedCandidate {
        canonical_path,
        mime_type,
        bytes,
    } = candidate;
    let simplified_path = crate::paths::simplify_verbatim_path(&canonical_path);
    let path = simplified_path
        .to_str()
        .ok_or_else(|| AppCommandError::invalid_input("Resolved image path is not valid UTF-8"))?
        .to_owned();
    #[cfg(windows)]
    if path.starts_with(r"\\?\") {
        return Err(AppCommandError::invalid_input(
            "Resolved image path cannot be represented by the frontend",
        ));
    }
    let data_base64 = match (include_data, bytes) {
        (true, Some(bytes)) if !bytes.is_empty() => {
            Some(base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        (false, None) => None,
        _ => {
            return Err(AppCommandError::task_execution_failed(
                "Resolved image byte invariant was violated",
            ));
        }
    };
    Ok(ResolveGrokSessionImageResponse {
        path,
        origin,
        mime_type: mime_type.to_owned(),
        data_base64,
    })
}

pub async fn resolve_grok_session_image_core(
    db: &AppDatabase,
    sessions_root: PathBuf,
    request: ResolveGrokSessionImageRequest,
) -> Result<ResolveGrokSessionImageResponse, AppCommandError> {
    if request.conversation_id <= 0 {
        return Err(AppCommandError::invalid_input(
            "Conversation id must be positive",
        ));
    }

    let conversation = conversation::Entity::find_by_id(request.conversation_id)
        .filter(conversation::Column::DeletedAt.is_null())
        .one(&db.conn)
        .await
        .map_err(|error| AppCommandError::db(error.into()))?;
    let Some(conversation) = conversation else {
        return Err(source_not_found());
    };
    if conversation.agent_type != AgentType::Grok.as_wire().as_ref() {
        return Err(source_not_found());
    }
    let Some(external_id) = conversation
        .external_id
        .as_deref()
        .filter(|external_id| !external_id.is_empty())
    else {
        return Err(source_not_found());
    };
    validate_external_session_id(external_id)?;
    let image_ref = parse_grok_session_image_ref(&request.href)?;

    let session_external_id = external_id.to_owned();
    let session_image_ref = image_ref.clone();
    let session_root_for_io = sessions_root.clone();
    let include_data = request.include_data;
    let session_outcome = run_file_io(move || {
        let session_dir = match locate_grok_session_dir(&session_root_for_io, &session_external_id)
        {
            Ok(Some(session_dir)) => session_dir,
            Ok(None) => return Ok(CandidateOutcome::Absent),
            Err(GrokSessionLocatorError::Ambiguous { .. }) => {
                return Err(AppCommandError::invalid_input(
                    "Ambiguous Grok session image source",
                ));
            }
            Err(GrokSessionLocatorError::Io(error)) => {
                return Err(map_session_authority_io(error));
            }
        };
        let canonical_session =
            validate_session_authority(&session_root_for_io, &session_dir, &session_external_id)?;
        inspect_image_candidate(&canonical_session, &session_image_ref, include_data)
    })
    .await?;

    if let CandidateOutcome::Found(candidate) = session_outcome {
        return response_from_candidate(
            candidate,
            GrokSessionImageOrigin::Session,
            request.include_data,
        );
    }

    let Some(workspace_root) = select_workspace_root(db, &conversation).await? else {
        return Err(source_not_found());
    };
    let workspace_outcome =
        run_file_io(move || inspect_image_candidate(&workspace_root, &image_ref, include_data))
            .await?;
    match workspace_outcome {
        CandidateOutcome::Found(candidate) => response_from_candidate(
            candidate,
            GrokSessionImageOrigin::Workspace,
            request.include_data,
        ),
        CandidateOutcome::Absent | CandidateOutcome::NotReady => Err(source_not_found()),
    }
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn resolve_grok_session_image(
    state: tauri::State<'_, AppState>,
    conversation_id: i32,
    href: String,
    include_data: Option<bool>,
) -> Result<ResolveGrokSessionImageResponse, AppCommandError> {
    resolve_grok_session_image_core(
        &state.db,
        resolve_grok_home_dir().join("sessions"),
        ResolveGrokSessionImageRequest {
            conversation_id,
            href,
            include_data: include_data.unwrap_or(false),
        },
    )
    .await
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
    use std::io::{BufRead, Cursor, Read, Seek, SeekFrom};

    struct FailsDuringDimensionRead {
        cursor: Cursor<Vec<u8>>,
        start_seek_count: usize,
    }

    impl FailsDuringDimensionRead {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                cursor: Cursor::new(bytes),
                start_seek_count: 0,
            }
        }

        fn read_error(&self) -> std::io::Error {
            std::io::Error::other("simulated raster read failure")
        }
    }

    impl Read for FailsDuringDimensionRead {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.start_seek_count >= 2 {
                return Err(self.read_error());
            }
            self.cursor.read(buffer)
        }
    }

    impl BufRead for FailsDuringDimensionRead {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            if self.start_seek_count >= 2 {
                return Err(self.read_error());
            }
            self.cursor.fill_buf()
        }

        fn consume(&mut self, amount: usize) {
            self.cursor.consume(amount);
        }
    }

    impl Seek for FailsDuringDimensionRead {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            if position == SeekFrom::Start(0) {
                self.start_seek_count += 1;
            }
            self.cursor.seek(position)
        }
    }

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
    fn non_eof_dimension_read_error_is_terminal_io_error() {
        let image_ref = parse_grok_session_image_ref("images/a.png").unwrap();
        let reader = FailsDuringDimensionRead::new(png_header(2, 3));

        let error = inspect_raster_header(reader, &image_ref).unwrap_err();

        assert_eq!(error.code, AppErrorCode::IoError);
        assert_eq!(
            error.detail.as_deref(),
            Some("simulated raster read failure")
        );
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

#[cfg(test)]
mod resolver_tests {
    use super::candidate_tests::png_header;
    use super::*;
    use crate::app_error::AppErrorCode;
    use crate::db::entities::{conversation, folder};
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::db::AppDatabase;
    use crate::models::agent::AgentType;
    #[allow(unused_imports)]
    use base64::Engine as _;
    #[allow(unused_imports)]
    use sea_orm::{
        ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter,
        Set,
    };
    use std::path::{Path, PathBuf};

    const SOURCE_NOT_FOUND: &str = "Grok session image source was not found";

    struct ResolverFixture {
        db: AppDatabase,
        _temp: tempfile::TempDir,
        sessions_root: PathBuf,
        workspace_root: PathBuf,
        session_root: PathBuf,
        conversation_id: i32,
    }

    impl ResolverFixture {
        async fn new() -> Self {
            let title_key_guard = crate::auto_title::title_key::test_hooks::SuiteGuard::enter();
            crate::auto_title::title_key::test_hooks::push_override_get(
                crate::auto_title::title_key::TitleKeyState::Absent,
            );
            let temp = tempfile::tempdir().unwrap();
            let sessions_root = temp.path().join("grok/sessions");
            let workspace_root = temp.path().join("workspace");
            let external_id = "session-123".to_string();
            let session_root = sessions_root.join("group-a").join(&external_id);
            std::fs::create_dir_all(&session_root).unwrap();
            std::fs::write(session_root.join("updates.jsonl"), b"\n").unwrap();
            std::fs::create_dir_all(&workspace_root).unwrap();

            let db = fresh_in_memory_db().await;
            let folder_id = seed_folder(&db, workspace_root.to_str().unwrap()).await;
            let conversation_id = seed_conversation(&db, folder_id, AgentType::Grok).await;
            drop(title_key_guard);
            let row = conversation::Entity::find_by_id(conversation_id)
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
            let mut active = row.into_active_model();
            active.external_id = Set(Some(external_id.clone()));
            active.update(&db.conn).await.unwrap();

            Self {
                db,
                _temp: temp,
                sessions_root,
                workspace_root,
                session_root,
                conversation_id,
            }
        }

        fn request(&self, include_data: bool) -> ResolveGrokSessionImageRequest {
            ResolveGrokSessionImageRequest {
                conversation_id: self.conversation_id,
                href: "images/a.png".into(),
                include_data,
            }
        }

        fn put_png(&self, root: &Path, width: u32, height: u32) -> Vec<u8> {
            let bytes = png_header(width, height);
            std::fs::create_dir_all(root.join("images")).unwrap();
            std::fs::write(root.join("images/a.png"), &bytes).unwrap();
            bytes
        }

        fn put_bytes(&self, root: &Path, name: &str, bytes: &[u8]) {
            std::fs::create_dir_all(root.join("images")).unwrap();
            std::fs::write(root.join("images").join(name), bytes).unwrap();
        }

        async fn conversation(&self) -> conversation::Model {
            conversation::Entity::find_by_id(self.conversation_id)
                .one(&self.db.conn)
                .await
                .unwrap()
                .unwrap()
        }

        async fn set_external_id(&self, value: Option<String>) {
            let mut active = self.conversation().await.into_active_model();
            active.external_id = Set(value);
            active.update(&self.db.conn).await.unwrap();
        }

        async fn set_origin_cwd(&self, value: Option<String>) {
            let mut active = self.conversation().await.into_active_model();
            active.origin_cwd = Set(value);
            active.update(&self.db.conn).await.unwrap();
        }

        async fn set_agent_type(&self, value: &str) {
            let mut active = self.conversation().await.into_active_model();
            active.agent_type = Set(value.to_owned());
            active.update(&self.db.conn).await.unwrap();
        }

        async fn delete_conversation(&self) {
            let mut active = self.conversation().await.into_active_model();
            active.deleted_at = Set(Some(chrono::Utc::now()));
            active.update(&self.db.conn).await.unwrap();
        }

        async fn folder(&self) -> folder::Model {
            let conversation = self.conversation().await;
            folder::Entity::find_by_id(conversation.folder_id)
                .one(&self.db.conn)
                .await
                .unwrap()
                .unwrap()
        }

        async fn set_folder_path(&self, value: String) {
            let mut active = self.folder().await.into_active_model();
            active.path = Set(value);
            active.update(&self.db.conn).await.unwrap();
        }

        async fn delete_folder(&self) {
            let mut active = self.folder().await.into_active_model();
            active.deleted_at = Set(Some(chrono::Utc::now()));
            active.update(&self.db.conn).await.unwrap();
        }

        async fn resolve(
            &self,
            include_data: bool,
        ) -> Result<ResolveGrokSessionImageResponse, AppCommandError> {
            resolve_grok_session_image_core(
                &self.db,
                self.sessions_root.clone(),
                self.request(include_data),
            )
            .await
        }
    }

    fn jpeg_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgba8(2, 3)
            .write_to(std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .unwrap();
        bytes
    }

    fn assert_public_not_found(error: AppCommandError) {
        assert_eq!(error.code, AppErrorCode::NotFound);
        assert_eq!(error.message, SOURCE_NOT_FOUND);
    }

    #[cfg(unix)]
    fn directory_alias(target: &Path, alias: &Path) -> bool {
        std::os::unix::fs::symlink(target, alias).unwrap();
        true
    }

    #[cfg(windows)]
    fn directory_alias(target: &Path, alias: &Path) -> bool {
        match std::os::windows::fs::symlink_dir(target, alias) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => false,
            Err(error) => panic!("failed to create directory alias: {error}"),
        }
    }

    #[cfg(unix)]
    fn file_alias(target: &Path, alias: &Path) -> bool {
        std::os::unix::fs::symlink(target, alias).unwrap();
        true
    }

    #[cfg(windows)]
    fn file_alias(target: &Path, alias: &Path) -> bool {
        match std::os::windows::fs::symlink_file(target, alias) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => false,
            Err(error) => panic!("failed to create file alias: {error}"),
        }
    }

    #[tokio::test]
    async fn safe_session_hit_returns_session_path_mime_and_same_handle_bytes() {
        let fixture = ResolverFixture::new().await;
        let bytes = fixture.put_png(&fixture.session_root, 2, 3);
        let response = resolve_grok_session_image_core(
            &fixture.db,
            fixture.sessions_root.clone(),
            fixture.request(true),
        )
        .await
        .unwrap();
        assert_eq!(response.origin, GrokSessionImageOrigin::Session);
        assert_eq!(response.mime_type, "image/png");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(response.data_base64.unwrap())
                .unwrap(),
            bytes
        );
        assert_eq!(
            PathBuf::from(&response.path),
            crate::paths::simplify_verbatim_path(
                &std::fs::canonicalize(fixture.session_root.join("images/a.png")).unwrap()
            )
        );
    }

    #[tokio::test]
    async fn session_wins_without_querying_a_deleted_folder() {
        let fixture = ResolverFixture::new().await;
        fixture.put_png(&fixture.session_root, 2, 3);
        fixture.delete_folder().await;
        let response = fixture.resolve(false).await.unwrap();
        assert_eq!(response.origin, GrokSessionImageOrigin::Session);
        assert!(response.data_base64.is_none());
    }

    #[tokio::test]
    async fn workspace_only_hit_returns_workspace_and_both_files_prefer_session() {
        let fixture = ResolverFixture::new().await;
        let workspace_bytes = fixture.put_png(&fixture.workspace_root, 2, 3);
        let workspace = fixture.resolve(true).await.unwrap();
        assert_eq!(workspace.origin, GrokSessionImageOrigin::Workspace);
        assert_eq!(
            PathBuf::from(&workspace.path),
            crate::paths::simplify_verbatim_path(
                &std::fs::canonicalize(fixture.workspace_root.join("images/a.png")).unwrap()
            )
        );
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(workspace.data_base64.unwrap())
                .unwrap(),
            workspace_bytes
        );

        fixture.put_png(&fixture.session_root, 4, 5);
        let session = fixture.resolve(false).await.unwrap();
        assert_eq!(session.origin, GrokSessionImageOrigin::Session);
    }

    #[tokio::test]
    async fn non_positive_ids_reject_before_database_lookup() {
        let fixture = ResolverFixture::new().await;
        fixture.db.conn.clone().close().await.unwrap();
        for conversation_id in [0, -1, i32::MIN] {
            let request = ResolveGrokSessionImageRequest {
                conversation_id,
                href: "images/a.png".into(),
                include_data: false,
            };
            let error = resolve_grok_session_image_core(
                &fixture.db,
                fixture.sessions_root.clone(),
                request,
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }
    }

    #[tokio::test]
    async fn missing_deleted_non_grok_and_empty_external_id_share_not_found() {
        let missing = ResolverFixture::new().await;
        let mut request = missing.request(false);
        request.conversation_id = i32::MAX;
        let missing_error =
            resolve_grok_session_image_core(&missing.db, missing.sessions_root.clone(), request)
                .await
                .unwrap_err();

        let deleted = ResolverFixture::new().await;
        deleted.delete_conversation().await;
        let deleted_error = deleted.resolve(false).await.unwrap_err();

        let non_grok = ResolverFixture::new().await;
        non_grok.set_agent_type("codex").await;
        let non_grok_error = non_grok.resolve(false).await.unwrap_err();

        let empty = ResolverFixture::new().await;
        empty.set_external_id(Some(String::new())).await;
        let empty_error = empty.resolve(false).await.unwrap_err();

        for error in [missing_error, deleted_error, non_grok_error, empty_error] {
            assert_public_not_found(error);
        }
    }

    #[tokio::test]
    async fn privacy_gate_precedes_href_parsing() {
        let missing = ResolverFixture::new().await;
        let mut missing_request = missing.request(false);
        missing_request.conversation_id = i32::MAX;
        missing_request.href = "../private.png".into();
        let missing_error = resolve_grok_session_image_core(
            &missing.db,
            missing.sessions_root.clone(),
            missing_request,
        )
        .await
        .unwrap_err();

        let deleted = ResolverFixture::new().await;
        deleted.delete_conversation().await;
        let mut deleted_request = deleted.request(false);
        deleted_request.href = "../private.png".into();
        let deleted_error = resolve_grok_session_image_core(
            &deleted.db,
            deleted.sessions_root.clone(),
            deleted_request,
        )
        .await
        .unwrap_err();

        let non_grok = ResolverFixture::new().await;
        non_grok.set_agent_type("codex").await;
        let mut non_grok_request = non_grok.request(false);
        non_grok_request.href = "../private.png".into();
        let non_grok_error = resolve_grok_session_image_core(
            &non_grok.db,
            non_grok.sessions_root.clone(),
            non_grok_request,
        )
        .await
        .unwrap_err();

        let empty = ResolverFixture::new().await;
        empty.set_external_id(Some(String::new())).await;
        let mut empty_request = empty.request(false);
        empty_request.href = "../private.png".into();
        let empty_error =
            resolve_grok_session_image_core(&empty.db, empty.sessions_root.clone(), empty_request)
                .await
                .unwrap_err();

        for error in [missing_error, deleted_error, non_grok_error, empty_error] {
            assert_public_not_found(error);
        }
    }

    #[tokio::test]
    async fn valid_grok_row_rejects_invalid_href_before_filesystem_lookup() {
        let fixture = ResolverFixture::new().await;
        std::fs::remove_dir_all(&fixture.sessions_root).unwrap();
        if !directory_alias(
            &fixture.sessions_root.with_file_name("missing"),
            &fixture.sessions_root,
        ) {
            return;
        }
        let mut request = fixture.request(false);
        request.href = "../private.png".into();
        let error =
            resolve_grok_session_image_core(&fixture.db, fixture.sessions_root.clone(), request)
                .await
                .unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);
        assert_eq!(error.message, INVALID_REF_MESSAGE);
    }

    #[tokio::test]
    async fn database_query_failure_preserves_database_error() {
        let fixture = ResolverFixture::new().await;
        fixture.db.conn.clone().close().await.unwrap();
        let error = fixture.resolve(false).await.unwrap_err();
        assert_eq!(error.code, AppErrorCode::DatabaseError);
    }

    #[tokio::test]
    async fn external_id_accepts_ascii_boundaries() {
        for external_id in ["a".to_owned(), format!("a{}", "z".repeat(254))] {
            let fixture = ResolverFixture::new().await;
            fixture.set_external_id(Some(external_id)).await;
            assert_public_not_found(fixture.resolve(false).await.unwrap_err());
        }
    }

    #[tokio::test]
    async fn external_id_rejects_every_unsafe_shape() {
        for external_id in [
            "a".repeat(256),
            "_leading".into(),
            "slash/value".into(),
            r"back\slash".into(),
            "colon:value".into(),
            "white space".into(),
            "目标".into(),
            ".".into(),
            "..".into(),
            "trailing.".into(),
            "CON".into(),
            "con.session".into(),
            "COM1".into(),
            "LPT9".into(),
        ] {
            let fixture = ResolverFixture::new().await;
            fixture.set_external_id(Some(external_id)).await;
            let error = fixture.resolve(false).await.unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }
    }

    #[tokio::test]
    async fn strict_and_loose_locator_hits_both_resolve() {
        let fixture = ResolverFixture::new().await;
        fixture.put_png(&fixture.session_root, 2, 3);
        let strict = fixture.resolve(false).await.unwrap();
        assert_eq!(strict.origin, GrokSessionImageOrigin::Session);

        std::fs::remove_file(fixture.session_root.join("updates.jsonl")).unwrap();
        let loose = fixture.resolve(false).await.unwrap();
        assert_eq!(loose.origin, GrokSessionImageOrigin::Session);
        assert_eq!(strict.path, loose.path);
    }

    #[tokio::test]
    async fn duplicate_strict_and_duplicate_loose_are_invalid_input() {
        for strict in [true, false] {
            let fixture = ResolverFixture::new().await;
            fixture.put_png(&fixture.workspace_root, 2, 3);
            let duplicate = fixture.sessions_root.join("group-b/session-123");
            std::fs::create_dir_all(&duplicate).unwrap();
            if strict {
                std::fs::write(duplicate.join("updates.jsonl"), b"\n").unwrap();
            } else {
                std::fs::remove_file(fixture.session_root.join("updates.jsonl")).unwrap();
            }
            let error = fixture.resolve(false).await.unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }
    }

    #[tokio::test]
    async fn session_hit_does_not_inspect_invalid_origin_cwd() {
        let fixture = ResolverFixture::new().await;
        fixture.put_png(&fixture.session_root, 2, 3);
        fixture.set_origin_cwd(Some("relative/path".into())).await;
        fixture.delete_folder().await;
        let response = fixture.resolve(false).await.unwrap();
        assert_eq!(response.origin, GrokSessionImageOrigin::Session);
    }

    #[tokio::test]
    async fn missing_sessions_root_allows_workspace() {
        let fixture = ResolverFixture::new().await;
        fixture.put_png(&fixture.workspace_root, 2, 3);
        std::fs::remove_dir_all(&fixture.sessions_root).unwrap();
        let response = fixture.resolve(false).await.unwrap();
        assert_eq!(response.origin, GrokSessionImageOrigin::Workspace);
    }

    #[tokio::test]
    async fn missing_candidates_under_both_existing_authorities_end_not_found() {
        let fixture = ResolverFixture::new().await;
        assert_public_not_found(fixture.resolve(false).await.unwrap_err());
    }

    #[tokio::test]
    async fn dangling_sessions_root_is_io_error_without_workspace_fallback() {
        let fixture = ResolverFixture::new().await;
        fixture.put_png(&fixture.workspace_root, 2, 3);
        std::fs::remove_dir_all(&fixture.sessions_root).unwrap();
        if !directory_alias(
            &fixture.sessions_root.with_file_name("missing"),
            &fixture.sessions_root,
        ) {
            return;
        }
        let error = fixture.resolve(false).await.unwrap_err();
        assert_eq!(error.code, AppErrorCode::IoError);
    }

    #[tokio::test]
    async fn dangling_sessions_root_ancestor_denies_workspace_fallback() {
        let fixture = ResolverFixture::new().await;
        fixture.put_png(&fixture.workspace_root, 2, 3);
        let sessions_parent = fixture.sessions_root.parent().unwrap().to_path_buf();
        std::fs::remove_dir_all(&sessions_parent).unwrap();
        if !directory_alias(
            &sessions_parent.with_file_name("missing-grok-root"),
            &sessions_parent,
        ) {
            return;
        }

        let error = fixture.resolve(false).await.unwrap_err();
        assert_eq!(error.code, AppErrorCode::IoError);
    }

    #[tokio::test]
    async fn origin_cwd_existing_directory_wins_current_folder() {
        let fixture = ResolverFixture::new().await;
        let origin = fixture._temp.path().join("origin");
        std::fs::create_dir(&origin).unwrap();
        fixture.put_png(&origin, 4, 5);
        fixture.put_png(&fixture.workspace_root, 2, 3);
        fixture
            .set_origin_cwd(Some(origin.to_str().unwrap().to_owned()))
            .await;
        fixture.delete_folder().await;
        let response = fixture.resolve(false).await.unwrap();
        assert_eq!(response.origin, GrokSessionImageOrigin::Workspace);
        assert_eq!(
            PathBuf::from(response.path),
            crate::paths::simplify_verbatim_path(
                &std::fs::canonicalize(origin.join("images/a.png")).unwrap()
            )
        );
    }

    #[tokio::test]
    async fn unset_or_empty_origin_cwd_uses_current_folder() {
        for origin_cwd in [None, Some(String::new())] {
            let fixture = ResolverFixture::new().await;
            fixture.put_png(&fixture.workspace_root, 2, 3);
            fixture.set_origin_cwd(origin_cwd).await;
            let response = fixture.resolve(false).await.unwrap();
            assert_eq!(response.origin, GrokSessionImageOrigin::Workspace);
            assert_eq!(
                PathBuf::from(response.path),
                crate::paths::simplify_verbatim_path(
                    &std::fs::canonicalize(fixture.workspace_root.join("images/a.png")).unwrap()
                )
            );
        }
    }

    #[tokio::test]
    async fn missing_origin_cwd_falls_back_to_current_folder() {
        let fixture = ResolverFixture::new().await;
        fixture.put_png(&fixture.workspace_root, 2, 3);
        fixture
            .set_origin_cwd(Some(
                fixture
                    ._temp
                    .path()
                    .join("ordinary-missing")
                    .to_str()
                    .unwrap()
                    .to_owned(),
            ))
            .await;
        let response = fixture.resolve(false).await.unwrap();
        assert_eq!(response.origin, GrokSessionImageOrigin::Workspace);
        assert_eq!(
            PathBuf::from(response.path),
            crate::paths::simplify_verbatim_path(
                &std::fs::canonicalize(fixture.workspace_root.join("images/a.png")).unwrap()
            )
        );
    }

    #[tokio::test]
    async fn dangling_origin_cwd_rejects_without_current_folder_fallback() {
        for ancestor in [false, true] {
            let fixture = ResolverFixture::new().await;
            fixture.put_png(&fixture.workspace_root, 2, 3);
            let alias = fixture._temp.path().join(if ancestor {
                "dangling-ancestor"
            } else {
                "dangling-final"
            });
            if !directory_alias(&fixture._temp.path().join("missing-target"), &alias) {
                return;
            }
            let origin = if ancestor { alias.join("child") } else { alias };
            fixture
                .set_origin_cwd(Some(origin.to_str().unwrap().to_owned()))
                .await;
            let error = fixture.resolve(false).await.unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }
    }

    #[tokio::test]
    async fn relative_origin_cwd_and_nondirectory_origin_reject() {
        for origin in [PathBuf::from("relative/path"), PathBuf::new()] {
            let fixture = ResolverFixture::new().await;
            fixture.put_png(&fixture.workspace_root, 2, 3);
            let value = if origin.as_os_str().is_empty() {
                let file = fixture._temp.path().join("origin-file");
                std::fs::write(&file, b"not a directory").unwrap();
                file
            } else {
                origin
            };
            fixture
                .set_origin_cwd(Some(value.to_str().unwrap().to_owned()))
                .await;
            let error = fixture.resolve(false).await.unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unreadable_origin_cwd_preserves_permission_denied() {
        use std::os::unix::fs::PermissionsExt;

        struct RestorePermissions {
            path: PathBuf,
            permissions: std::fs::Permissions,
        }
        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.path, self.permissions.clone());
            }
        }

        let fixture = ResolverFixture::new().await;
        fixture.put_png(&fixture.workspace_root, 2, 3);
        let parent = fixture._temp.path().join("unreadable-origin-parent");
        let origin = parent.join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        fixture
            .set_origin_cwd(Some(origin.to_str().unwrap().to_owned()))
            .await;
        let permissions = std::fs::metadata(&parent).unwrap().permissions();
        let _restore = RestorePermissions {
            path: parent.clone(),
            permissions,
        };
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o0)).unwrap();
        let probe = std::fs::metadata(&origin);
        if !matches!(probe, Err(ref error) if error.kind() == std::io::ErrorKind::PermissionDenied)
        {
            return;
        }
        let error = fixture.resolve(false).await.unwrap_err();
        assert_eq!(error.code, AppErrorCode::PermissionDenied);
    }

    #[tokio::test]
    async fn missing_or_deleted_current_folder_ends_not_found() {
        let missing = ResolverFixture::new().await;
        missing
            .db
            .conn
            .execute_unprepared("PRAGMA foreign_keys=OFF")
            .await
            .unwrap();
        let mut active = missing.conversation().await.into_active_model();
        active.folder_id = Set(i32::MAX);
        active.update(&missing.db.conn).await.unwrap();
        assert_public_not_found(missing.resolve(false).await.unwrap_err());

        let deleted = ResolverFixture::new().await;
        deleted.delete_folder().await;
        assert_public_not_found(deleted.resolve(false).await.unwrap_err());
    }

    #[tokio::test]
    async fn relative_or_nondirectory_current_folder_rejects() {
        for path in [PathBuf::from("relative/path"), PathBuf::new()] {
            let fixture = ResolverFixture::new().await;
            let value = if path.as_os_str().is_empty() {
                let file = fixture._temp.path().join("folder-file");
                std::fs::write(&file, b"not a directory").unwrap();
                file.to_str().unwrap().to_owned()
            } else {
                path.to_str().unwrap().to_owned()
            };
            fixture.set_folder_path(value).await;
            let error = fixture.resolve(false).await.unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }
    }

    #[tokio::test]
    async fn dangling_current_folder_rejects() {
        for ancestor in [false, true] {
            let fixture = ResolverFixture::new().await;
            let alias = fixture._temp.path().join(if ancestor {
                "folder-dangling-ancestor"
            } else {
                "folder-dangling-final"
            });
            if !directory_alias(&fixture._temp.path().join("missing-folder-target"), &alias) {
                return;
            }
            let folder = if ancestor { alias.join("child") } else { alias };
            fixture
                .set_folder_path(folder.to_str().unwrap().to_owned())
                .await;
            let error = fixture.resolve(false).await.unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unreadable_current_folder_preserves_permission_denied() {
        use std::os::unix::fs::PermissionsExt;

        struct RestorePermissions {
            path: PathBuf,
            permissions: std::fs::Permissions,
        }
        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.path, self.permissions.clone());
            }
        }

        let fixture = ResolverFixture::new().await;
        let parent = fixture._temp.path().join("unreadable-folder-parent");
        let folder = parent.join("folder");
        std::fs::create_dir_all(&folder).unwrap();
        fixture
            .set_folder_path(folder.to_str().unwrap().to_owned())
            .await;
        let permissions = std::fs::metadata(&parent).unwrap().permissions();
        let _restore = RestorePermissions {
            path: parent.clone(),
            permissions,
        };
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o0)).unwrap();
        let probe = std::fs::metadata(&folder);
        if !matches!(probe, Err(ref error) if error.kind() == std::io::ErrorKind::PermissionDenied)
        {
            return;
        }
        let error = fixture.resolve(false).await.unwrap_err();
        assert_eq!(error.code, AppErrorCode::PermissionDenied);
    }

    #[tokio::test]
    async fn session_not_ready_allows_workspace_but_rejected_session_does_not() {
        let not_ready = ResolverFixture::new().await;
        not_ready.put_bytes(&not_ready.session_root, "a.png", b"\x89PNG");
        not_ready.put_png(&not_ready.workspace_root, 2, 3);
        let response = not_ready.resolve(false).await.unwrap();
        assert_eq!(response.origin, GrokSessionImageOrigin::Workspace);

        let rejected = ResolverFixture::new().await;
        rejected.put_bytes(&rejected.session_root, "a.png", &jpeg_bytes());
        rejected.put_png(&rejected.workspace_root, 2, 3);
        let error = rejected.resolve(false).await.unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);
    }

    #[tokio::test]
    async fn session_permission_and_oversize_errors_do_not_fall_back() {
        let oversize = ResolverFixture::new().await;
        oversize.put_png(&oversize.workspace_root, 2, 3);
        let mut bytes = png_header(1, 1);
        bytes.resize(GROK_IMAGE_MAX_BYTES + 1, 0);
        oversize.put_bytes(&oversize.session_root, "a.png", &bytes);
        let error = oversize.resolve(false).await.unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            struct RestorePermissions {
                path: PathBuf,
                permissions: std::fs::Permissions,
            }
            impl Drop for RestorePermissions {
                fn drop(&mut self) {
                    let _ = std::fs::set_permissions(&self.path, self.permissions.clone());
                }
            }

            let permission = ResolverFixture::new().await;
            permission.put_png(&permission.workspace_root, 2, 3);
            permission.put_png(&permission.session_root, 2, 3);
            let images = permission.session_root.join("images");
            let permissions = std::fs::metadata(&images).unwrap().permissions();
            let _restore = RestorePermissions {
                path: images.clone(),
                permissions,
            };
            std::fs::set_permissions(&images, std::fs::Permissions::from_mode(0o0)).unwrap();
            let probe = std::fs::read_dir(&images);
            if matches!(
                probe,
                Err(ref error) if error.kind() == std::io::ErrorKind::PermissionDenied
            ) {
                let error = permission.resolve(false).await.unwrap_err();
                assert_eq!(error.code, AppErrorCode::PermissionDenied);
            }
        }
    }

    #[tokio::test]
    async fn workspace_not_ready_ends_not_found() {
        let fixture = ResolverFixture::new().await;
        fixture.put_bytes(&fixture.workspace_root, "a.png", b"\x89PNG");
        assert_public_not_found(fixture.resolve(false).await.unwrap_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_group_and_session_identity_reject() {
        for group_alias in [true, false] {
            let fixture = ResolverFixture::new().await;
            fixture.put_png(&fixture.workspace_root, 2, 3);
            std::fs::remove_dir_all(&fixture.sessions_root).unwrap();
            std::fs::create_dir_all(&fixture.sessions_root).unwrap();
            if group_alias {
                let real_group = fixture._temp.path().join("real-group");
                let session = real_group.join("session-123");
                std::fs::create_dir_all(&session).unwrap();
                std::fs::write(session.join("updates.jsonl"), b"\n").unwrap();
                std::os::unix::fs::symlink(&real_group, fixture.sessions_root.join("group-a"))
                    .unwrap();
            } else {
                let group = fixture.sessions_root.join("group-a");
                let real_session = fixture._temp.path().join("real-session");
                std::fs::create_dir_all(&group).unwrap();
                std::fs::create_dir_all(&real_session).unwrap();
                std::fs::write(real_session.join("updates.jsonl"), b"\n").unwrap();
                std::os::unix::fs::symlink(&real_session, group.join("session-123")).unwrap();
            }
            let error = fixture.resolve(false).await.unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_session_reparse_identity_rejects_when_creation_is_available() {
        let fixture = ResolverFixture::new().await;
        fixture.put_png(&fixture.workspace_root, 2, 3);
        std::fs::remove_dir_all(&fixture.sessions_root).unwrap();
        std::fs::create_dir_all(&fixture.sessions_root).unwrap();
        let real_group = fixture._temp.path().join("real-group");
        let session = real_group.join("session-123");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("updates.jsonl"), b"\n").unwrap();
        if !directory_alias(&real_group, &fixture.sessions_root.join("group-a")) {
            return;
        }
        let error = fixture.resolve(false).await.unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);
    }

    #[tokio::test]
    async fn escaping_dangling_and_directory_candidates_reject_without_fallback() {
        enum Shape {
            Escape,
            Dangling,
            Directory,
        }
        for session_authority in [true, false] {
            for shape in [Shape::Escape, Shape::Dangling, Shape::Directory] {
                let fixture = ResolverFixture::new().await;
                let root = if session_authority {
                    fixture.put_png(&fixture.workspace_root, 2, 3);
                    &fixture.session_root
                } else {
                    &fixture.workspace_root
                };
                std::fs::create_dir_all(root.join("images")).unwrap();
                let candidate = root.join("images/a.png");
                let created = match shape {
                    Shape::Escape => {
                        let outside = fixture._temp.path().join("outside.png");
                        std::fs::write(&outside, png_header(2, 3)).unwrap();
                        file_alias(&outside, &candidate)
                    }
                    Shape::Dangling => {
                        file_alias(&fixture._temp.path().join("missing.png"), &candidate)
                    }
                    Shape::Directory => {
                        std::fs::create_dir(&candidate).unwrap();
                        true
                    }
                };
                if !created {
                    continue;
                }
                let error = fixture.resolve(false).await.unwrap_err();
                assert_eq!(error.code, AppErrorCode::InvalidInput);
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn same_images_directory_file_symlink_passes() {
        let fixture = ResolverFixture::new().await;
        fixture.put_bytes(&fixture.session_root, "real.png", &png_header(2, 3));
        std::os::unix::fs::symlink("real.png", fixture.session_root.join("images/a.png")).unwrap();
        fixture.delete_folder().await;
        let response = fixture.resolve(false).await.unwrap();
        assert_eq!(response.origin, GrokSessionImageOrigin::Session);
        assert_eq!(
            PathBuf::from(response.path),
            crate::paths::simplify_verbatim_path(
                &std::fs::canonicalize(fixture.session_root.join("images/real.png")).unwrap()
            )
        );
    }

    #[tokio::test]
    async fn uppercase_href_extension_returns_header_mime() {
        let fixture = ResolverFixture::new().await;
        fixture.put_bytes(&fixture.session_root, "a.JPEG", &jpeg_bytes());
        let mut request = fixture.request(false);
        request.href = "images/a.JPEG".into();
        let response =
            resolve_grok_session_image_core(&fixture.db, fixture.sessions_root.clone(), request)
                .await
                .unwrap();
        assert_eq!(response.mime_type, "image/jpeg");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_canonical_result_path_rejects() {
        use std::os::unix::ffi::OsStringExt;

        let candidate = ResolvedCandidate {
            canonical_path: PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff, b'a'])),
            mime_type: "image/png",
            bytes: None,
        };
        let error =
            response_from_candidate(candidate, GrokSessionImageOrigin::Session, false).unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);
    }

    #[cfg(windows)]
    #[test]
    fn windows_unsimplifiable_verbatim_result_path_rejects() {
        let candidate = ResolvedCandidate {
            canonical_path: PathBuf::from(
                r"\\?\Volume{7b2f1c40-0000-0000-0000-100000000000}\images\a.png",
            ),
            mime_type: "image/png",
            bytes: None,
        };
        let error =
            response_from_candidate(candidate, GrokSessionImageOrigin::Session, false).unwrap_err();
        assert_eq!(error.code, AppErrorCode::InvalidInput);
    }

    #[tokio::test]
    async fn include_data_false_omits_data_but_still_rejects_bad_header_size_and_pixels() {
        let valid = ResolverFixture::new().await;
        valid.put_png(&valid.session_root, 2, 3);
        let response = valid.resolve(false).await.unwrap();
        assert!(response.data_base64.is_none());

        let bad_header = ResolverFixture::new().await;
        bad_header.put_bytes(&bad_header.session_root, "a.png", &jpeg_bytes());
        assert_eq!(
            bad_header.resolve(false).await.unwrap_err().code,
            AppErrorCode::InvalidInput
        );

        let size = ResolverFixture::new().await;
        let mut bytes = png_header(1, 1);
        bytes.resize(GROK_IMAGE_MAX_BYTES + 1, 0);
        size.put_bytes(&size.session_root, "a.png", &bytes);
        assert_eq!(
            size.resolve(false).await.unwrap_err().code,
            AppErrorCode::InvalidInput
        );

        let pixels = ResolverFixture::new().await;
        pixels.put_png(&pixels.session_root, 40_000_001, 1);
        assert_eq!(
            pixels.resolve(false).await.unwrap_err().code,
            AppErrorCode::InvalidInput
        );
    }

    #[test]
    fn response_conversion_rejects_byte_presence_mismatch_without_panicking() {
        for (include_data, bytes) in [
            (true, None),
            (true, Some(Vec::new())),
            (false, Some(vec![1_u8])),
        ] {
            let candidate = ResolvedCandidate {
                canonical_path: PathBuf::from("/tmp/images/a.png"),
                mime_type: "image/png",
                bytes,
            };
            let error =
                response_from_candidate(candidate, GrokSessionImageOrigin::Session, include_data)
                    .unwrap_err();
            assert_eq!(error.code, AppErrorCode::TaskExecutionFailed);
        }
    }
}
