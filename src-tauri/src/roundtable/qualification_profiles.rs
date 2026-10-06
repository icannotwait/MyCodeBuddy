//! Pinned Linux qualification profiles. A profile names the adapter, the
//! isolator, and the OS versions it may be certified on. It is not a
//! certificate: the probe still has to measure this host and run the checks.
//!
//! Debian 13 is an accepted profile parameter. A certificate issued on Debian
//! 12 still fails on Debian 13, and the reverse, because the issued key pins
//! the OS that was actually measured.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcceptedOs {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthFile {
    /// Path relative to the operator's home. Not a directory.
    pub home_relative: &'static str,
    /// Absolute path inside the container.
    pub destination: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct AdapterProfile {
    pub exact_id: &'static str,
    pub agent: &'static str,
    pub adapter_version: &'static str,
    pub version_needle: &'static str,
    pub isolator_version: &'static str,
    pub accepted_os: &'static [AcceptedOs],
    pub container_cli: &'static str,
    pub cli_args: &'static [&'static str],
    pub container_mcp: &'static str,
    pub auth_files: &'static [AuthFile],
    pub container_env: &'static [(&'static str, &'static str)],
    /// The probe must complete an ACP initialize, session/new, and prompt
    /// inside the isolator. A missing adapter or a failed turn is a failure.
    pub requires_acp_turn: bool,
}

const DEBIAN: &[AcceptedOs] = &[
    AcceptedOs {
        name: "linux",
        version: "debian-12",
    },
    AcceptedOs {
        name: "linux",
        version: "debian-13",
    },
];

const COMMON_ENV: &[(&str, &str)] = &[
    ("HOME", "/rt-home"),
    ("PATH", "/usr/local/bin:/usr/bin:/bin"),
];

pub fn adapter_profiles() -> &'static [AdapterProfile] {
    &PROFILES
}

pub fn profile_for_agent(agent: &str) -> Option<&'static AdapterProfile> {
    PROFILES.iter().find(|profile| profile.agent == agent)
}

pub fn os_accepted(profile: &AdapterProfile, name: &str, version: &str) -> bool {
    profile
        .accepted_os
        .iter()
        .any(|os| os.name == name && os.version == version)
}

const PROFILES: &[AdapterProfile] = &[
    AdapterProfile {
        exact_id: "linux-codex-2.1.1",
        agent: "codex",
        adapter_version: "codex-acp@2.1.1",
        version_needle: "2.1.1",
        isolator_version: "linux-oci",
        accepted_os: DEBIAN,
        container_cli: "/usr/local/bin/codex-acp",
        cli_args: &[],
        container_mcp: "/usr/local/bin/codeg-mcp",
        auth_files: &[AuthFile {
            home_relative: ".codex/auth.json",
            destination: "/rt-home/.codex/auth.json",
        }],
        container_env: COMMON_ENV,
        requires_acp_turn: true,
    },
    AdapterProfile {
        exact_id: "linux-grok-1.0.46",
        agent: "grok",
        adapter_version: "grok@1.0.46",
        version_needle: "1.0.46",
        isolator_version: "linux-oci",
        accepted_os: DEBIAN,
        container_cli: "/usr/local/bin/grok",
        cli_args: &["--no-auto-update", "agent", "stdio"],
        container_mcp: "/usr/local/bin/codeg-mcp",
        auth_files: &[AuthFile {
            home_relative: ".grok/auth.json",
            destination: "/rt-home/.grok/auth.json",
        }],
        container_env: COMMON_ENV,
        requires_acp_turn: true,
    },
    AdapterProfile {
        exact_id: "linux-cursor-acp-2026.09.28-64d2043",
        agent: "cursor",
        adapter_version: "cursor-agent@2026.09.28-64d2043",
        version_needle: "2026.09.28-64d2043",
        isolator_version: "linux-oci",
        accepted_os: DEBIAN,
        container_cli: "/usr/local/bin/cursor-agent",
        cli_args: &["acp"],
        container_mcp: "/usr/local/bin/codeg-mcp",
        auth_files: &[
            AuthFile {
                home_relative: ".cursor/cli-config.json",
                destination: "/rt-home/.cursor/cli-config.json",
            },
            AuthFile {
                home_relative: ".config/cursor/auth.json",
                destination: "/rt-home/.config/cursor/auth.json",
            },
        ],
        container_env: &[
            ("HOME", "/rt-home"),
            ("PATH", "/usr/local/bin:/usr/bin:/bin"),
            ("CURSOR_CONFIG_DIR", "/rt-home/.cursor"),
        ],
        requires_acp_turn: true,
    },
    AdapterProfile {
        exact_id: "linux-antigravity-acp-1.2.1",
        agent: "antigravity",
        adapter_version: "antigravity-acp@1.2.1",
        version_needle: "1.2.1",
        isolator_version: "linux-oci",
        accepted_os: DEBIAN,
        container_cli: "/usr/local/bin/agy_acp_server.par",
        cli_args: &["--uid="],
        container_mcp: "/usr/local/bin/codeg-mcp",
        auth_files: &[AuthFile {
            home_relative: ".gemini/antigravity-acp/settings.json",
            destination: "/rt-home/.gemini/antigravity-acp/settings.json",
        }],
        container_env: &[
            ("HOME", "/rt-home"),
            ("PATH", "/usr/local/bin:/usr/bin:/bin"),
            ("GEMINI_HOME", "/rt-home/.gemini"),
        ],
        requires_acp_turn: true,
    },
];
