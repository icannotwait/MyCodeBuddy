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
    /// Credential keys copied from the agent's Codeg settings (`env_json`)
    /// into the container env. Values are never logged; every copied value
    /// joins the diagnostic redaction list.
    pub settings_credential: SettingsCredential,
}

/// When an adapter's credential comes from Codeg's agent settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsGate {
    /// The adapter has no settings credential.
    Never,
    /// Copied whenever the required keys are non-empty.
    Always,
    /// Cursor: copied unless `CURSOR_AUTH_MODE=subscription`. A row with no
    /// explicit mode and a saved key is custom (the settings panel's rule).
    CursorCustomMode,
}

#[derive(Clone, Copy, Debug)]
pub struct SettingsCredential {
    pub gate: SettingsGate,
    /// Every one of these must be non-empty for the credential to count.
    pub required: &'static [&'static str],
    /// Copied when present and non-empty. Includes `required`.
    pub keys: &'static [&'static str],
    /// With the settings credential active, host auth files are neither
    /// required nor mounted. Cursor's ACP server clears a login file it finds
    /// next to an API key, so the files must stay out of the container.
    pub replaces_auth_files: bool,
}

pub const NO_SETTINGS_CREDENTIAL: SettingsCredential = SettingsCredential {
    gate: SettingsGate::Never,
    required: &[],
    keys: &[],
    replaces_auth_files: false,
};

impl SettingsCredential {
    /// The container env this credential contributes, or `None` when the
    /// gate is closed or a required key is missing. `settings` is the
    /// agent's saved `env_json`.
    pub fn container_env(
        &self,
        settings: &std::collections::BTreeMap<String, String>,
    ) -> Option<Vec<(String, String)>> {
        let value = |key: &str| {
            settings
                .get(key)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        match self.gate {
            SettingsGate::Never => return None,
            SettingsGate::Always => {}
            SettingsGate::CursorCustomMode => {
                let mode = value("CURSOR_AUTH_MODE");
                if mode.as_deref() == Some("subscription") {
                    return None;
                }
            }
        }
        if self.required.is_empty() || self.required.iter().any(|key| value(key).is_none()) {
            return None;
        }
        Some(
            self.keys
                .iter()
                .filter_map(|key| value(key).map(|found| ((*key).to_string(), found)))
                .collect(),
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ModelGatewayEnv {
    pub base_url_keys: &'static [&'static str],
    pub bearer_keys: &'static [&'static str],
    /// Keys whose value is the relay origin with no `/v1` suffix.
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

/// Grok 1.0.46 ignores `OPENAI_*`. `session/new` succeeds when these two
/// variables name the loopback gateway and the attempt bearer.
const GROK_GATEWAY: ModelGatewayEnv = ModelGatewayEnv {
    base_url_keys: &["GROK_XAI_API_BASE_URL"],
    bearer_keys: &["XAI_API_KEY"],
    origin_keys: &[],
    fixed: &[],
};

/// Live seat environment. A profile with gateway variables uses those.
/// Antigravity does not: `AGY_ACP_CCPA_BASE_URL` points `fetchAvailableModels`
/// at the loopback relay, which does not serve that API, and the other `AGY_*`
/// gateway variables are unnecessary once the host oauth files are mounted.
/// Cursor, Codex and CodeBuddy talk to their own service through slirp, so a
/// relay `OPENAI_BASE_URL` would only misroute them. Unknown agents still get
/// the OpenAI loopback pair.
pub fn live_model_env(agent: &str, bearer: &str) -> Vec<(String, String)> {
    let profile = profile_for_agent(agent);
    let modeled = profile
        .map(|profile| profile.model_gateway.entries(bearer))
        .unwrap_or_default();
    if !modeled.is_empty() || profile.is_some() {
        return modeled;
    }
    vec![
        (
            "OPENAI_BASE_URL".to_string(),
            super::relay::SANDBOX_ENDPOINT.to_string(),
        ),
        ("OPENAI_API_KEY".to_string(), bearer.to_string()),
    ]
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
        // API-key mode stores the key in `~/.codex/auth.json`. Codex sends
        // that key itself, so it cannot ride the attempt-bearer gateway; it
        // leaves through slirp like Cursor and Antigravity.
        model_gateway: NO_MODEL_GATEWAY,
        require_one_filename: None,
        requires_acp_turn: true,
        settings_credential: NO_SETTINGS_CREDENTIAL,
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
        settings_credential: NO_SETTINGS_CREDENTIAL,
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
        // Custom (API-key) mode: `cursor-agent acp` authenticates from
        // CURSOR_API_KEY at startup. Subscription mode keeps the login files.
        settings_credential: SettingsCredential {
            gate: SettingsGate::CursorCustomMode,
            required: &["CURSOR_API_KEY"],
            keys: &["CURSOR_API_KEY"],
            replaces_auth_files: true,
        },
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
        model_gateway: NO_MODEL_GATEWAY,
        require_one_filename: None,
        requires_acp_turn: true,
        settings_credential: NO_SETTINGS_CREDENTIAL,
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
        model_gateway: NO_MODEL_GATEWAY,
        require_one_filename: None,
        requires_acp_turn: true,
        settings_credential: NO_SETTINGS_CREDENTIAL,
    },
    AdapterProfile {
        exact_id: "linux-codebuddy-2.161.0",
        agent: "code_buddy",
        adapter_version: "codebuddy-code@2.161.0",
        version_needle: "2.161.0",
        isolator_version: "linux-oci",
        accepted_os: DEBIAN,
        // Shim over the npm tree in /usr/local/codebuddy, run by the Node 22
        // copied next to it (the image's Debian node is 20; CodeBuddy needs 22).
        container_cli: "/usr/local/bin/codebuddy",
        cli_args: &["--acp"],
        container_mcp: "/usr/local/bin/codeg-mcp",
        auth_files: &[],
        container_env: COMMON_ENV,
        model_gateway: NO_MODEL_GATEWAY,
        require_one_filename: None,
        requires_acp_turn: true,
        settings_credential: SettingsCredential {
            gate: SettingsGate::Always,
            required: &["CODEBUDDY_API_KEY"],
            keys: &[
                "CODEBUDDY_API_KEY",
                "CODEBUDDY_INTERNET_ENVIRONMENT",
                "CODEBUDDY_BASE_URL",
            ],
            replaces_auth_files: true,
        },
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
        // Host oauth selection (`auth.type`) is what `session/new` reads.
        // A gateway settings file is not written over this mount.
        required: true,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antigravity_live_env_is_empty_and_grok_keeps_its_gateway() {
        let bearer = "attempt-bearer";
        assert!(live_model_env("antigravity", bearer).is_empty());
        let grok = live_model_env("grok", bearer);
        assert!(grok.iter().any(|(key, value)| {
            key == "GROK_XAI_API_BASE_URL" && value == "http://127.0.0.1:39173/v1"
        }));
        assert!(grok
            .iter()
            .any(|(key, value)| key == "XAI_API_KEY" && value == bearer));
        assert!(!grok.iter().any(|(key, _)| key.starts_with("AGY_")));
        for direct in ["cursor", "codex", "code_buddy"] {
            assert!(live_model_env(direct, bearer).is_empty(), "{direct}");
        }
        let unknown = live_model_env("unknown-agent", bearer);
        assert!(unknown
            .iter()
            .any(|(key, value)| key == "OPENAI_BASE_URL" && value == "http://127.0.0.1:39173/v1"));
        assert!(unknown
            .iter()
            .any(|(key, value)| key == "OPENAI_API_KEY" && value == bearer));
    }

    fn settings(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn cursor_settings_key_follows_the_custom_mode() {
        let cursor = profile_for_agent("cursor")
            .expect("cursor")
            .settings_credential;
        let custom = cursor
            .container_env(&settings(&[
                ("CURSOR_AUTH_MODE", "custom"),
                ("CURSOR_API_KEY", " key-1 "),
                ("CURSOR_FORCE", "1"),
            ]))
            .expect("custom mode copies the key");
        assert_eq!(
            custom,
            vec![("CURSOR_API_KEY".to_string(), "key-1".to_string())]
        );
        // Legacy row: no explicit mode, saved key => custom.
        assert!(cursor
            .container_env(&settings(&[("CURSOR_API_KEY", "key-1")]))
            .is_some());
        assert!(cursor
            .container_env(&settings(&[
                ("CURSOR_AUTH_MODE", "subscription"),
                ("CURSOR_API_KEY", "key-1"),
            ]))
            .is_none());
        assert!(cursor
            .container_env(&settings(&[
                ("CURSOR_AUTH_MODE", "custom"),
                ("CURSOR_API_KEY", " ")
            ]))
            .is_none());
    }

    #[test]
    fn codebuddy_profile_copies_only_its_own_keys() {
        let profile = profile_for_agent("code_buddy").expect("codebuddy profile");
        assert_eq!(profile.container_cli, "/usr/local/bin/codebuddy");
        assert_eq!(profile.cli_args, &["--acp"]);
        assert!(profile.auth_files.is_empty());
        let env = profile
            .settings_credential
            .container_env(&settings(&[
                ("CODEBUDDY_API_KEY", "cb-key"),
                ("CODEBUDDY_INTERNET_ENVIRONMENT", "internal"),
                ("UNRELATED", "x"),
            ]))
            .expect("api key present");
        assert_eq!(
            env,
            vec![
                ("CODEBUDDY_API_KEY".to_string(), "cb-key".to_string()),
                (
                    "CODEBUDDY_INTERNET_ENVIRONMENT".to_string(),
                    "internal".to_string()
                ),
            ]
        );
        assert!(profile
            .settings_credential
            .container_env(&settings(&[("CODEBUDDY_INTERNET_ENVIRONMENT", "internal")]))
            .is_none());
        for agent in ["grok", "antigravity", "codex"] {
            assert!(profile_for_agent(agent)
                .expect(agent)
                .settings_credential
                .container_env(&settings(&[
                    ("CURSOR_API_KEY", "k"),
                    ("CODEBUDDY_API_KEY", "k")
                ]))
                .is_none());
        }
    }
}
