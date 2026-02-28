use ariadne::{sources, Color, Label, Report, ReportKind};
use fidan_source::SourceMap;
use crate::{Diagnostic, Severity};

/// Render a single diagnostic to **stderr** using ariadne.
///
/// `source_map` is used to look up the source text and file name from the
/// `FileId` stored in the diagnostic's span.
pub fn render_to_stderr(diag: &Diagnostic, source_map: &SourceMap) {
    let file = source_map.get(diag.span.file);
    let name: String = file.name.to_string();
    let src:  String = file.src.to_string();

    let kind = match diag.severity {
        Severity::Error   => ReportKind::Error,
        Severity::Warning => ReportKind::Warning,
        Severity::Note    => ReportKind::Advice,
    };

    // In ariadne 0.6 the primary span (file, range) is passed directly to build().
    let primary_range = diag.span.start as usize..diag.span.end as usize;
    let mut b = Report::build(kind, (name.clone(), primary_range.clone()))
        .with_message(&diag.message);

    if !diag.code.is_empty() {
        b = b.with_code(&diag.code);
    }

    let primary_color = match diag.severity {
        Severity::Error   => Color::Red,
        Severity::Warning => Color::Yellow,
        Severity::Note    => Color::BrightBlue,
    };

    if diag.labels.is_empty() {
        // Emit an implicit primary label so ariadne has something to underline.
        b = b.with_label(
            Label::new((name.clone(), primary_range))
                .with_color(primary_color),
        );
    } else {
        for lbl in &diag.labels {
            let color = if lbl.primary { primary_color } else { Color::Blue };
            let range = lbl.span.start as usize..lbl.span.end as usize;
            let mut al = Label::new((name.clone(), range)).with_color(color);
            if !lbl.message.is_empty() {
                al = al.with_message(&lbl.message);
            }
            b = b.with_label(al);
        }
    }

    b.finish()
        .eprint(sources([(name, src)]))
        .expect("failed to render diagnostic");
}
