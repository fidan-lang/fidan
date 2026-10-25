//! Shared observable failures for CLI, REPL and both native backends.
use std::path::Path;

pub struct RuntimeCase {
    pub name: &'static str,
    pub source: String,
    pub code: &'static str,
    pub catchable: bool,
}

pub fn cases(directory: &Path) -> Vec<RuntimeCase> {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join("invalid-utf8"), [0xff, 0xfe]).unwrap();
    std::fs::write(directory.join("file"), "fixture").unwrap();
    let path = directory.to_string_lossy().replace('\\', "/");
    let mut cases = vec![
        ("parallel-invalid-list", "use std.parallel as workers\naction identity with (certain value oftype integer) returns integer { return value }\nvar invalid oftype flexible = \"not a list\"\nworkers.parallelMap(invalid, identity)", "R0001", true),
        ("parallel-invalid-callback", "use std.parallel as workers\nvar invalid oftype flexible = 42\nworkers.parallelMap([1], invalid)", "R0001", true),
        ("assert", "assert(false, \"explicit failure\")", "R0001", false),
        ("assert-eq", "assert_eq(1, 2, \"explicit failure\")", "R0001", false),
        ("assert-ne", "assert_ne(1, 1, \"explicit failure\")", "R0001", false),
        ("test-assert", "use std.test as testing\ntesting.assertEq(1, 2, \"test failure\")", "R0001", false),
        ("slice-step", "var step oftype flexible = 0\nprint(\"abc\"[::step])", "R0001", false),
        ("certain", "action required with (certain value oftype integer) { print(value) }\nvar missing oftype flexible = nothing\nrequired(missing)", "R0001", true),
        ("gather", "use std.async\naction fail { panic(\"gather failure\") }\nvar pending = spawn fail()\nawait async.gather([pending])", "R1002", true),
        ("wait-any", "use std.async\naction fail { panic(\"wait failure\") }\nvar pending = spawn fail()\nawait async.waitAny([pending])", "R1002", true),
        ("timeout", "use std.async\naction fail { panic(\"timeout failure\") }\nvar pending = spawn fail()\nawait async.timeout(pending, 1000)", "R1002", true),
        ("parallel-callback", "use std.parallel as workers\naction fail with (certain value oftype integer) returns integer { return 1 / value }\nworkers.parallelMap([0, 1], fail)", "R2001", true),
        ("depth", "action descend with (certain n oftype integer) returns integer { if n == 0 { return 0 } return descend(n - 1) }\nprint(descend(4096))", "R1001", true),
        ("task-group", "parallel { task one { panic(\"task one\") } task two { panic(\"task two\") } }", "R9001", false),
        ("concurrent-group", "concurrent { task one { panic(\"task one\") } task two { panic(\"task two\") } }", "R9001", false),
        ("direct", "print(\"before\")\npanic(\"user failure\")", "R1002", true),
        ("callback", "var state = Shared(0)\nstate.update(action with (value) { panic(\"callback failure\") })", "R1002", true),
        ("await", "action fail { panic(\"await failure\") }\nvar pending = spawn fail()\nawait pending", "R1002", true),
        ("parallel-for", "parallel for i in 0..2 { panic(\"iteration failure\") }", "R1002", true),
        ("arithmetic", "var zero oftype flexible = 0\nprint(1 / zero)", "R2001", true),
        ("bounds", "var items = [1]\nvar index oftype flexible = 2\nprint(items[index])", "R2002", true),
        ("overflow", "var large oftype flexible = 9223372036854775807\nprint(large + 1)", "R2003", true),
        ("type", "var bad oftype flexible = \"oops\"\nprint(bad - 1)", "R0001", true),
        ("spoof", "panic(\"error [R2001]: not arithmetic\")", "R1002", true),
        ("open", "use std.io\nio.readFile(\"$DIR/missing\")", "R3001", true),
        ("read", "use std.io\nio.readFile(\"$DIR/invalid-utf8\")", "R3002", true),
        ("write", "use std.io\nio.writeFile(\"$DIR/missing/child\", \"x\")", "R3003", true),
        ("json", "use std.json\njson.loads(\"invalid json\")", "R3005", true),
        ("list", "use std.io\nio.listDir(\"$DIR/missing\")", "R3006", true),
        ("copy", "use std.io\nio.copyFile(\"$DIR/missing\", \"$DIR/destination\")", "R3007", true),
        ("rename", "use std.io\nio.renameFile(\"$DIR/missing\", \"$DIR/destination\")", "R3008", true),
        ("delete", "use std.io\nio.deleteFile(\"$DIR/missing\")", "R3009", true),
        ("mkdir", "use std.io\nio.makeDir(\"$DIR/file/child\")", "R3010", true),
    ].into_iter().map(|(name, source, code, catchable)| RuntimeCase { name, source: source.replace("$DIR", &path), code, catchable }).collect::<Vec<_>>();
    #[cfg(windows)]
    {
        let file = directory.join("readonly");
        std::fs::write(&file, "readonly fixture").unwrap();
        let mut permissions = std::fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&file, permissions).unwrap();
        cases.push(RuntimeCase {
            name: "permission",
            source: format!("use std.io\nio.writeFile(\"{path}/readonly\", \"x\")"),
            code: "R3004",
            catchable: true,
        });
    }
    let mut wide = String::from("action wide with (certain n oftype integer) returns integer { ");
    for index in 0..512 {
        wide.push_str(&format!("var local{index} = n + {index}; "));
    }
    wide.push_str("if n == 0 { return 0 }; var total = wide(n - 1); ");
    for index in 0..512 {
        wide.push_str(&format!("total += local{index}; "));
    }
    wide.push_str("return total }\nprint(wide(4096))");
    cases.insert(
        0,
        RuntimeCase {
            name: "wide-depth",
            source: wide,
            code: "R1001",
            catchable: true,
        },
    );
    cases
}

pub fn cleanup(directory: &Path) {
    #[cfg(windows)]
    {
        let file = directory.join("readonly");
        // Restore the attributes of the writable file created alongside it.
        let permissions = std::fs::metadata(directory.join("file"))
            .unwrap()
            .permissions();
        std::fs::set_permissions(&file, permissions).unwrap();
    }
    std::fs::remove_dir_all(directory).unwrap();
}

pub fn assert_code(stderr: &str, code: &str) {
    let headers = stderr
        .lines()
        .filter(|line| {
            let normalized = line.replace(['[', ']', ':'], " ");
            let mut words = normalized.split_whitespace();
            words.next() == Some("error") && words.next() == Some(code)
        })
        .count();
    assert_eq!(headers, 1, "expected one {code} diagnostic: {stderr}");
}

impl RuntimeCase {
    pub fn caught_stdout(&self) -> &'static str {
        if self.name == "direct" {
            "before\ncaught diagnostic"
        } else {
            "caught diagnostic"
        }
    }

    pub fn caught_source(&self) -> String {
        let (imports, body): (Vec<_>, Vec<_>) = self.source.lines().partition(|line| {
            line.trim_start().starts_with("use ") || line.trim_start().starts_with("action ")
        });
        format!(
            "{}\nattempt {{ {} }} catch failure {{ print(\"caught diagnostic\") }}",
            imports.join("\n"),
            body.join("\n")
        )
    }
}
