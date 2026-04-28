use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::agent::AgentType;

#[derive(Debug, Clone, Serialize)]
pub struct FolderHistoryEntry {
    pub id: i32,
    pub path: String,
    pub name: String,
    pub last_opened_at: DateTime<Utc>,
    pub connection_id: Option<String>,
    pub remote_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderDetail {
    pub id: i32,
    pub name: String,
    pub path: String,
    pub git_branch: Option<String>,
    pub default_agent_type: Option<AgentType>,
    pub last_opened_at: DateTime<Utc>,
    pub sort_order: i32,
    pub color: String,
    pub connection_id: Option<String>,
    pub remote_path: Option<String>,
}

/// Compose the synthetic `path` value used for remote folders. The format is
/// `ssh://<connection_id><normalized-remote-path>` — for absolute paths the
/// leading `/` is preserved; for `~`-rooted paths a separator is inserted.
pub fn synthetic_remote_path(connection_id: &str, remote_path: &str) -> String {
    let normalized = remote_path.trim();
    if normalized.starts_with('~') {
        format!("ssh://{}/{}", connection_id, normalized)
    } else {
        format!("ssh://{}{}", connection_id, normalized)
    }
}

/// Inverse of [`synthetic_remote_path`]. Returns None for non-remote paths.
#[allow(dead_code)] // Reserved for callers that need to inspect a synthetic path; kept here for symmetry with `synthetic_remote_path`.
pub fn parse_remote_path(p: &str) -> Option<(&str, &str)> {
    let rest = p.strip_prefix("ssh://")?;
    let slash = rest.find('/')?;
    let conn = &rest[..slash];
    let path = &rest[slash + 1..];
    if path.starts_with('~') {
        Some((conn, path))
    } else {
        // Restore the leading slash for absolute remote paths.
        Some((conn, &rest[slash..]))
    }
}

/// Pick a display name from a remote path: the last non-empty path segment,
/// falling back to "remote" when the path is just `/` or `~`.
pub fn folder_name_from_remote_path(remote_path: &str) -> String {
    let trimmed = remote_path.trim().trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "~" {
        return "remote".to_string();
    }
    trimmed
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("remote")
        .to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenedTab {
    pub id: i32,
    pub folder_id: i32,
    pub conversation_id: Option<i32>,
    pub agent_type: AgentType,
    pub position: i32,
    pub is_active: bool,
    pub is_pinned: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderCommandInfo {
    pub id: i32,
    pub folder_id: i32,
    pub name: String,
    pub command: String,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_path_absolute() {
        assert_eq!(
            synthetic_remote_path("conn_x", "/home/alice/proj"),
            "ssh://conn_x/home/alice/proj"
        );
    }

    #[test]
    fn synthetic_path_tilde() {
        assert_eq!(
            synthetic_remote_path("conn_x", "~/code/foo"),
            "ssh://conn_x/~/code/foo"
        );
    }

    #[test]
    fn synthetic_path_trims_input() {
        assert_eq!(
            synthetic_remote_path("conn_x", "  /tmp/test  "),
            "ssh://conn_x/tmp/test"
        );
    }

    #[test]
    fn parse_remote_round_trip_absolute() {
        let s = synthetic_remote_path("conn_x", "/home/alice/proj");
        let (conn, path) = parse_remote_path(&s).unwrap();
        assert_eq!(conn, "conn_x");
        assert_eq!(path, "/home/alice/proj");
    }

    #[test]
    fn parse_remote_round_trip_tilde() {
        let s = synthetic_remote_path("conn_x", "~/code/foo");
        let (conn, path) = parse_remote_path(&s).unwrap();
        assert_eq!(conn, "conn_x");
        assert_eq!(path, "~/code/foo");
    }

    #[test]
    fn parse_remote_rejects_local() {
        assert!(parse_remote_path("/Users/alice/proj").is_none());
        assert!(parse_remote_path("https://example.com/x").is_none());
    }

    #[test]
    fn folder_name_basic() {
        assert_eq!(folder_name_from_remote_path("/home/alice/myproj"), "myproj");
        assert_eq!(folder_name_from_remote_path("~/code/foo/"), "foo");
    }

    #[test]
    fn folder_name_root_fallback() {
        assert_eq!(folder_name_from_remote_path("/"), "remote");
        assert_eq!(folder_name_from_remote_path("~"), "remote");
        assert_eq!(folder_name_from_remote_path(""), "remote");
    }
}
