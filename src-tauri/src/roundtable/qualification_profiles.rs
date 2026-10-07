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
    /// Missing optional files are skipped. A required file that is absent fails.
    pub required: bool,
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
    /// Sandbox variables that point this adapter's own client at the host
    /// gateway. The base URL is the loopback relay. The bearer is the attempt
    /// token, never a host credential.
    pub model_gateway: ModelGatewayEnv,
    /// When set, at least one existing auth file must use this filename.
    /// Cursor accepts either XDG `auth.json` or `~/.cursor/auth.json`.
    pub require_one_filename: Option<&'static str>,
    /// The probe must complete an ACP initialize, session/new, and prompt
    /// inside the isolator. A missing adapter or a failed turn is a failure.
    pub requires_acp_turn: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ModelGatewayEnv {
    pub base_url_keys: &'static [&'static str],
    pub bearer_keys: &'static [&'static str],
    /// Keys whose value is the relay origin with no `/v1` suffix. Antigravity's
    /// cloud-code client appends `/v1internal:...` to this base.
    pub origin_keys: &'static [&'static str],
    pub fixed: &'static [(&'static str, &'static str)],
}

impl ModelGatewayEnv {
    pub fn entries(self, bearer: &str) -> Vec<(String, String)> {
        let mut entries = Vec::new();
        for key in self.base_url_keys {
            entries.push((
                (*key).to_string(),
                super::relay::SANDBOX_ENDPOINT.to_string(),
            ));
        }
        for key in self.origin_keys {
            entries.push(((*key).to_string(), super::relay::SANDBOX_ORIGIN.to_string()));
        }
        for key in self.bearer_keys {
            entries.push(((*key).to_string(), bearer.to_string()));
        }
        for (key, value) in self.fixed {
            entries.push(((*key).to_string(), (*value).to_string()));
        }
        entries
    }
}

const NO_MODEL_GATEWAY: ModelGatewayEnv = ModelGatewayEnv {
    base_url_keys: &[],
    bearer_keys: &[],
    origin_keys: &[],
    fixed: &[],
};

const OPENAI_GATEWAY: ModelGatewayEnv = ModelGatewayEnv {
    base_url_keys: &["OPENAI_BASE_URL"],
    bearer_keys: &["OPENAI_API_KEY"],
    origin_keys: &[],
    fixed: &[],
};

/// Grok 1.0.46 ignores `OPENAI_*`. `session/new` succeeds when these two
/// variables name the loopback gateway and the attempt bearer.
const GROK_GATEWAY: ModelGatewayEnv = ModelGatewayEnv {
    base_url_keys: &["GROK_XAI_API_BASE_URL"],
    bearer_keys: &["XAI_API_KEY"],
    origin_keys: &[],
    fixed: &[],
};

/// Antigravity 1.3.0 reads its own gateway variables. `auth.type=gateway` is
/// not an `authenticate` method; the attempt home must contain
/// `settings.json` before the process starts. The cloud-code base is the
/// relay origin so `/v1internal:...` is forwarded unchanged.
const ANTIGRAVITY_GATEWAY: ModelGatewayEnv = ModelGatewayEnv {
    base_url_keys: &["AGY_LLM_GATEWAY_URL", "AGY_GATEWAY_URL"],
    bearer_keys: &["AGY_LLM_GATEWAY_API_KEY", "AGY_GATEWAY_API_KEY"],
    origin_keys: &["AGY_ACP_CCPA_BASE_URL"],
    fixed: &[("AGY_ACP_ENABLE_GATEWAY_AUTH", "1")],
};

/// Non-secret file the ACP server reads at process start. `gateway` is absent
/// from `authMethods`, so `authenticate` cannot select it.
pub const ANTIGRAVITY_GATEWAY_SETTINGS_REL: &str = ".gemini/antigravity-acp/settings.json";
pub const ANTIGRAVITY_GATEWAY_SETTINGS_DEST: &str =
    "/rt-home/.gemini/antigravity-acp/settings.json";
pub const ANTIGRAVITY_GATEWAY_SETTINGS_BODY: &[u8] = b"{\"auth\":{\"type\":\"gateway\"}}\n";

pub fn antigravity_profile(profile: &AdapterProfile) -> bool {
    profile
        .container_env
        .iter()
        .any(|(key, _)| *key == "GEMINI_HOME")
}

pub fn write_antigravity_gateway_settings(upper: &std::path::Path) -> std::io::Result<()> {
    let path = upper.join(ANTIGRAVITY_GATEWAY_SETTINGS_REL);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, ANTIGRAVITY_GATEWAY_SETTINGS_BODY)
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
    PROFILES
}

pub fn profile_for_agent(agent: &str) -> Option<&'static AdapterProfile> {
    PROFILES.iter().find(|profile| profile.agent == agent)
}

pub fn profile_by_id(exact_id: &str) -> Option<&'static AdapterProfile> {
    PROFILES.iter().find(|profile| profile.exact_id == exact_id)
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
            required: true,
        }],
        container_env: COMMON_ENV,
        model_gateway: OPENAI_GATEWAY,
        require_one_filename: None,
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
            required: true,
        }],
        container_env: COMMON_ENV,
        model_gateway: GROK_GATEWAY,
        require_one_filename: None,
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
                required: true,
            },
            AuthFile {
                home_relative: ".config/cursor/auth.json",
                destination: "/rt-home/.config/cursor/auth.json",
                required: false,
            },
            AuthFile {
                home_relative: ".cursor/auth.json",
                destination: "/rt-home/.cursor/auth.json",
                required: false,
            },
        ],
        container_env: &[
            ("HOME", "/rt-home"),
            ("PATH", "/usr/local/bin:/usr/bin:/bin"),
            ("CURSOR_CONFIG_DIR", "/rt-home/.cursor"),
            ("XDG_CONFIG_HOME", "/rt-home/.config"),
        ],
        model_gateway: NO_MODEL_GATEWAY,
        require_one_filename: Some("auth.json"),
        requires_acp_turn: true,
    },
    AdapterProfile {
        exact_id: "linux-antigravity-acp-1.3.0",
        agent: "antigravity",
        adapter_version: "antigravity-acp@1.3.0",
        version_needle: "1.3.0",
        isolator_version: "linux-oci",
        accepted_os: DEBIAN,
        container_cli: "/usr/local/bin/agy_acp_server.par",
        cli_args: &["--uid="],
        container_mcp: "/usr/local/bin/codeg-mcp",
        auth_files: ANTIGRAVITY_AUTH,
        container_env: ANTIGRAVITY_ENV,
        model_gateway: ANTIGRAVITY_GATEWAY,
        require_one_filename: None,
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
        auth_files: ANTIGRAVITY_AUTH,
        container_env: ANTIGRAVITY_ENV,
        model_gateway: ANTIGRAVITY_GATEWAY,
        require_one_filename: None,
        requires_acp_turn: true,
    },
];

const ANTIGRAVITY_ENV: &[(&str, &str)] = &[
    ("HOME", "/rt-home"),
    ("PATH", "/usr/local/bin:/usr/bin:/bin"),
    ("GEMINI_HOME", "/rt-home/.gemini"),
];

const ANTIGRAVITY_AUTH: &[AuthFile] = &[
    AuthFile {
        home_relative: ".gemini/antigravity-acp/settings.json",
        destination: "/rt-home/.gemini/antigravity-acp/settings.json",
        // The attempt writes a non-secret gateway settings file. A missing
        // host copy is not a credential failure, and a host copy must not
        // be mounted over the gateway file.
        required: false,
    },
    AuthFile {
        home_relative: ".gemini/antigravity-acp/acp_token.json",
        destination: "/rt-home/.gemini/antigravity-acp/acp_token.json",
        required: true,
    },
    AuthFile {
        home_relative: ".gemini/antigravity-acp/acp_business_token.json",
        destination: "/rt-home/.gemini/antigravity-acp/acp_business_token.json",
        required: false,
    },
];
