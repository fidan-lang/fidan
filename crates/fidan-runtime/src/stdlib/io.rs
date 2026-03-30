use crate::{FidanList, FidanValue, OwnedRef, display as format_val};

use super::common::{coerce_string, display_string, string_value};

pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        "print" => {
            let parts: Vec<String> = args.iter().map(format_val).collect();
            println!("{}", parts.join(" "));
            Some(FidanValue::Nothing)
        }
        "eprint" => {
            let parts: Vec<String> = args.iter().map(format_val).collect();
            eprintln!("{}", parts.join(" "));
            Some(FidanValue::Nothing)
        }
        "readLine" | "read_line" | "readline" => {
            use std::io::BufRead;
            let prompt = args.first().map(display_string).unwrap_or_default();
            if !prompt.is_empty() {
                use std::io::Write;
                print!("{prompt}");
                let _ = std::io::stdout().flush();
            }
            let stdin = std::io::stdin();
            let mut line = String::new();
            stdin.lock().read_line(&mut line).ok()?;
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            Some(string_value(&line))
        }
        "readFile" | "read_file" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            match std::fs::read_to_string(&path) {
                Ok(content) => Some(string_value(&content)),
                Err(_) => Some(FidanValue::Nothing),
            }
        }
        "readLines" | "read_lines" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    let mut list = FidanList::new();
                    for line in content.lines() {
                        list.append(string_value(line));
                    }
                    Some(FidanValue::List(OwnedRef::new(list)))
                }
                Err(_) => Some(FidanValue::Nothing),
            }
        }
        "writeFile" | "write_file" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let content = display_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::fs::write(&path, content).is_ok()))
        }
        "appendFile" | "append_file" => {
            use std::io::Write;
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let content = display_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                Ok(mut file) => Some(FidanValue::Boolean(
                    file.write_all(content.as_bytes()).is_ok(),
                )),
                Err(_) => Some(FidanValue::Boolean(false)),
            }
        }
        "deleteFile" | "delete_file" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::fs::remove_file(&path).is_ok()))
        }
        "fileExists" | "file_exists" | "exists" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::path::Path::new(&path).exists()))
        }
        "isFile" | "is_file" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::path::Path::new(&path).is_file()))
        }
        "isDir" | "is_dir" | "isDirectory" | "is_directory" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::path::Path::new(&path).is_dir()))
        }
        "makeDir" | "make_dir" | "mkdir" | "createDir" | "create_dir" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::fs::create_dir_all(&path).is_ok()))
        }
        "listDir" | "list_dir" | "readDir" | "read_dir" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let mut list = FidanList::new();
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    list.append(string_value(&entry.file_name().to_string_lossy()));
                }
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "copyFile" | "copy_file" => {
            let from = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let to = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::fs::copy(&from, &to).is_ok()))
        }
        "renameFile" | "rename_file" | "moveFile" | "move_file" => {
            let from = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let to = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(FidanValue::Boolean(std::fs::rename(&from, &to).is_ok()))
        }
        "join" | "joinPath" | "join_path" => {
            let mut path = std::path::PathBuf::new();
            for arg in &args {
                path.push(coerce_string(arg));
            }
            Some(string_value(&path.to_string_lossy()))
        }
        "dirname" | "dir_name" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let dir = std::path::Path::new(&path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            Some(string_value(&dir))
        }
        "basename" | "base_name" | "fileName" | "file_name" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            Some(string_value(&name))
        }
        "extension" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let ext = std::path::Path::new(&path)
                .extension()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            Some(string_value(&ext))
        }
        "cwd" | "currentDir" | "current_dir" => {
            let dir = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            Some(string_value(&dir))
        }
        "absolutePath" | "absolute_path" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let abs = std::fs::canonicalize(&path)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or(path);
            Some(string_value(&abs))
        }
        "getEnv" | "get_env" | "env" => {
            let key = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            match std::env::var(&key) {
                Ok(value) => Some(string_value(&value)),
                Err(_) => Some(FidanValue::Nothing),
            }
        }
        "setEnv" | "set_env" => {
            let key = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let value = display_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            #[allow(unused_unsafe)]
            unsafe {
                std::env::set_var(&key, &value)
            };
            Some(FidanValue::Nothing)
        }
        "args" | "argv" => {
            let mut list = FidanList::new();
            for arg in std::env::args() {
                list.append(string_value(&arg));
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "flush" => {
            use std::io::Write;
            let _ = std::io::stdout().flush();
            Some(FidanValue::Nothing)
        }
        "isatty" => {
            use std::io::IsTerminal;
            let stream = args.first().map(coerce_string).unwrap_or_default();
            let tty = match stream.as_str() {
                "stdin" => std::io::stdin().is_terminal(),
                "stderr" => std::io::stderr().is_terminal(),
                _ => std::io::stdout().is_terminal(),
            };
            Some(FidanValue::Boolean(tty))
        }
        _ => None,
    }
}

pub fn exported_names() -> &'static [&'static str] {
    &[
        "print",
        "eprint",
        "readLine",
        "read_line",
        "readline",
        "readFile",
        "read_file",
        "readLines",
        "read_lines",
        "writeFile",
        "write_file",
        "appendFile",
        "append_file",
        "deleteFile",
        "delete_file",
        "fileExists",
        "file_exists",
        "exists",
        "isFile",
        "is_file",
        "isDir",
        "is_dir",
        "isDirectory",
        "is_directory",
        "makeDir",
        "make_dir",
        "mkdir",
        "createDir",
        "create_dir",
        "listDir",
        "list_dir",
        "readDir",
        "read_dir",
        "copyFile",
        "copy_file",
        "renameFile",
        "rename_file",
        "moveFile",
        "move_file",
        "join",
        "joinPath",
        "join_path",
        "dirname",
        "dir_name",
        "basename",
        "base_name",
        "fileName",
        "file_name",
        "extension",
        "cwd",
        "currentDir",
        "current_dir",
        "absolutePath",
        "absolute_path",
        "getEnv",
        "get_env",
        "env",
        "setEnv",
        "set_env",
        "args",
        "argv",
        "flush",
        "isatty",
    ]
}
