use anyhow::{bail, Context, Result};

// ── Replay bundle helpers ─────────────────────────────────────────────────────
//
// Bundle format (plain text, one stdin line per line):
//   fidan-replay-v1\n
//   <line0>\n
//   <line1>\n
//   …
//
// Bundles are stored in ~/.fidan/replays/<id>.bundle where `id` is 8 lowercase
// hex digits derived from a hash of the source path + current Unix timestamp.

fn replay_dir() -> std::path::PathBuf {
    dirs_or_home().join(".fidan").join("replays")
}

fn dirs_or_home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

pub(crate) fn save_replay_bundle(source: &std::path::Path, lines: &[String]) -> Result<String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut h = DefaultHasher::new();
    source.hash(&mut h);
    ts.hash(&mut h);
    let id = format!("{:08x}", h.finish() & 0xFFFF_FFFF);

    let dir = replay_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create replay dir {:?}", dir))?;

    let path = dir.join(format!("{id}.bundle"));
    let mut content = String::from("fidan-replay-v1\n");
    for line in lines {
        content.push_str(line);
        content.push('\n');
    }
    std::fs::write(&path, &content)
        .with_context(|| format!("cannot write replay bundle {:?}", path))?;
    Ok(id)
}

pub(crate) fn load_replay_bundle(id_or_path: &str) -> Result<Vec<String>> {
    let path = if id_or_path.ends_with(".bundle") || id_or_path.contains(std::path::MAIN_SEPARATOR)
    {
        std::path::PathBuf::from(id_or_path)
    } else {
        replay_dir().join(format!("{id_or_path}.bundle"))
    };

    if !path.exists() {
        bail!("replay bundle not found: {:?}", path);
    }

    let content =
        std::fs::read_to_string(&path).with_context(|| format!("cannot read {:?}", path))?;
    let mut lines = content.lines();
    match lines.next() {
        Some("fidan-replay-v1") => {}
        _ => bail!("unrecognised replay bundle format in {:?}", path),
    }
    Ok(lines.map(str::to_string).collect())
}
