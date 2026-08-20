use anyhow::{Result, anyhow};
use std::fs;
use std::path::{Path, PathBuf};

/// Load environment overrides for the CLI.
///
/// Security: unlike the previous `dotenvy::dotenv()` call, this does NOT search
/// the current directory (or its ancestors) for a `.env` file. Overrides come
/// only from the trusted config dir (`~/.txio/.env`) plus an explicit
/// `--env-file` opt-in.
///
/// Precedence (highest wins): pre-existing process env vars, then `--env-file`,
/// then `~/.txio/.env`. dotenvy's `from_path` is non-override (it sets a var
/// only when it is not already present), so loading the explicit file first
/// makes it win over the trusted default, while real process env — set before
/// either loader runs — always wins over both.
pub fn load_environment(explicit_env_file: Option<&Path>) -> Result<()> {
    let mut trusted = get_config_dir();
    trusted.push(".env");
    let cwd_env = Path::new(".env");
    load_env_files(explicit_env_file, &trusted, cwd_env)
}

fn load_env_files(explicit: Option<&Path>, trusted_env: &Path, cwd_env: &Path) -> Result<()> {
    // Explicit opt-in first so it wins over the trusted default (dotenvy is
    // non-override: first loader to set a var wins). An explicitly requested
    // file that cannot be loaded is a hard error, not a silent shrug.
    if let Some(path) = explicit {
        dotenvy::from_path(path)
            .map_err(|e| anyhow!("failed to load --env-file '{}': {}", path.display(), e))?;
    }

    // Trusted config dir: best-effort, silent when absent.
    if trusted_env.exists() {
        let _ = dotenvy::from_path(trusted_env);
    }

    // Discoverability: a planted `./.env` is never auto-loaded; if one is present
    // and the user did not opt in, point them at the explicit flag.
    if should_warn_unloaded_cwd_env(explicit.is_some(), cwd_env) {
        eprintln!("warning: found ./.env but it was not loaded; pass --env-file .env to use it");
    }

    Ok(())
}

/// Whether to emit the "found ./.env but it was not loaded" discoverability
/// warning: only when the user did not pass `--env-file` and a `./.env` exists.
fn should_warn_unloaded_cwd_env(explicit_provided: bool, cwd_env: &Path) -> bool {
    !explicit_provided && cwd_env.exists()
}

pub fn get_config_dir() -> PathBuf {
    let mut path = dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push(".txio");
    if !path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            builder.mode(0o700);
            let _ = builder.create(&path);
        }
        #[cfg(not(unix))]
        {
            let _ = fs::create_dir_all(&path);
        }
    }
    path
}

pub fn save_current_chain(chain: &str) -> Result<()> {
    let mut path = get_config_dir();
    path.push("current_chain");
    fs::write(path, chain)?;
    Ok(())
}

pub fn get_current_chain() -> Option<String> {
    let mut path = get_config_dir();
    path.push("current_chain");
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

pub fn save_current_network(network: &str) -> Result<()> {
    write_network_file(&get_config_dir(), network)
}

/// Alias for `save_current_network`, matching the naming used elsewhere
/// (`save_config`/`save_current_chain`) for discoverability.
pub fn save_network(network: &str) -> Result<()> {
    save_current_network(network)
}

pub fn get_current_network() -> Option<String> {
    read_network_file(&get_config_dir())
}

/// Testable core of `save_current_network`: takes the config dir explicitly
/// so tests don't need to touch the real `HOME`-derived directory.
pub(crate) fn write_network_file(dir: &Path, network: &str) -> Result<()> {
    fs::write(dir.join("current_network"), network)?;
    Ok(())
}

/// Testable core of `get_current_network`. A missing or unreadable file
/// (including "a directory sits where the file should be") is just `None`.
pub(crate) fn read_network_file(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join("current_network"))
        .ok()
        .map(|s| s.trim().to_string())
}

fn write_file_secure(path: &Path, contents: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;

        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(contents.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, contents)?;
    }

    Ok(())
}

pub fn save_token(token: &str) -> Result<()> {
    let mut path = get_config_dir();
    path.push("token");
    write_file_secure(&path, token)
}

pub fn get_token() -> Option<String> {
    let mut path = get_config_dir();
    path.push("token");
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

pub fn remove_token() -> Result<()> {
    let mut path = get_config_dir();
    path.push("token");
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub fn save_config(key: &str, value: &str) -> Result<()> {
    let mut path = get_config_dir();
    path.push("config.json");
    let mut map: serde_json::Map<String, serde_json::Value> = if path.exists() {
        let content = fs::read_to_string(&path)?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    map.insert(
        key.to_string(),
        serde_json::Value::String(value.to_string()),
    );
    write_file_secure(&path, &serde_json::to_string_pretty(&map)?)
}

pub fn get_config(key: &str) -> Result<Option<String>> {
    let mut path = get_config_dir();
    path.push("config.json");
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)?;
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&content).unwrap_or_default();
    Ok(map.get(key).and_then(|v| v.as_str()).map(|s| s.to_string()))
}

pub fn list_config() -> Result<Vec<(String, String)>> {
    let mut path = get_config_dir();
    path.push("config.json");
    if !path.exists() {
        return Ok(vec![]);
    }
    let content = fs::read_to_string(&path)?;
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&content).unwrap_or_default();
    Ok(map
        .into_iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
        .collect())
}

pub fn remove_config(key: &str) -> Result<()> {
    let mut path = get_config_dir();
    path.push("config.json");
    if !path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(&path)?;
    let mut map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&content).unwrap_or_default();
    map.remove(key);
    fs::write(path, serde_json::to_string_pretty(&map)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Env vars are process-global. Serialize every test that reads or writes the
    // process environment so parallel test threads can't observe each other's
    // mutations. Combined with per-test unique var KEYS, this keeps the suite
    // deterministic.
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A unique-per-call scratch directory under the OS temp dir.
    fn unique_dir(tag: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut d = std::env::temp_dir();
        d.push(format!(
            "txio_env_test_{}_{}_{}",
            tag,
            std::process::id(),
            n
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A unique env var key so concurrent tests never collide on the same name.
    fn unique_key(tag: &str) -> String {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        format!("TXIO_TEST_{}_{}_{}", tag, std::process::id(), n)
    }

    fn write_file(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        p
    }

    #[test]
    fn loads_from_trusted_location() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = unique_dir("trusted");
        let key = unique_key("TRUSTED");
        let trusted = write_file(&dir, ".env", &format!("{key}=from_trusted\n"));
        let missing_cwd = dir.join("nope.env");

        load_env_files(None, &trusted, &missing_cwd).unwrap();

        assert_eq!(std::env::var(&key).unwrap(), "from_trusted");
        unsafe {
            std::env::remove_var(&key);
        }
    }

    #[test]
    fn does_not_load_cwd_env_by_default() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = unique_dir("cwd");
        let key = unique_key("CWD");
        // A ".env" sitting where the CWD file would be must never be read for values.
        let cwd_env = write_file(&dir, ".env", &format!("{key}=planted\n"));
        let missing_trusted = dir.join("trusted.env");

        load_env_files(None, &missing_trusted, &cwd_env).unwrap();

        assert!(
            std::env::var(&key).is_err(),
            "a CWD .env must not be loaded without --env-file"
        );
    }

    #[test]
    fn loads_explicit_env_file() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = unique_dir("explicit");
        let key = unique_key("EXPLICIT");
        let explicit = write_file(&dir, "custom.env", &format!("{key}=from_explicit\n"));
        let missing_trusted = dir.join("trusted.env");
        let missing_cwd = dir.join("nope.env");

        load_env_files(Some(&explicit), &missing_trusted, &missing_cwd).unwrap();

        assert_eq!(std::env::var(&key).unwrap(), "from_explicit");
        unsafe {
            std::env::remove_var(&key);
        }
    }

    #[test]
    fn missing_explicit_env_file_is_an_error() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = unique_dir("missing");
        let explicit = dir.join("does_not_exist.env");
        let missing_trusted = dir.join("trusted.env");
        let missing_cwd = dir.join("nope.env");

        let result = load_env_files(Some(&explicit), &missing_trusted, &missing_cwd);
        assert!(
            result.is_err(),
            "an explicitly requested missing file must error"
        );
    }

    #[test]
    fn explicit_env_file_wins_over_trusted() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = unique_dir("precedence");
        let key = unique_key("PRECEDENCE");
        let explicit = write_file(&dir, "explicit.env", &format!("{key}=from_explicit\n"));
        let trusted = write_file(&dir, ".env", &format!("{key}=from_trusted\n"));
        let missing_cwd = dir.join("nope.env");

        load_env_files(Some(&explicit), &trusted, &missing_cwd).unwrap();

        assert_eq!(
            std::env::var(&key).unwrap(),
            "from_explicit",
            "--env-file must take precedence over the trusted default"
        );
        unsafe {
            std::env::remove_var(&key);
        }
    }

    #[test]
    fn preexisting_env_var_is_never_clobbered() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = unique_dir("preexisting");
        let key = unique_key("PREEXISTING");
        unsafe {
            std::env::set_var(&key, "real_value");
        }

        let explicit = write_file(&dir, "explicit.env", &format!("{key}=from_explicit\n"));
        let trusted = write_file(&dir, ".env", &format!("{key}=from_trusted\n"));
        let missing_cwd = dir.join("nope.env");

        load_env_files(Some(&explicit), &trusted, &missing_cwd).unwrap();

        assert_eq!(
            std::env::var(&key).unwrap(),
            "real_value",
            "a pre-existing process env var must survive both loaders"
        );
        unsafe {
            std::env::remove_var(&key);
        }
    }

    #[test]
    fn warns_only_when_cwd_env_present_and_no_opt_in() {
        let dir = unique_dir("warn");
        let present = write_file(&dir, ".env", "X=1\n");
        let absent = dir.join("nope.env");

        // Warn: no opt-in and a ./.env exists.
        assert!(should_warn_unloaded_cwd_env(false, &present));
        // No warn: user opted in via --env-file.
        assert!(!should_warn_unloaded_cwd_env(true, &present));
        // No warn: no ./.env exists.
        assert!(!should_warn_unloaded_cwd_env(false, &absent));
    }

    #[test]
    #[cfg(unix)]
    fn config_dir_created_with_mode_0700() {
        use std::os::unix::fs::PermissionsExt;

        let _g = ENV_LOCK.lock().unwrap();
        let temp_home = unique_dir("config_dir_mode");
        let old_home = std::env::var_os("HOME");

        unsafe {
            std::env::set_var("HOME", &temp_home);
        }

        let config_dir = get_config_dir();

        let mode = fs::metadata(&config_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "config dir must have mode 0o700");

        match old_home {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[test]
    #[cfg(unix)]
    fn save_token_creates_secure_file() {
        use std::os::unix::fs::PermissionsExt;

        let _g = ENV_LOCK.lock().unwrap();
        let temp_home = unique_dir("token_mode");
        let old_home = std::env::var_os("HOME");

        unsafe {
            std::env::set_var("HOME", &temp_home);
        }

        let test_token = "test_jwt_token_content";
        save_token(test_token).unwrap();

        let config_dir = temp_home.join(".txio");
        let token_path = config_dir.join("token");

        let dir_mode = fs::metadata(&config_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "config dir must have mode 0o700");

        let file_mode = fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "token file must have mode 0o600");

        let content = fs::read_to_string(&token_path).unwrap();
        assert_eq!(content, test_token, "token file must contain exact token");

        match old_home {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[test]
    #[cfg(unix)]
    fn save_token_secures_existing_insecure_file() {
        use std::os::unix::fs::PermissionsExt;

        let _g = ENV_LOCK.lock().unwrap();
        let temp_home = unique_dir("token_secure_existing");
        let old_home = std::env::var_os("HOME");

        // Create .txio directory with insecure permissions
        let config_dir = temp_home.join(".txio");
        fs::create_dir_all(&config_dir).unwrap();

        // Create token file with insecure permissions (0644) and old content
        let token_path = config_dir.join("token");
        fs::write(&token_path, "old_token").unwrap();
        fs::set_permissions(&token_path, fs::Permissions::from_mode(0o644)).unwrap();

        // Verify it's initially insecure
        let initial_mode = fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(initial_mode, 0o644, "test setup: token should start with 0o644");

        // Set HOME and call save_token
        unsafe {
            std::env::set_var("HOME", &temp_home);
        }

        save_token("new_token").unwrap();

        // Verify permissions are now 0600
        let final_mode = fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(final_mode, 0o600, "save_token must secure existing token file to 0o600");

        // Verify content is the new token
        let content = fs::read_to_string(&token_path).unwrap();
        assert_eq!(content, "new_token", "save_token must replace with new token content");

        match old_home {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[test]
    fn save_and_get_current_network_round_trip() {
        let _g = ENV_LOCK.lock().unwrap();
        let temp_home = unique_dir("network_persist");
        let old_home = std::env::var_os("HOME");

        unsafe {
            std::env::set_var("HOME", &temp_home);
        }

        assert_eq!(get_current_network(), None);

        save_current_network("testnet").unwrap();
        assert_eq!(get_current_network(), Some("testnet".to_string()));

        save_network("devnet").unwrap();
        assert_eq!(get_current_network(), Some("devnet".to_string()));

        match old_home {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[test]
    #[cfg(unix)]
    fn save_config_creates_secure_file() {
        use std::os::unix::fs::PermissionsExt;

        let _g = ENV_LOCK.lock().unwrap();
        let temp_home = unique_dir("config_mode");
        let old_home = std::env::var_os("HOME");

        unsafe {
            std::env::set_var("HOME", &temp_home);
        }

        save_config("test_key", "test_val").unwrap();

        let config_dir = temp_home.join(".txio");
        let config_path = config_dir.join("config.json");

        let file_mode = fs::metadata(&config_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "config file must have mode 0o600");

        let content = fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("test_val"));

        match old_home {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[test]
    #[cfg(unix)]
    fn save_config_secures_existing_insecure_file() {
        use std::os::unix::fs::PermissionsExt;

        let _g = ENV_LOCK.lock().unwrap();
        let temp_home = unique_dir("config_secure_existing");
        let old_home = std::env::var_os("HOME");

        let config_dir = temp_home.join(".txio");
        fs::create_dir_all(&config_dir).unwrap();

        let config_path = config_dir.join("config.json");
        fs::write(&config_path, "{}").unwrap();
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o644)).unwrap();

        let initial_mode = fs::metadata(&config_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(initial_mode, 0o644, "test setup: config should start with 0o644");

        unsafe {
            std::env::set_var("HOME", &temp_home);
        }

        save_config("new_key", "new_val").unwrap();

        let final_mode = fs::metadata(&config_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(final_mode, 0o600, "save_config must secure existing file to 0o600");

        let content = fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("new_val"));

        match old_home {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    // ── network persistence ────────────────────────────────────────────────

    #[test]
    fn network_file_round_trips() {
        let dir = unique_dir("netround");
        write_network_file(&dir, "testnet").unwrap();
        assert_eq!(read_network_file(&dir).as_deref(), Some("testnet"));
    }

    #[test]
    fn network_file_is_trimmed_on_read() {
        let dir = unique_dir("nettrim");
        write_network_file(&dir, "  devnet \n\n").unwrap();
        assert_eq!(read_network_file(&dir).as_deref(), Some("devnet"));
    }

    #[test]
    fn missing_or_corrupt_network_file_reads_as_none() {
        let dir = unique_dir("netmissing");
        assert_eq!(read_network_file(&dir), None);
        // A directory where the file should be is just as absent to the reader.
        let nested = dir.join("current_network");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(read_network_file(&dir), None);
    }
}
