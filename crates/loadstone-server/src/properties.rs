//! `server.properties`: the same keys, defaults and file shape a vanilla server
//! uses, so a panel or an operator can configure this server the way they
//! configure any other.
//!
//! Two rules keep it usable:
//!
//! * **Nothing a user wrote is dropped.** Keys we do not act on yet are read and
//!   written back verbatim, so setting `pvp=false` today does not get silently
//!   erased before the feature lands.
//! * **Missing keys are added, not required.** A file from an older server, or a
//!   hand-written one with three lines in it, is filled out with the vanilla
//!   defaults on startup rather than rejecting the server.
//!
//! Values are *not* Java-escaped when written (`level-type=minecraft:normal`,
//! not `minecraft\:normal`), but the escaped form is understood when read so a
//! file copied from a vanilla server loads unchanged.

use std::fmt::Write as _;
use std::io;
use std::path::Path;

/// The keys this server actually acts on. Everything else in the file is
/// preserved but has no effect yet, and the README says so.
///
/// `view-distance` and `simulation-distance` are deliberately *not* here. The
/// chunk streamer keeps its own radius so a joining client finishes "downloading
/// terrain" quickly; taking the file's `10` would ask it for a 21x21 square
/// instead of a 3x3 one. Having the file and the login packet disagree is worse
/// than either value on its own, so the pair lands together or not at all.
pub const HONOURED: &[&str] = &[
    "server-ip",
    "server-port",
    "motd",
    "max-players",
    "online-mode",
    "level-name",
    "enable-status",
    "network-compression-threshold",
    "gamemode",
    "hardcore",
];

/// The numeric id for a `gamemode` name, which is the form Login (play) carries.
/// The field is a signed byte on the wire, so this is one too.
pub fn gamemode_id(name: &str) -> Option<i8> {
    match name {
        "survival" => Some(0),
        "creative" => Some(1),
        "adventure" => Some(2),
        "spectator" => Some(3),
        _ => None,
    }
}

/// The name for a `gamemode` id, which is what the console's `Default game type`
/// line prints (vanilla upper-cases it there).
pub fn gamemode_name(id: i8) -> &'static str {
    match id {
        1 => "creative",
        2 => "adventure",
        3 => "spectator",
        _ => "survival",
    }
}

/// Vanilla `server.properties` for 1.21.11, in the order the file writes them.
///
/// The order is the alphabetical one Java's `Properties.store` produces, which is
/// why it is not grouped by subject: a first run's file is meant to be diffable
/// against a vanilla one line for line. `tests/data/vanilla-server.properties` is
/// the file a real 1.21.11 server wrote for itself, and a test compares this table
/// against it in both keys and values, so drift shows up as a failure rather than
/// as a missing key an operator only notices when a setting does nothing.
///
/// `management-server-secret` is the one value that cannot match: vanilla
/// generates a random one per install. It is left empty, which is the same "off"
/// state `management-server-enabled=false` is in — nothing here speaks that
/// protocol. Keys an older vanilla wrote and this one does not (`pvp`,
/// `allow-nether`, `enable-command-block`, the `spawn-*` switches) are absent for
/// the same reason vanilla's file is: they are not in the set any more. A file
/// that still has them keeps them.
const DEFAULTS: &[(&str, &str)] = &[
    ("accepts-transfers", "false"),
    ("allow-flight", "false"),
    ("broadcast-console-to-ops", "true"),
    ("broadcast-rcon-to-ops", "true"),
    ("bug-report-link", ""),
    ("difficulty", "easy"),
    ("enable-code-of-conduct", "false"),
    ("enable-jmx-monitoring", "false"),
    ("enable-query", "false"),
    ("enable-rcon", "false"),
    ("enable-status", "true"),
    ("enforce-secure-profile", "true"),
    ("enforce-whitelist", "false"),
    ("entity-broadcast-range-percentage", "100"),
    ("force-gamemode", "false"),
    ("function-permission-level", "2"),
    ("gamemode", "survival"),
    ("generate-structures", "true"),
    ("generator-settings", "{}"),
    ("hardcore", "false"),
    ("hide-online-players", "false"),
    ("initial-disabled-packs", ""),
    ("initial-enabled-packs", "vanilla"),
    ("level-name", "world"),
    ("level-seed", ""),
    ("level-type", "minecraft:normal"),
    ("log-ips", "true"),
    ("management-server-allowed-origins", ""),
    ("management-server-enabled", "false"),
    ("management-server-host", "localhost"),
    ("management-server-port", "0"),
    ("management-server-secret", ""),
    ("management-server-tls-enabled", "true"),
    ("management-server-tls-keystore", ""),
    ("management-server-tls-keystore-password", ""),
    ("max-chained-neighbor-updates", "1000000"),
    ("max-players", "20"),
    ("max-tick-time", "60000"),
    ("max-world-size", "29999984"),
    ("motd", "A Minecraft Server"),
    ("network-compression-threshold", "256"),
    ("online-mode", "true"),
    ("op-permission-level", "4"),
    ("pause-when-empty-seconds", "60"),
    ("player-idle-timeout", "0"),
    ("prevent-proxy-connections", "false"),
    ("query.port", "25565"),
    ("rate-limit", "0"),
    ("rcon.password", ""),
    ("rcon.port", "25575"),
    ("region-file-compression", "deflate"),
    ("require-resource-pack", "false"),
    ("resource-pack", ""),
    ("resource-pack-id", ""),
    ("resource-pack-prompt", ""),
    ("resource-pack-sha1", ""),
    ("server-ip", ""),
    ("server-port", "25565"),
    ("simulation-distance", "10"),
    ("spawn-protection", "16"),
    ("status-heartbeat-interval", "0"),
    ("sync-chunk-writes", "true"),
    ("text-filtering-config", ""),
    ("text-filtering-version", "0"),
    ("use-native-transport", "true"),
    ("view-distance", "10"),
    ("white-list", "false"),
];

/// The vanilla default for a key, or `None` if it is not a vanilla key.
pub fn default_for(key: &str) -> Option<&'static str> {
    DEFAULTS
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| *value)
}

/// An ordered view of `server.properties`.
#[derive(Debug, Clone)]
pub struct ServerProperties {
    entries: Vec<(String, String)>,
}

impl Default for ServerProperties {
    fn default() -> Self {
        Self {
            entries: DEFAULTS
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        }
    }
}

impl ServerProperties {
    /// A fresh file, exactly as a first run writes it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a properties file.
    ///
    /// Returns `None` when the file does not exist yet, which is how a first run
    /// is recognised. Keys in [`DEFAULTS`] that the file is missing are added, so
    /// the caller always sees a complete set.
    pub fn load(path: &Path) -> io::Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };

        let mut properties = Self {
            entries: Vec::new(),
        };
        for line in text.lines() {
            if let Some((key, value)) = parse_line(line) {
                properties.insert(key, value);
            }
        }
        // Fill in anything the file predates (or that someone deleted).
        for (key, value) in DEFAULTS {
            if properties.get(key).is_none() {
                properties
                    .entries
                    .push(((*key).to_string(), (*value).to_string()));
            }
        }
        Ok(Some(properties))
    }

    /// Write the file, with a one-line header noting what it is.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut out = String::from(
            "#Minecraft server properties\n#Written by LoadstoneMC. Keys this server does not act on yet are preserved.\n",
        );
        for (key, value) in &self.entries {
            let _ = writeln!(out, "{key}={value}");
        }
        std::fs::write(path, out)
    }

    /// Replaces an existing key in place, or appends it. Appending keeps the
    /// relative order stable so rewriting the file does not churn a diff.
    fn insert(&mut self, key: String, value: String) {
        match self.entries.iter_mut().find(|(name, _)| *name == key) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// A string, falling back to the vanilla default when the key is absent or
    /// empty where vanilla treats empty as unset.
    pub fn string(&self, key: &str) -> String {
        match self.get(key) {
            Some(value) if !value.is_empty() => value.to_string(),
            _ => default_for(key).unwrap_or_default().to_string(),
        }
    }

    /// An optional string, where `None` means "unset" rather than "use the
    /// default" — vanilla uses `server-ip=` for "listen on everything".
    pub fn optional_string(&self, key: &str) -> Option<String> {
        match self.get(key) {
            Some(value) if !value.is_empty() => Some(value.to_string()),
            _ => None,
        }
    }

    pub fn boolean(&self, key: &str) -> bool {
        match self.get(key) {
            Some(value) => matches!(value.trim().to_ascii_lowercase().as_str(), "true"),
            None => default_for(key) == Some("true"),
        }
    }

    pub fn integer(&self, key: &str) -> i32 {
        self.get(key)
            .and_then(|value| value.trim().parse().ok())
            .or_else(|| default_for(key).and_then(|value| value.parse().ok()))
            .unwrap_or_default()
    }

    /// The keys this file carries that the server ignores, for a startup notice.
    pub fn unhonoured_keys(&self) -> Vec<&str> {
        self.entries
            .iter()
            .map(|(key, _)| key.as_str())
            .filter(|key| !HONOURED.contains(key))
            .collect()
    }
}

/// One `key=value` line. Comments (`#`, `!`) and blank lines are skipped, and the
/// value keeps everything after the first `=`, so values may contain `=`.
fn parse_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('!') {
        return None;
    }
    let (key, value) = trimmed.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), unescape(value.trim())))
}

/// Undo the escaping Java's `Properties` applies, so a file copied from a vanilla
/// server loads with the same values it has there.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some(':') => out.push(':'),
                Some('=') => out.push('='),
                Some('\\') => out.push('\\'),
                Some('#') => out.push('#'),
                Some('!') => out.push('!'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(name: &str, contents: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("loadstone-props-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn a_missing_file_is_a_first_run() {
        let path = std::env::temp_dir().join("loadstone-does-not-exist-xyz.properties");
        let _ = std::fs::remove_file(&path);
        assert!(ServerProperties::load(&path).unwrap().is_none());
    }

    #[test]
    fn defaults_match_the_vanilla_set() {
        let props = ServerProperties::new();
        assert_eq!(props.string("level-name"), "world");
        assert_eq!(props.string("motd"), "A Minecraft Server");
        assert_eq!(props.integer("server-port"), 25565);
        assert_eq!(props.integer("max-players"), 20);
        assert_eq!(props.integer("view-distance"), 10);
        assert!(props.boolean("online-mode"));
        assert!(!props.boolean("hardcore"));
        // 1.21.11's file has neither of these: `pvp` became a game rule, and
        // `enable-command-block` is gone from the key set entirely. Writing them
        // on a first run would make this file differ from vanilla's.
        assert_eq!(props.get("pvp"), None);
        assert_eq!(props.get("enable-command-block"), None);
        assert!(props.optional_string("server-ip").is_none());
    }

    #[test]
    fn comments_blank_lines_and_equals_in_values_are_handled() {
        let path = write_temp(
            "messy.properties",
            "# a comment\n\n! also a comment\nmotd=Hello = world\nserver-port=25570\n",
        );
        let props = ServerProperties::load(&path).unwrap().unwrap();
        assert_eq!(props.string("motd"), "Hello = world");
        assert_eq!(props.integer("server-port"), 25570);
        // Missing keys are filled in rather than left absent.
        assert_eq!(props.string("level-name"), "world");
    }

    #[test]
    fn java_escaped_values_from_a_vanilla_file_load_unescaped() {
        let path = write_temp("escaped.properties", "level-type=minecraft\\:normal\n");
        let props = ServerProperties::load(&path).unwrap().unwrap();
        assert_eq!(props.string("level-type"), "minecraft:normal");
    }

    #[test]
    fn custom_values_and_unknown_keys_survive_a_save() {
        let path = write_temp(
            "custom.properties",
            "motd=Custom\nsome-future-key=keep me\n",
        );
        let props = ServerProperties::load(&path).unwrap().unwrap();

        let out = std::env::temp_dir().join(format!(
            "loadstone-props-{}/rewritten.properties",
            std::process::id()
        ));
        props.save(&out).unwrap();
        let reloaded = ServerProperties::load(&out).unwrap().unwrap();
        assert_eq!(reloaded.string("motd"), "Custom");
        assert_eq!(reloaded.get("some-future-key"), Some("keep me"));
        // And the rewritten file is still readable as a whole.
        assert_eq!(reloaded.integer("server-port"), 25565);
    }

    #[test]
    fn saving_and_loading_is_a_fixed_point() {
        let props = ServerProperties::new();
        let path = std::env::temp_dir().join(format!(
            "loadstone-props-{}/roundtrip.properties",
            std::process::id()
        ));
        props.save(&path).unwrap();
        let reloaded = ServerProperties::load(&path).unwrap().unwrap();
        // The second save must be byte-identical, or every start would rewrite
        // the file and show up as a spurious diff in a panel.
        let first = std::fs::read_to_string(&path).unwrap();
        reloaded.save(&path).unwrap();
        assert_eq!(first, std::fs::read_to_string(&path).unwrap());
        assert_eq!(props.entries.len(), reloaded.entries.len());
    }

    #[test]
    fn garbage_values_fall_back_to_the_default_instead_of_failing() {
        let path = write_temp("garbage.properties", "max-players=lots\nserver-port=\n");
        let props = ServerProperties::load(&path).unwrap().unwrap();
        assert_eq!(props.integer("max-players"), 20);
        // An empty server-port is unset, which is the vanilla default anyway.
        assert_eq!(props.integer("server-port"), 25565);
    }

    /// The reference is the file a real 1.21.11 server writes for itself, not a
    /// documentation page: docs lag versions, and this list has already drifted
    /// once in each direction — a key vanilla dropped was still here, and a whole
    /// family added since (the management server, transfers, the pause timer) was
    /// missing, so a fresh install was not vanilla's key set at all.
    #[test]
    fn gamemode_names_map_to_the_ids_login_carries() {
        for name in ["survival", "creative", "adventure", "spectator"] {
            let id = gamemode_id(name).expect("a known gamemode");
            assert_eq!(gamemode_name(id), name, "{name} did not round-trip");
        }

        assert_eq!(gamemode_id("survival"), Some(0));
        assert_eq!(gamemode_id("creative"), Some(1));
        assert_eq!(gamemode_id("adventure"), Some(2));
        assert_eq!(gamemode_id("spectator"), Some(3));
        // Vanilla writes it in lower case, and `hardcore` is a separate key rather
        // than a mode, so neither of these is a mode to guess at.
        assert_eq!(gamemode_id("Creative"), None);
        assert_eq!(gamemode_id("hardcore"), None);
    }

    #[test]
    fn the_key_set_is_the_one_a_vanilla_server_writes() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/data/vanilla-server.properties"
        );
        let text =
            std::fs::read_to_string(path).expect("the captured vanilla file ships with the tests");
        let vanilla: Vec<(String, String)> = text.lines().filter_map(parse_line).collect();
        assert!(!vanilla.is_empty(), "the fixture did not parse");

        assert_eq!(
            DEFAULTS.len(),
            vanilla.len(),
            "the default key count has to match vanilla's file"
        );
        for ((key, value), (vanilla_key, vanilla_value)) in DEFAULTS.iter().zip(&vanilla) {
            assert_eq!(
                key, vanilla_key,
                "vanilla writes its keys in this order, so a first run should too"
            );
            if *key == "management-server-secret" {
                // Random per install, so there is nothing to compare it with: the
                // fixture has one because a real server wrote it, and this server
                // leaves it empty because it has no management server to protect.
                assert_eq!(
                    *value, "",
                    "this server has no management server to give a secret"
                );
                assert!(
                    !vanilla_value.is_empty(),
                    "vanilla generates a secret on the first run"
                );
                continue;
            }
            assert_eq!(value, vanilla_value, "{key} has the wrong default");
        }
    }

    #[test]
    fn honoured_keys_are_a_subset_of_the_real_ones() {
        let props = ServerProperties::new();
        let unhonoured = props.unhonoured_keys();
        for key in HONOURED {
            assert!(
                default_for(key).is_some(),
                "{key} is honoured but is not a vanilla key"
            );
            assert!(
                !unhonoured.contains(key),
                "{key} is honoured but reported as ignored"
            );
        }
        assert!(
            unhonoured.len() > HONOURED.len(),
            "the list of keys we do not act on yet should be the larger one"
        );
    }
}
