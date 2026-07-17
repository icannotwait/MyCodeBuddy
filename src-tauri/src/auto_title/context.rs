//! Visible prompt projection and scalar-safe context bounding for auto titles.

use crate::acp::types::PromptInputBlock;
use crate::parsers::fold_reference_links;

/// Project wire prompt blocks to privacy-safe visible text for title capture.
///
/// Rules:
/// - Drop a text block only when every non-empty line begins with the structured
///   mandatory-route prefix.
/// - Project `ResourceLink` to `name`.
/// - Project embedded `Resource` to a URI-derived basename.
/// - Ignore image data and embedded resource `text`/`blob`.
pub fn project_visible_prompt(blocks: &[PromptInputBlock]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match block {
            PromptInputBlock::Text { text } => {
                if is_internal_route_only(text) {
                    continue;
                }
                parts.push(text.clone());
            }
            PromptInputBlock::ResourceLink { name, .. } => {
                parts.push(name.clone());
            }
            PromptInputBlock::Resource { uri, .. } => {
                parts.push(uri_basename(uri));
            }
            PromptInputBlock::Image { .. } => {}
        }
    }
    parts.join("\n")
}

/// Fold Markdown reference links, then cap to 4,000 Unicode scalar values as
/// 2,995 prefix + `\n...\n` + 1,000 suffix when over the limit.
pub fn bound_context(text: &str) -> String {
    let folded = fold_reference_links(text);
    let chars: Vec<char> = folded.chars().collect();
    if chars.len() <= 4_000 {
        return folded;
    }
    let mut bounded = String::with_capacity(folded.len());
    bounded.extend(chars[..2_995].iter());
    bounded.push_str("\n...\n");
    bounded.extend(chars[chars.len() - 1_000..].iter());
    bounded
}

const MANDATORY_ROUTE_PREFIX: &str = "Codeg mandatory delegation route:";

fn is_internal_route_only(text: &str) -> bool {
    let mut saw_non_empty = false;
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        saw_non_empty = true;
        if !line.starts_with(MANDATORY_ROUTE_PREFIX) {
            return false;
        }
    }
    saw_non_empty
}

fn uri_basename(uri: &str) -> String {
    let without_fragment = uri.split('#').next().unwrap_or(uri);
    let path = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment);
    path.rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(path)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::types::PromptInputBlock;
    use crate::auto_title::types::{
        parse_supported_app_locale, CapturedPrompt, ConnectionLaunchContext, ConnectionPurpose,
        PromptCaptureContext,
    };
    use crate::models::system::AppLocale;

    #[test]
    fn fallback_projection_keeps_labels_and_drops_private_payloads() {
        let blocks = vec![
            PromptInputBlock::Text {
                text: "Codeg mandatory delegation route: profile_id=\"x\"\n".into(),
            },
            PromptInputBlock::ResourceLink {
                uri: "file:///repo/README.md".into(),
                name: "README.md".into(),
                mime_type: Some("text/markdown".into()),
                description: None,
            },
            PromptInputBlock::Resource {
                uri: "file:///repo/secret.txt".into(),
                mime_type: Some("text/plain".into()),
                text: Some("SECRET-BYTES".into()),
                blob: Some("BASE64".into()),
            },
            PromptInputBlock::Image {
                data: "IMAGE-BYTES".into(),
                mime_type: "image/png".into(),
                uri: Some("file:///repo/screen.png".into()),
            },
        ];
        let visible = project_visible_prompt(&blocks);
        assert_eq!(visible, "README.md\nsecret.txt");
        assert!(!visible.contains("SECRET-BYTES"));
        assert!(!visible.contains("IMAGE-BYTES"));
    }

    #[test]
    fn internal_route_filtering_keeps_mixed_user_text() {
        let blocks = vec![PromptInputBlock::Text {
            text: "Codeg mandatory delegation route: profile_id=\"x\"\nPlease fix the bug\n".into(),
        }];
        let visible = project_visible_prompt(&blocks);
        assert!(
            visible.contains("Please fix the bug"),
            "user prose must not be dropped when mixed with route lines"
        );
        assert!(
            visible.contains("Codeg mandatory delegation route:"),
            "mixed blocks are kept whole"
        );
    }

    #[test]
    fn internal_route_filtering_drops_all_prefix_lines_even_with_blanks() {
        let blocks = vec![PromptInputBlock::Text {
            text: "Codeg mandatory delegation route: profile_id=\"a\"\n\nCodeg mandatory delegation route: profile_id=\"b\"\n"
                .into(),
        }];
        assert_eq!(project_visible_prompt(&blocks), "");
    }

    #[test]
    fn bounded_context_keeps_2995_marker_and_1000_suffix() {
        // 4_001 scalars forces truncation: 2_995 + "\n...\n" (5) + 1_000 = 4_000.
        let mut text = String::new();
        for i in 0..4_001 {
            text.push(char::from_u32(0x4E00 + (i % 100) as u32).unwrap_or('x'));
        }
        assert_eq!(text.chars().count(), 4_001);

        let bounded = bound_context(&text);
        let chars: Vec<char> = bounded.chars().collect();
        assert_eq!(chars.len(), 4_000);

        let original: Vec<char> = text.chars().collect();
        assert_eq!(&chars[..2_995], &original[..2_995]);
        assert_eq!(&chars[2_995..3_000], &['\n', '.', '.', '.', '\n']);
        assert_eq!(&chars[3_000..], &original[original.len() - 1_000..]);
    }

    #[test]
    fn bound_context_folds_reference_links_before_cap() {
        let text = "see [README](file:///very/long/path/README.md) please";
        let bounded = bound_context(text);
        assert_eq!(bounded, "see README please");
        assert!(!bounded.contains("file://"));
    }

    #[test]
    fn parse_supported_app_locale_accepts_all_ten_snake_case_ids() {
        let cases = [
            ("en", AppLocale::En),
            ("zh_cn", AppLocale::ZhCn),
            ("zh_tw", AppLocale::ZhTw),
            ("ja", AppLocale::Ja),
            ("ko", AppLocale::Ko),
            ("es", AppLocale::Es),
            ("de", AppLocale::De),
            ("fr", AppLocale::Fr),
            ("pt", AppLocale::Pt),
            ("ar", AppLocale::Ar),
        ];
        for (wire, expected) in cases {
            assert_eq!(
                parse_supported_app_locale(Some(wire)),
                Some(expected),
                "wire {wire}"
            );
        }
    }

    #[test]
    fn parse_supported_app_locale_rejects_unknown_and_mixed_case() {
        assert_eq!(parse_supported_app_locale(None), None);
        assert_eq!(parse_supported_app_locale(Some("")), None);
        assert_eq!(parse_supported_app_locale(Some("EN")), None);
        assert_eq!(parse_supported_app_locale(Some("Zh_Cn")), None);
        assert_eq!(parse_supported_app_locale(Some("zh-CN")), None);
        assert_eq!(parse_supported_app_locale(Some("klingon")), None);
    }

    #[test]
    fn prompt_capture_context_constructor_and_launch_types_exist() {
        let capture = PromptCaptureContext::new(Some(String::new()), Some(AppLocale::Ja));
        assert_eq!(capture.visible_text.as_deref(), Some(""));
        assert_eq!(capture.locale, Some(AppLocale::Ja));

        let launch = ConnectionLaunchContext {
            purpose: ConnectionPurpose::User,
            inherited_locale: Some(AppLocale::En),
        };
        assert_eq!(launch.purpose, ConnectionPurpose::User);
        assert_eq!(launch.inherited_locale, Some(AppLocale::En));

        let captured = CapturedPrompt {
            visible_text: "hi".into(),
            locale: AppLocale::Ko,
        };
        assert_eq!(captured.visible_text, "hi");
        assert_eq!(captured.locale, AppLocale::Ko);
    }

    #[test]
    fn prompt_capture_from_wire_omits_when_both_absent() {
        use crate::auto_title::types::prompt_capture_from_wire;
        assert_eq!(prompt_capture_from_wire(None, None), None);
    }

    #[test]
    fn prompt_capture_from_wire_keeps_empty_visible_text_authoritative() {
        use crate::auto_title::types::prompt_capture_from_wire;
        let capture = prompt_capture_from_wire(Some(String::new()), Some("ja".into()))
            .expect("capture present");
        assert_eq!(capture.visible_text.as_deref(), Some(""));
        assert_eq!(capture.locale, Some(AppLocale::Ja));
    }

    #[test]
    fn prompt_capture_from_wire_unknown_locale_is_none_not_reject() {
        use crate::auto_title::types::prompt_capture_from_wire;
        let capture = prompt_capture_from_wire(Some("visible".into()), Some("Klingon".into()))
            .expect("unknown locale must not reject");
        assert_eq!(capture.visible_text.as_deref(), Some("visible"));
        assert_eq!(
            capture.locale, None,
            "unknown wire locale falls through to connection effective locale"
        );
        let mixed = prompt_capture_from_wire(None, Some("Zh_Cn".into())).expect("present");
        assert_eq!(mixed.visible_text, None);
        assert_eq!(mixed.locale, None);
    }

    #[tokio::test]
    async fn user_launch_context_loads_persisted_system_language() {
        use crate::auto_title::types::user_launch_context_from_db;
        use crate::commands::system_settings::SYSTEM_LANGUAGE_SETTINGS_KEY;
        use crate::db::service::app_metadata_service;
        use crate::db::test_helpers;
        use crate::models::system::{LanguageMode, SystemLanguageSettings};

        let db = test_helpers::fresh_in_memory_db().await;
        let settings = SystemLanguageSettings {
            mode: LanguageMode::Manual,
            language: AppLocale::Ja,
        };
        app_metadata_service::upsert_value(
            &db.conn,
            SYSTEM_LANGUAGE_SETTINGS_KEY,
            &serde_json::to_string(&settings).expect("serialize language"),
        )
        .await
        .expect("persist language");

        let launch = user_launch_context_from_db(&db.conn).await;
        assert_eq!(launch.purpose, ConnectionPurpose::User);
        assert_eq!(
            launch.inherited_locale,
            Some(AppLocale::Ja),
            "UI/automation roots must load persisted system language, not English default"
        );
    }
}
