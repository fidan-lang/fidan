//! `std.io` — File and console I/O for Fidan.
//!
//! Available via:
//!   `use std.io`  → `io.readFile(path)`, `io.writeFile(path, content)`, `io.readLine()`, etc.
//!   `use std.io.{readFile, writeFile}` → free names in scope.

use fidan_runtime::{FidanString, FidanValue};

fn as_str(v: &FidanValue) -> String {
    match v {
        FidanValue::String(s) => s.as_str().to_string(),
        _ => String::new(),
    }
}

fn str_val(s: &str) -> FidanValue {
    FidanValue::String(FidanString::new(s))
}

/// Dispatch an `io.<name>(args)` call.
pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        // ── Console I/O ───────────────────────────────────────────────────
        "print" => {
            let parts: Vec<String> = args.iter().map(|v| format_val(v)).collect();
            println!("{}", parts.join(" "));
            Some(FidanValue::Nothing)
        }
        "println" => {
            let parts: Vec<String> = args.iter().map(|v| format_val(v)).collect();
            println!("{}", parts.join(" "));
            Some(FidanValue::Nothing)
        }
        "eprint" => {
            let parts: Vec<String> = args.iter().map(|v| format_val(v)).collect();
            eprintln!("{}", parts.join(" "));
            Some(FidanValue::Nothing)
        }
        "readLine" | "read_line" | "readline" => {
            use std::io::BufRead;
            let prompt = args.first().map(|v| format_val(v)).unwrap_or_default();
            if !prompt.is_empty() {
                use std::io::Write;
                print!("{}", prompt);
                let _ = std::io::stdout().flush();
            }
            let stdin = std::io::stdin();
            let mut line = String::new();
            stdin.lock().read_line(&mut line).ok()?;
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') { line.pop(); }
            }
            Some(str_val(&line))
        }

        // ── File I/O ──────────────────────────────────────────────────────
        "readFile" | "read_file" => {
            let path = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            match std::fs::read_to_string(&path) {
                Ok(content) => Some(str_val(&content)),
                Err(e) => {
                    eprintln!("io.readFile error: {e}");
                    Some(FidanValue::Nothing)
                }
            }
        }
        "writeFile" | "write_file" => {
            let path    = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let content = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            match std::fs::write(&path, content) {
                Ok(_)  => Some(FidanValue::Boolean(true)),
                Err(e) => {
                    eprintln!("io.writeFile error: {e}");
                    Some(FidanValue::Boolean(false))
                }
            }
        }
        "appendFile" | "append_file" => {
            use std::io::Write;
            let path    = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let content = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                Ok(mut f) => {
                    let _ = f.write_all(content.as_bytes());
                    Some(FidanValue::Boolean(true))
                }
                Err(e) => {
                    eprintln!("io.appendFile error: {e}");
                    Some(FidanValue::Boolean(false))
                }
            }
        }
        "deleteFile" | "delete_file" => {
            let path = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::fs::remove_file(&path).is_ok()))
        }
        "fileExists" | "file_exists" | "exists" => {
            let path = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::path::Path::new(&path).exists()))
        }
        "isFile" | "is_file" => {
            let path = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::path::Path::new(&path).is_file()))
        }
        "isDir" | "is_dir" => {
            let path = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::path::Path::new(&path).is_dir()))
        }
        "makeDir" | "make_dir" | "mkdir" => {
            let path = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::fs::create_dir_all(&path).is_ok()))
        }
        "listDir" | "list_dir" | "readDir" | "read_dir" => {
            use fidan_runtime::{FidanList, OwnedRef};
            let path = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let mut list = FidanList::new();
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    list.append(str_val(&name));
                }
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "copyFile" | "copy_file" => {
            let from = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let to   = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::fs::copy(&from, &to).is_ok()))
        }
        "renameFile" | "rename_file" => {
            let from = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let to   = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::fs::rename(&from, &to).is_ok()))
        }

        // ── Path utilities ────────────────────────────────────────────────
        "join" | "joinPath" | "join_path" => {
            let mut path = std::path::PathBuf::new();
            for arg in &args {
                path.push(as_str(arg));
            }
            Some(str_val(&path.to_string_lossy()))
        }
        "dirname" | "dir_name" => {
            let path = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let p = std::path::Path::new(&path);
            let dir = p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
            Some(str_val(&dir))
        }
        "basename" | "base_name" | "fileName" | "file_name" => {
            let path = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let p = std::path::Path::new(&path);
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            Some(str_val(&name))
        }
        "extension" => {
            let path = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let p = std::path::Path::new(&path);
            let ext = p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
            Some(str_val(&ext))
        }
        "cwd" | "currentDir" | "current_dir" => {
            let dir = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            Some(str_val(&dir))
        }
        "absolutePath" | "absolute_path" => {
            let path = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let abs = std::fs::canonicalize(&path)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or(path);
            Some(str_val(&abs))
        }

        // ── Env ───────────────────────────────────────────────────────────
        "getEnv" | "get_env" | "env" => {
            let key = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            match std::env::var(&key) {
                Ok(val) => Some(str_val(&val)),
                Err(_)  => Some(FidanValue::Nothing),
            }
        }
        "setEnv" | "set_env" => {
            let key = as_str(args.first().unwrap_or(&FidanValue::Nothing));
            let val = as_str(args.get(1).unwrap_or(&FidanValue::Nothing));
            // SAFETY: single-threaded interpreter; no concurrent env access.
            #[allow(unused_unsafe)]
            unsafe { std::env::set_var(&key, &val) };
            Some(FidanValue::Nothing)
        }
        "args" | "argv" => {
            use fidan_runtime::{FidanList, OwnedRef};
            let mut list = FidanList::new();
            for a in std::env::args() {
                list.append(str_val(&a));
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }

        // ── Misc ──────────────────────────────────────────────────────────
        "flush" => {
            use std::io::Write;
            let _ = std::io::stdout().flush();
            Some(FidanValue::Nothing)
        }
        _ => None,
    }
}

fn format_val(v: &FidanValue) -> String {
    match v {
        FidanValue::String(s) => s.as_str().to_string(),
        FidanValue::Integer(n) => n.to_string(),
        FidanValue::Float(f) => {
            if f.fract() == 0.0 { format!("{:.1}", f) } else { f.to_string() }
        }
        FidanValue::Boolean(b) => b.to_string(),
        FidanValue::Nothing => "nothing".to_string(),
        FidanValue::List(_) => "[list]".to_string(),
        FidanValue::Dict(_) => "{dict}".to_string(),
        FidanValue::Object(_) => "[object]".to_string(),
        _ => "[value]".to_string(),
    }
}

pub fn exported_names() -> &'static [&'static str] {
    &[
        "print", "println", "eprint", "readLine", "read_line", "readline",
        "readFile", "read_file", "writeFile", "write_file",
        "appendFile", "append_file", "deleteFile", "delete_file",
        "fileExists", "file_exists", "exists", "isFile", "is_file", "isDir", "is_dir",
        "makeDir", "make_dir", "mkdir",
        "listDir", "list_dir", "readDir", "read_dir",
        "copyFile", "copy_file", "renameFile", "rename_file",
        "join", "joinPath", "join_path", "dirname", "dir_name",
        "basename", "base_name", "fileName", "file_name", "extension",
        "cwd", "currentDir", "current_dir", "absolutePath", "absolute_path",
        "getEnv", "get_env", "env", "setEnv", "set_env", "args", "argv",
        "flush",
    ]
}
