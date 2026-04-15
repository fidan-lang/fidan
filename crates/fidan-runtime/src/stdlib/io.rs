use crate::value::display_into;
use crate::{FidanList, FidanValue, OwnedRef, current_program_args};
use fidan_diagnostics::{DiagCode, diag_code};

use super::StdlibRuntimeError;
use super::common::{coerce_string, display_string, string_value};

fn io_runtime_error(
    non_permission_code: DiagCode,
    message: String,
    err: &std::io::Error,
) -> StdlibRuntimeError {
    let code = if err.kind() == std::io::ErrorKind::PermissionDenied {
        diag_code!("R3004")
    } else {
        non_permission_code
    };
    StdlibRuntimeError::new(code, message)
}

fn read_file_text(path: &str) -> Result<String, StdlibRuntimeError> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(|err| {
        io_runtime_error(
            diag_code!("R3001"),
            format!("failed to open file `{path}`: {err}"),
            &err,
        )
    })?;

    let mut text = String::new();
    file.read_to_string(&mut text).map_err(|err| {
        io_runtime_error(
            diag_code!("R3002"),
            format!("failed to read file `{path}`: {err}"),
            &err,
        )
    })?;

    Ok(text)
}

fn read_file_lines(path: &str) -> Result<FidanValue, StdlibRuntimeError> {
    let content = read_file_text(path)?;
    let lines = content.lines();
    let (lower, upper) = lines.size_hint();
    let mut list = FidanList::with_capacity(upper.unwrap_or(lower));
    for line in lines {
        list.append(string_value(line));
    }
    Ok(FidanValue::List(OwnedRef::new(list)))
}

fn write_file_text(path: &str, content: &str) -> Result<FidanValue, StdlibRuntimeError> {
    std::fs::write(path, content).map_err(|err| {
        io_runtime_error(
            diag_code!("R3003"),
            format!("failed to write file `{path}`: {err}"),
            &err,
        )
    })?;
    Ok(FidanValue::Boolean(true))
}

fn append_file_text(path: &str, content: &str) -> Result<FidanValue, StdlibRuntimeError> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| {
            io_runtime_error(
                diag_code!("R3003"),
                format!("failed to open file `{path}` for append: {err}"),
                &err,
            )
        })?;
    file.write_all(content.as_bytes()).map_err(|err| {
        io_runtime_error(
            diag_code!("R3003"),
            format!("failed to append file `{path}`: {err}"),
            &err,
        )
    })?;
    Ok(FidanValue::Boolean(true))
}

fn delete_file(path: &str) -> Result<FidanValue, StdlibRuntimeError> {
    std::fs::remove_file(path).map_err(|err| {
        io_runtime_error(
            diag_code!("R3009"),
            format!("failed to delete file `{path}`: {err}"),
            &err,
        )
    })?;
    Ok(FidanValue::Boolean(true))
}

fn create_dir_all(path: &str) -> Result<FidanValue, StdlibRuntimeError> {
    std::fs::create_dir_all(path).map_err(|err| {
        io_runtime_error(
            diag_code!("R3010"),
            format!("failed to create directory `{path}`: {err}"),
            &err,
        )
    })?;
    Ok(FidanValue::Boolean(true))
}

fn list_dir_names(path: &str) -> Result<FidanValue, StdlibRuntimeError> {
    let entries = std::fs::read_dir(path).map_err(|err| {
        io_runtime_error(
            diag_code!("R3006"),
            format!("failed to list directory `{path}`: {err}"),
            &err,
        )
    })?;

    let mut list = FidanList::new();
    for entry in entries {
        let entry = entry.map_err(|err| {
            io_runtime_error(
                diag_code!("R3006"),
                format!("failed to read directory entry in `{path}`: {err}"),
                &err,
            )
        })?;
        list.append(string_value(&entry.file_name().to_string_lossy()));
    }
    Ok(FidanValue::List(OwnedRef::new(list)))
}

fn copy_file(from: &str, to: &str) -> Result<FidanValue, StdlibRuntimeError> {
    std::fs::copy(from, to).map_err(|err| {
        io_runtime_error(
            diag_code!("R3007"),
            format!("failed to copy `{from}` to `{to}`: {err}"),
            &err,
        )
    })?;
    Ok(FidanValue::Boolean(true))
}

fn rename_file(from: &str, to: &str) -> Result<FidanValue, StdlibRuntimeError> {
    std::fs::rename(from, to).map_err(|err| {
        io_runtime_error(
            diag_code!("R3008"),
            format!("failed to rename `{from}` to `{to}`: {err}"),
            &err,
        )
    })?;
    Ok(FidanValue::Boolean(true))
}

pub fn dispatch_result(
    name: &str,
    args: Vec<FidanValue>,
) -> Option<Result<FidanValue, StdlibRuntimeError>> {
    match name {
        "print" => {
            let mut rendered = String::new();
            for (index, value) in args.iter().enumerate() {
                if index > 0 {
                    rendered.push(' ');
                }
                display_into(&mut rendered, value);
            }
            println!("{rendered}");
            Some(Ok(FidanValue::Nothing))
        }
        "eprint" => {
            let mut rendered = String::new();
            for (index, value) in args.iter().enumerate() {
                if index > 0 {
                    rendered.push(' ');
                }
                display_into(&mut rendered, value);
            }
            eprintln!("{rendered}");
            Some(Ok(FidanValue::Nothing))
        }
        "readLine" | "read_line" | "readline" => Some((|| {
            use std::io::BufRead;
            let prompt = args.first().map(display_string).unwrap_or_default();
            if !prompt.is_empty() {
                use std::io::Write;
                print!("{prompt}");
                let _ = std::io::stdout().flush();
            }
            let stdin = std::io::stdin();
            let mut line = String::new();
            stdin.lock().read_line(&mut line).map_err(|err| {
                StdlibRuntimeError::new(
                    diag_code!("R3002"),
                    format!("failed to read from stdin: {err}"),
                )
            })?;
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            Ok(string_value(&line))
        })()),
        "readFile" | "read_file" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(read_file_text(&path).map(|content| string_value(&content)))
        }
        "readLines" | "read_lines" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(read_file_lines(&path))
        }
        "writeFile" | "write_file" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let content = display_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(write_file_text(&path, &content))
        }
        "appendFile" | "append_file" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let content = display_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(append_file_text(&path, &content))
        }
        "deleteFile" | "delete_file" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(delete_file(&path))
        }
        "fileExists" | "file_exists" | "exists" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(Ok(FidanValue::Boolean(
                std::path::Path::new(&path).exists(),
            )))
        }
        "isFile" | "is_file" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(Ok(FidanValue::Boolean(
                std::path::Path::new(&path).is_file(),
            )))
        }
        "isDir" | "is_dir" | "isDirectory" | "is_directory" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(Ok(FidanValue::Boolean(
                std::path::Path::new(&path).is_dir(),
            )))
        }
        "makeDir" | "make_dir" | "mkdir" | "createDir" | "create_dir" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(create_dir_all(&path))
        }
        "listDir" | "list_dir" | "readDir" | "read_dir" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            Some(list_dir_names(&path))
        }
        "copyFile" | "copy_file" => {
            let from = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let to = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(copy_file(&from, &to))
        }
        "renameFile" | "rename_file" | "moveFile" | "move_file" => {
            let from = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let to = coerce_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            Some(rename_file(&from, &to))
        }
        "join" | "joinPath" | "join_path" => {
            let mut path = std::path::PathBuf::new();
            for arg in &args {
                path.push(coerce_string(arg));
            }
            Some(Ok(string_value(&path.to_string_lossy())))
        }
        "dirname" | "dir_name" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let dir = std::path::Path::new(&path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            Some(Ok(string_value(&dir)))
        }
        "basename" | "base_name" | "fileName" | "file_name" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            Some(Ok(string_value(&name)))
        }
        "extension" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let ext = std::path::Path::new(&path)
                .extension()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            Some(Ok(string_value(&ext)))
        }
        "cwd" | "currentDir" | "current_dir" => {
            let dir = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            Some(Ok(string_value(&dir)))
        }
        "absolutePath" | "absolute_path" => {
            let path = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let path_buf = std::path::PathBuf::from(&path);
            let abs = std::fs::canonicalize(&path_buf)
                .or_else(|_| {
                    if path_buf.is_absolute() {
                        Ok(path_buf.clone())
                    } else {
                        std::env::current_dir().map(|cwd| cwd.join(&path_buf))
                    }
                })
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or(path);
            Some(Ok(string_value(&abs)))
        }
        "getEnv" | "get_env" | "env" => {
            let key = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let value = match std::env::var(&key) {
                Ok(value) => string_value(&value),
                Err(_) => FidanValue::Nothing,
            };
            Some(Ok(value))
        }
        "setEnv" | "set_env" => {
            let key = coerce_string(args.first().unwrap_or(&FidanValue::Nothing));
            let value = display_string(args.get(1).unwrap_or(&FidanValue::Nothing));
            #[allow(unused_unsafe)]
            unsafe {
                std::env::set_var(&key, &value)
            };
            Some(Ok(FidanValue::Nothing))
        }
        "args" | "argv" => {
            let mut list = FidanList::new();
            for arg in current_program_args() {
                list.append(string_value(&arg));
            }
            Some(Ok(FidanValue::List(OwnedRef::new(list))))
        }
        "flush" => Some((|| {
            use std::io::Write;
            std::io::stdout().flush().map_err(|err| {
                StdlibRuntimeError::new(
                    diag_code!("R3003"),
                    format!("failed to flush stdout: {err}"),
                )
            })?;
            Ok(FidanValue::Nothing)
        })()),
        "isatty" => {
            use std::io::IsTerminal;
            let stream = args.first().map(coerce_string).unwrap_or_default();
            let tty = match stream.as_str() {
                "stdin" => std::io::stdin().is_terminal(),
                "stderr" => std::io::stderr().is_terminal(),
                _ => std::io::stdout().is_terminal(),
            };
            Some(Ok(FidanValue::Boolean(tty)))
        }
        _ => None,
    }
}

pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    dispatch_result(name, args)?.ok()
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

#[cfg(test)]
mod tests {
    use super::{dispatch, dispatch_result};
    use crate::FidanValue;
    use fidan_diagnostics::diag_code;

    fn string_arg(value: &str) -> FidanValue {
        FidanValue::String(crate::FidanString::new(value))
    }

    #[test]
    fn absolute_path_makes_missing_relative_paths_absolute() {
        let missing = "fidan-runtime-missing-path-check/file.txt";
        let value = dispatch("absolutePath", vec![string_arg(missing)]).expect("dispatch result");

        let FidanValue::String(path) = value else {
            panic!("expected absolutePath to return a string");
        };
        let path = std::path::PathBuf::from(path.as_str());
        assert!(
            path.is_absolute(),
            "expected absolute path, got {}",
            path.display()
        );
        assert!(path.ends_with(std::path::Path::new(missing)));
    }

    #[test]
    fn read_file_missing_returns_runtime_error() {
        let path = std::env::temp_dir().join("fidan-runtime-io-missing-file.txt");
        let path_str = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&path);

        let err = dispatch_result("readFile", vec![string_arg(&path_str)])
            .expect("dispatch result")
            .expect_err("expected missing file runtime error");
        assert_eq!(err.code, diag_code!("R3001"));
        assert!(err.message.contains("failed to open file"));
    }

    #[test]
    fn read_lines_missing_returns_runtime_error() {
        let path = std::env::temp_dir().join("fidan-runtime-io-missing-lines.txt");
        let path_str = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&path);

        let err = dispatch_result("readLines", vec![string_arg(&path_str)])
            .expect("dispatch result")
            .expect_err("expected missing file runtime error");
        assert_eq!(err.code, diag_code!("R3001"));
        assert!(err.message.contains("failed to open file"));
    }

    #[test]
    fn read_file_and_lines_return_contents() {
        let path = std::env::temp_dir().join("fidan-runtime-io-read-file.txt");
        std::fs::write(&path, "alpha\nbeta\n").expect("write fixture");
        let path_str = path.to_string_lossy().to_string();

        let file = dispatch_result("readFile", vec![string_arg(&path_str)])
            .expect("dispatch result")
            .expect("expected file contents");
        let FidanValue::String(text) = file else {
            panic!("expected string result from readFile");
        };
        assert_eq!(text.as_str(), "alpha\nbeta\n");

        let lines = dispatch_result("readLines", vec![string_arg(&path_str)])
            .expect("dispatch result")
            .expect("expected line list");
        let FidanValue::List(lines) = lines else {
            panic!("expected list result from readLines");
        };
        let lines = lines.borrow();
        assert_eq!(lines.len(), 2);
        assert!(matches!(lines.get(0), Some(FidanValue::String(line)) if line.as_str() == "alpha"));
        assert!(matches!(lines.get(1), Some(FidanValue::String(line)) if line.as_str() == "beta"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn io_mutators_and_directory_listing_work() {
        let root = std::env::temp_dir().join("fidan-runtime-io-mutators");
        let nested = root.join("nested");
        let file_a = root.join("a.txt");
        let file_b = root.join("b.txt");
        let file_c = root.join("c.txt");
        let _ = std::fs::remove_dir_all(&root);

        assert!(matches!(
            dispatch_result("makeDir", vec![string_arg(&nested.to_string_lossy())])
                .expect("dispatch result")
                .expect("expected makeDir success"),
            FidanValue::Boolean(true)
        ));
        assert!(matches!(
            dispatch_result(
                "writeFile",
                vec![string_arg(&file_a.to_string_lossy()), string_arg("hello")],
            )
            .expect("dispatch result")
            .expect("expected writeFile success"),
            FidanValue::Boolean(true)
        ));
        assert!(matches!(
            dispatch_result(
                "appendFile",
                vec![string_arg(&file_a.to_string_lossy()), string_arg(" world")],
            )
            .expect("dispatch result")
            .expect("expected appendFile success"),
            FidanValue::Boolean(true)
        ));
        assert!(matches!(
            dispatch_result(
                "copyFile",
                vec![
                    string_arg(&file_a.to_string_lossy()),
                    string_arg(&file_b.to_string_lossy()),
                ],
            )
            .expect("dispatch result")
            .expect("expected copyFile success"),
            FidanValue::Boolean(true)
        ));
        assert!(matches!(
            dispatch_result(
                "renameFile",
                vec![
                    string_arg(&file_b.to_string_lossy()),
                    string_arg(&file_c.to_string_lossy()),
                ],
            )
            .expect("dispatch result")
            .expect("expected renameFile success"),
            FidanValue::Boolean(true)
        ));

        let listing = dispatch_result("listDir", vec![string_arg(&root.to_string_lossy())])
            .expect("dispatch result")
            .expect("expected listDir success");
        let FidanValue::List(listing) = listing else {
            panic!("expected list result from listDir");
        };
        let listing = listing.borrow();
        assert!(
            listing.iter().any(
                |value| matches!(value, FidanValue::String(name) if name.as_str() == "nested")
            )
        );
        assert!(
            listing
                .iter()
                .any(|value| matches!(value, FidanValue::String(name) if name.as_str() == "a.txt"))
        );
        assert!(
            listing
                .iter()
                .any(|value| matches!(value, FidanValue::String(name) if name.as_str() == "c.txt"))
        );

        assert!(matches!(
            dispatch_result("deleteFile", vec![string_arg(&file_c.to_string_lossy())])
                .expect("dispatch result")
                .expect("expected deleteFile success"),
            FidanValue::Boolean(true)
        ));

        let _ = std::fs::remove_file(file_a);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn io_mutator_failures_return_specific_runtime_errors() {
        let root = std::env::temp_dir().join("fidan-runtime-io-mutator-errors");
        let missing_parent_file = root.join("missing").join("out.txt");
        let missing_dir = root.join("missing-dir");
        let missing_source = root.join("source-missing.txt");
        let missing_dest = root.join("dest-missing.txt");
        let invalid_parent = root.join("parent-file.txt");
        let invalid_child = invalid_parent.join("child");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root dir");
        std::fs::write(&invalid_parent, "x").expect("write invalid parent fixture");

        let err = dispatch_result(
            "writeFile",
            vec![
                string_arg(&missing_parent_file.to_string_lossy()),
                string_arg("data"),
            ],
        )
        .expect("dispatch result")
        .expect_err("expected writeFile error");
        assert_eq!(err.code, diag_code!("R3003"));

        let err = dispatch_result("listDir", vec![string_arg(&missing_dir.to_string_lossy())])
            .expect("dispatch result")
            .expect_err("expected listDir error");
        assert_eq!(err.code, diag_code!("R3006"));

        let err = dispatch_result(
            "copyFile",
            vec![
                string_arg(&missing_source.to_string_lossy()),
                string_arg(&missing_dest.to_string_lossy()),
            ],
        )
        .expect("dispatch result")
        .expect_err("expected copyFile error");
        assert_eq!(err.code, diag_code!("R3007"));

        let err = dispatch_result(
            "renameFile",
            vec![
                string_arg(&missing_source.to_string_lossy()),
                string_arg(&missing_dest.to_string_lossy()),
            ],
        )
        .expect("dispatch result")
        .expect_err("expected renameFile error");
        assert_eq!(err.code, diag_code!("R3008"));

        let err = dispatch_result(
            "deleteFile",
            vec![string_arg(&missing_source.to_string_lossy())],
        )
        .expect("dispatch result")
        .expect_err("expected deleteFile error");
        assert_eq!(err.code, diag_code!("R3009"));

        let err = dispatch_result(
            "makeDir",
            vec![string_arg(&invalid_child.to_string_lossy())],
        )
        .expect("dispatch result")
        .expect_err("expected makeDir error");
        assert_eq!(err.code, diag_code!("R3010"));

        let _ = std::fs::remove_dir_all(root);
    }
}
