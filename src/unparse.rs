//! Turn a parsed `CommandList` back into runnable shell source.
//!
//! Exists for the two introspection features that must *print* a stored
//! function body: `declare -f` (C96's documented remainder) and `export -f`
//! (C98's — the `BASH_FUNC_name%%=() { … }` environment encoding). The
//! contract is *round-trip fidelity through rush's own parser* — feeding
//! the output back to `parser::parse` must reproduce an equivalent AST —
//! not byte-fidelity with the original source (comments and exact spacing
//! are gone from the AST) nor with bash's own printer.
//!
//! Words are re-quoted by provenance: `Literal` parts get single quotes
//! (what `'…'`/backslash escapes produced), `Quoted` parts double quotes
//! (their `$`-expansions must stay live), `Unquoted` parts pass through
//! verbatim. Here-documents are re-emitted as `<<`-docs after the line
//! that carries them.

use crate::lexer::{Word, WordPart};
use crate::parser::{
    AndOrList, CaseTerm, CommandList, Compound, CondAst, Connector, RawCommand, RawCompound,
    RawPipeline, RawRedirect, RawSimple, RedirMode,
};

/// A function body as `declare -f` shows it — bash's layout:
/// `name () ` / `{` / indented body / `}`.
pub fn function_source(name: &str, body: &CommandList) -> String {
    let mut p = Printer::default();
    p.push_line(&format!("{name} () "));
    p.push_line("{ ");
    p.indent += 1;
    p.command_list(body);
    p.indent -= 1;
    p.push_line("}");
    p.finish()
}

/// The compact single-value form `export -f` puts in the environment:
/// `() {  body-on-one-logical-block }` — read back by `import_functions`.
pub fn function_export_value(body: &CommandList) -> String {
    let mut p = Printer::default();
    p.push_line("() { ");
    p.indent += 1;
    p.command_list(body);
    p.indent -= 1;
    p.push_line("}");
    p.finish()
}

#[derive(Default)]
struct Printer {
    out: String,
    indent: usize,
    /// The current physical line being assembled (statements append here;
    /// `push_line`/`end_line` terminate it).
    line: String,
    /// Here-doc bodies to flush after the current line ends: `(body,
    /// delimiter, quoted)`.
    pending_heredocs: Vec<(String, String, bool)>,
}

impl Printer {
    fn finish(mut self) -> String {
        self.end_line();
        self.out
    }

    fn stmt(&mut self, text: &str) {
        if self.line.is_empty() {
            self.line = "    ".repeat(self.indent);
        }
        self.line.push_str(text);
    }

    fn end_line(&mut self) {
        if !self.line.trim().is_empty() {
            self.out.push_str(self.line.trim_end());
            self.out.push('\n');
        }
        self.line.clear();
        for (body, delim, quoted) in std::mem::take(&mut self.pending_heredocs) {
            // The `<<DELIM` operator itself was already emitted inline.
            let _ = quoted;
            self.out.push_str(&body);
            if !body.ends_with('\n') {
                self.out.push('\n');
            }
            self.out.push_str(&delim);
            self.out.push('\n');
        }
    }

    fn push_line(&mut self, text: &str) {
        self.end_line();
        self.stmt(text);
        self.end_line();
    }

    fn command_list(&mut self, list: &CommandList) {
        for job in &list.jobs {
            self.and_or(&job.list);
            if job.background {
                self.stmt(" &");
            }
            self.end_line();
        }
    }

    /// A command list rendered inline (for `if …; then`-style headers) —
    /// jobs joined with `; `.
    fn inline_list(&mut self, list: &CommandList) {
        for (i, job) in list.jobs.iter().enumerate() {
            if i > 0 {
                self.stmt("; ");
            }
            self.and_or(&job.list);
            if job.background {
                self.stmt(" &");
            }
        }
    }

    fn and_or(&mut self, list: &AndOrList) {
        self.pipeline(&list.first);
        for (conn, pipe) in &list.rest {
            self.stmt(match conn {
                Connector::And => " && ",
                Connector::Or => " || ",
            });
            self.pipeline(pipe);
        }
    }

    fn pipeline(&mut self, pipe: &RawPipeline) {
        if pipe.timed {
            self.stmt(if pipe.time_posix { "time -p " } else { "time " });
        }
        if pipe.negated {
            self.stmt("! ");
        }
        for (i, cmd) in pipe.commands.iter().enumerate() {
            if i > 0 {
                self.stmt(" | ");
            }
            match cmd {
                RawCommand::Simple(simple) => self.simple(simple),
                RawCommand::Compound(rc) => self.compound(rc),
            }
        }
    }

    fn simple(&mut self, simple: &RawSimple) {
        for (i, word) in simple.argv.iter().enumerate() {
            if i > 0 {
                self.stmt(" ");
            }
            let text = unparse_word(word);
            self.stmt(&text);
        }
        self.redirects(&simple.redirects, !simple.argv.is_empty());
    }

    fn redirects(&mut self, redirects: &[RawRedirect], mut space_before: bool) {
        for r in redirects {
            if space_before {
                self.stmt(" ");
            }
            space_before = true;
            match r {
                RawRedirect::File { fd, file, mode } => {
                    let (op, default_fd) = match mode {
                        RedirMode::Read => ("<", 0),
                        RedirMode::Write => (">", 1),
                        RedirMode::Clobber => (">|", 1),
                        RedirMode::Append => (">>", 1),
                    };
                    if *fd != default_fd {
                        self.stmt(&fd.to_string());
                    }
                    self.stmt(op);
                    self.stmt(&unparse_word(file));
                }
                RawRedirect::Both { file, append } => {
                    self.stmt(if *append { "&>>" } else { "&>" });
                    self.stmt(&unparse_word(file));
                }
                RawRedirect::Dup { fd, target } => self.stmt(&format!("{fd}>&{target}")),
                RawRedirect::Move { fd, target } => self.stmt(&format!("{fd}>&{target}-")),
                RawRedirect::DupWord { fd, word } => {
                    self.stmt(&format!("{fd}>&{}", unparse_word(word)))
                }
                RawRedirect::Heredoc { body, expand } => {
                    // A delimiter that can't occur in the body; quoted when
                    // the original suppressed expansion.
                    let delim = "RUSH_EOF";
                    if *expand {
                        self.stmt(&format!("<<{delim}"));
                    } else {
                        self.stmt(&format!("<<'{delim}'"));
                    }
                    self.pending_heredocs.push((body.clone(), delim.to_string(), !expand));
                }
                RawRedirect::VarFd { name, inner } => {
                    self.stmt(&format!("{{{name}}}"));
                    // Re-emit the wrapped operator without a leading space.
                    self.redirects(std::slice::from_ref(inner), false);
                }
                RawRedirect::HereString(word) => {
                    self.stmt("<<< ");
                    self.stmt(&unparse_word(word));
                }
            }
        }
    }

    fn compound(&mut self, rc: &RawCompound) {
        match rc.compound.as_ref() {
            Compound::Group(list) => {
                self.stmt("{ ");
                self.end_line();
                self.indent += 1;
                self.command_list(list);
                self.indent -= 1;
                self.stmt("}");
            }
            Compound::Subshell(list) => {
                self.stmt("( ");
                self.inline_list(list);
                self.stmt(" )");
            }
            Compound::If { branches, else_body } => {
                for (i, (cond, body)) in branches.iter().enumerate() {
                    self.stmt(if i == 0 { "if " } else { "elif " });
                    self.inline_list(cond);
                    self.stmt("; then");
                    self.end_line();
                    self.indent += 1;
                    self.command_list(body);
                    self.indent -= 1;
                }
                if let Some(body) = else_body {
                    self.stmt("else");
                    self.end_line();
                    self.indent += 1;
                    self.command_list(body);
                    self.indent -= 1;
                }
                self.stmt("fi");
            }
            Compound::Loop { until, cond, body } => {
                self.stmt(if *until { "until " } else { "while " });
                self.inline_list(cond);
                self.stmt("; do");
                self.end_line();
                self.indent += 1;
                self.command_list(body);
                self.indent -= 1;
                self.stmt("done");
            }
            Compound::For { var, words, has_in, body } => {
                self.stmt(&format!("for {var}"));
                if *has_in {
                    self.stmt(" in");
                    for w in words {
                        self.stmt(" ");
                        let text = unparse_word(w);
                        self.stmt(&text);
                    }
                }
                self.stmt("; do");
                self.end_line();
                self.indent += 1;
                self.command_list(body);
                self.indent -= 1;
                self.stmt("done");
            }
            Compound::Select { var, words, has_in, body } => {
                self.stmt(&format!("select {var}"));
                if *has_in {
                    self.stmt(" in");
                    for w in words {
                        self.stmt(" ");
                        let text = unparse_word(w);
                        self.stmt(&text);
                    }
                }
                self.stmt("; do");
                self.end_line();
                self.indent += 1;
                self.command_list(body);
                self.indent -= 1;
                self.stmt("done");
            }
            Compound::CFor { init, cond, update, body } => {
                self.stmt(&format!(
                    "for (({}; {}; {})); do",
                    init.as_deref().unwrap_or(""),
                    cond.as_deref().unwrap_or(""),
                    update.as_deref().unwrap_or("")
                ));
                self.end_line();
                self.indent += 1;
                self.command_list(body);
                self.indent -= 1;
                self.stmt("done");
            }
            Compound::Case { word, items } => {
                self.stmt("case ");
                let text = unparse_word(word);
                self.stmt(&text);
                self.stmt(" in");
                self.end_line();
                self.indent += 1;
                for (patterns, body, term) in items {
                    let pats: Vec<String> = patterns.iter().map(unparse_word).collect();
                    self.stmt(&format!("{})", pats.join(" | ")));
                    self.end_line();
                    self.indent += 1;
                    self.command_list(body);
                    self.stmt(match term {
                        CaseTerm::Break => ";;",
                        CaseTerm::FallThrough => ";&",
                        CaseTerm::Continue => ";;&",
                    });
                    self.end_line();
                    self.indent -= 1;
                }
                self.indent -= 1;
                self.stmt("esac");
            }
            Compound::Arith(expr) => self.stmt(&format!("(({expr}))")),
            Compound::Cond(ast) => {
                self.stmt("[[ ");
                self.stmt(&unparse_cond(ast));
                self.stmt(" ]]");
            }
            Compound::FuncDef { name, body } => {
                self.stmt(&format!("{name} () "));
                self.stmt("{ ");
                self.end_line();
                self.indent += 1;
                self.command_list(body);
                self.indent -= 1;
                self.stmt("}");
            }
            Compound::Coproc { name, cmd } => {
                if name == "COPROC" {
                    self.stmt("coproc ");
                } else {
                    self.stmt(&format!("coproc {name} "));
                }
                match cmd.as_ref() {
                    RawCommand::Simple(simple) => self.simple(simple),
                    RawCommand::Compound(rc) => self.compound(rc),
                }
            }
        }
        self.redirects(&rc.redirects, true);
    }
}

fn unparse_cond(ast: &CondAst) -> String {
    fn operand(ast: &CondAst) -> String {
        match ast {
            CondAst::Or(..) | CondAst::And(..) => format!("( {} )", unparse_cond(ast)),
            _ => unparse_cond(ast),
        }
    }
    match ast {
        CondAst::Or(l, r) => format!("{} || {}", operand(l), operand(r)),
        CondAst::And(l, r) => format!("{} && {}", operand(l), operand(r)),
        CondAst::Not(inner) => format!("! {}", operand(inner)),
        CondAst::Unary(op, w) => format!("{op} {}", unparse_word(w)),
        CondAst::Binary(l, op, r) => {
            format!("{} {op} {}", unparse_word(l), unparse_word(r))
        }
        CondAst::Str(w) => unparse_word(w),
    }
}

/// Re-quote one word by part provenance — see the module doc comment.
fn unparse_word(word: &Word) -> String {
    let mut out = String::new();
    for part in word {
        match part {
            WordPart::Unquoted(s) => out.push_str(s),
            WordPart::Quoted(s) => {
                out.push('"');
                for c in s.chars() {
                    if matches!(c, '"' | '\\' | '`') {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out.push('"');
            }
            WordPart::Literal(s) => {
                out.push('\'');
                out.push_str(&s.replace('\'', "'\\''"));
                out.push('\'');
            }
            WordPart::ArrayLiteral(elements) => {
                out.push('(');
                for (i, el) in elements.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    out.push_str(&unparse_word(el));
                }
                out.push(')');
            }
        }
    }
    out
}
