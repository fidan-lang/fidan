//! Frontend diagnostics happen before execution/backend selection.
pub const ERRORS: &[(&str, &str)] = &[
    ("E0000", "var ="),
    ("E0001", "\"unterminated"),
    ("E0100", "object Child extends Missing {}"),
    ("E0101", "print(unknown_name)"),
    (
        "E0102",
        "action duplicate { var x = 1; var x = 2; print(x) }",
    ),
    ("E0103", "const var x = 1; x = 2"),
    ("E0104", "const var x oftype integer"),
    ("E0105", "var x oftype Missing = nothing"),
    ("E0106", "use definitely_missing_module"),
    ("E0107", "object Recursive extends Recursive {}"),
    ("E0108", "use std.math.{missing_export}"),
    ("E0109", "action duplicate {}\naction duplicate {}"),
    ("E0201", "var x oftype integer = \"text\""),
    (
        "E0202",
        "action incorrect returns integer { return \"text\" }",
    ),
    ("E0203", "print(true + 1)"),
    ("E0204", "var x = \"text\"; x.missing_method()"),
    (
        "E0205",
        "action unsafe with (n oftype integer) { for i in 0..n { print(i) } }",
    ),
    (
        "E0301",
        "action required with (certain x oftype integer) {}\nrequired()",
    ),
    (
        "E0302",
        "action required with (certain x oftype integer) {}\nrequired(\"text\")",
    ),
    (
        "E0303",
        "action badDecorator with (certain target oftype integer) returns action { return action {} }\n@badDecorator\naction decorated {}",
    ),
    ("E0304", "@extern(123)\naction decorated {}"),
    ("E0305", "action noArgs {}\nnoArgs(1)"),
    ("E0306", "print(this)"),
    ("E0307", "print(parent)"),
    ("E0308", "var value = 1; value()"),
    (
        "E0401",
        "var counter = 0\nparallel { task a { counter += 1 } task b { counter += 1 } }",
    ),
];

/// Active warning/note emitters; inactive reserved codes are documented separately.
pub const WARNINGS: &[(&str, &str, &str)] = &[
    ("W1004", "warning", "action work {}\nspawn work()"),
    ("W1005", "note", "use std.math"),
    (
        "W1006",
        "warning",
        "action work { return; print(1) }\nwork()",
    ),
    (
        "W1007",
        "warning",
        "use std.math\nuse std.math\nprint(math.abs(-1))",
    ),
    ("W2001", "warning", "print(1)"),
    ("W2002", "warning", "42"),
    ("W2003", "warning", "action work {}\nwork"),
    ("W2004", "warning", "@unsafe\naction work {}\nwork()"),
    (
        "W2005",
        "warning",
        "@deprecated(\"use replacement\")\naction work {}\nwork()",
    ),
    (
        "W2006",
        "warning",
        "action work with (certain value oftype integer) {}\naction runCheck { var value oftype flexible = nothing; attempt { work(value) } catch failure {} }\nrunCheck()",
    ),
    (
        "W5001",
        "warning",
        "action dynamicValue returns flexible { return 1 }\naction runCheck { for i in 0..2 { var value = dynamicValue(); print(value) } }\nrunCheck()",
    ),
    (
        "W5003",
        "warning",
        "action dynamicValue returns flexible { return 1 }\naction runCheck { for i in 0..2 { var value = dynamicValue(); print(value) } }\nrunCheck()",
    ),
];
