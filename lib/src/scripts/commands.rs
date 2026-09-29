//! The commands a shell script runs, found by parsing it (brush-parser) and
//! classifying command words the way resholve does: aliases, keywords,
//! builtins, functions defined anywhere in the script, then external
//! commands. Commands run by other commands (`sudo`, `env`, `xargs`,
//! `find -exec`, `bash -c`, `trap`, `eval`...) are found too.

use brush_parser::{
    ParserOptions,
    ast::{
        self, Command, CommandPrefixOrSuffixItem, CompoundCommand, CompoundList, ExtendedTestExpr,
        IoFileRedirectTarget, IoRedirect,
    },
    word::{self, WordPiece},
};
use std::collections::HashSet;

/// How deep `bash -c '...'`, `eval`, `trap` and `$(...)` nesting is followed.
const MAX_DEPTH: usize = 8;

mod builtins;
mod execers;

use builtins::{BASH_BUILTINS, DASH_BUILTINS};
use execers::{runs_command, skip_options, split_flags};

/// Shells whose `-c STRING` is a script.
const SHELLS: &[&str] = &["bash", "sh", "dash", "zsh", "ksh", "mksh"];

/// How a command is looked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    /// alias, function, builtin, external
    Any,
    /// `command x`, `type x`: builtin, external
    NoFunction,
    /// `exec x`, commands run by external commands: external only
    External,
}

/// A command word the script runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Use {
    /// The command word with quotes removed. May contain the `${...}`
    /// placeholder or be a path.
    pub name: String,
    /// Byte range of the word in the script text.
    pub start: usize,
    pub end: usize,
    pub lookup: Lookup,
    /// Its arguments' static text (`None` when dynamic).
    pub args: Vec<Option<String>>,
    /// Commands running it (`sudo env x`: `["sudo", "env"]`).
    pub via: Vec<String>,
}

/// Commands found in a script.
#[derive(Debug, Default, Clone)]
pub struct Commands {
    /// External commands (and paths) the script runs, in order.
    pub uses: Vec<Use>,
    /// Commands the script checks for (`command -v`, `type`, `which`,
    /// `hash`): optional dependencies.
    pub probed: HashSet<String>,
    /// Byte ranges of commands (simple and compound), outermost first, for
    /// attaching directives to the next command.
    pub spans: Vec<(usize, usize)>,
    /// The script (or a nested one) didn't parse; the result may be partial.
    pub incomplete: bool,
    /// The script changes directory (`cd`, `pushd`): relative paths are
    /// relative to somewhere else.
    pub changes_dir: bool,
    /// The script sources other files (`source`, `.`), which may define
    /// functions and aliases.
    pub sources: bool,
    /// Byte ranges of data (here-documents, multi-line words), which can't
    /// hold comments.
    pub data: Vec<(usize, usize)>,
    /// Runs of simple commands executed one after another (`a; b`, `a && b`,
    /// lines), without redirections, for suggesting shorter idioms.
    pub sequences: Vec<Vec<Simple>>,
    /// Pipelines of simple commands (`a | b`).
    pub pipelines: Vec<Vec<Simple>>,
    /// Every top-level simple command, redirections included.
    pub simples: Vec<Simple>,
    /// `for x in $(ls ...)`: byte range of the `$(ls ...)` word.
    pub ls_loops: Vec<(usize, usize)>,
    /// Functions and aliases the script defines.
    pub defined: HashSet<String>,
    /// Files written with text known before the script runs:
    /// `cat > x.json <<'EOF'`, `echo '{}' > x.json`.
    pub writes: Vec<Write>,
}

/// A simple command: its words' static text (`None` when dynamic) and byte
/// ranges in the script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Simple {
    pub words: Vec<(Option<String>, usize, usize)>,
    /// File redirections (`> f`, `>> f`, `< f`).
    pub redirects: Vec<Redirect>,
    /// Anything else than words and file redirections (`VAR=x`,
    /// here-documents, process substitution): not a plain command.
    pub other: bool,
    /// Byte range of the whole command, redirections included.
    pub span: (usize, usize),
    /// Runs only if the command before succeeded (`a && b`).
    pub after_and: bool,
}

/// A file redirection of a simple command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub fd: Option<u32>,
    pub kind: RedirectKind,
    /// The target's static text and byte range.
    pub target: (Option<String>, usize, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectKind {
    /// `>`, `>|`
    Write,
    /// `>>`
    Append,
    /// `<`
    Read,
    Other,
}

impl Simple {
    #[must_use]
    pub fn start(&self) -> usize {
        self.words.first().map_or(0, |w| w.1)
    }
    #[must_use]
    pub fn end(&self) -> usize {
        self.words.last().map_or(0, |w| w.2)
    }
    #[must_use]
    pub fn word(&self, i: usize) -> Option<&str> {
        self.words.get(i)?.0.as_deref()
    }
}

/// A file a command writes (`>`, not `>>`) with static text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    /// The file as written.
    pub file: String,
    /// The text written (Nix `${...}` placeholders included).
    pub text: String,
    /// Byte range of the command word.
    pub start: usize,
    pub end: usize,
    /// For an unquoted here-document (`<<EOF`): where its text starts in
    /// the script. Nix `${...}` in it may render to shell expansions.
    pub expanding_at: Option<usize>,
}

/// Commands `text` (a `shell` script) runs.
#[must_use]
pub fn commands(text: &str, shell: &str) -> Commands {
    let mut walker = Walker {
        builtins: if matches!(shell, "sh" | "dash") {
            DASH_BUILTINS
        } else {
            BASH_BUILTINS
        },
        options: ParserOptions {
            sh_mode: matches!(shell, "sh" | "dash"),
            ..ParserOptions::default()
        },
        top: text.to_string(),
        raw: Vec::new(),
        via: Vec::new(),
        last_simple: None,
        after_and: false,
        in_string: 0,
        functions: HashSet::new(),
        aliases: HashSet::new(),
        out: Commands::default(),
    };
    walker.script(text, 0, 0, None);

    let Walker {
        raw,
        functions,
        aliases,
        builtins,
        mut out,
        ..
    } = walker;
    out.defined = functions.iter().chain(&aliases).cloned().collect();
    // Classify now that all functions and aliases are known (a function
    // may be defined after it is used).
    let is_function = |name: &str| functions.contains(name) || aliases.contains(name);
    out.uses = raw
        .into_iter()
        // `sudo() { ...; }; sudo x`: x is an argument, not a command
        .filter(|u| !u.via.iter().any(|runner| is_function(runner)))
        .filter(|u| {
            let name = u.name.strip_prefix('\\').unwrap_or(&u.name);
            let builtin = builtins.contains(&name);
            match u.lookup {
                Lookup::Any => {
                    !builtin
                        && !functions.contains(name)
                        && (!aliases.contains(name) || u.name.starts_with('\\'))
                }
                Lookup::NoFunction => !builtin,
                Lookup::External => true,
            }
        })
        .map(|mut u| {
            if let Some(name) = u.name.strip_prefix('\\') {
                u.name = name.to_string();
            }
            u
        })
        .collect();
    out.uses.sort_by_key(|u| u.start);
    out.spans
        .sort_by_key(|&(start, end)| (start, std::cmp::Reverse(end)));
    out
}

/// A word of a command line: its static text (`None` when it depends on
/// expansions) and byte range in the script.
#[derive(Debug, Clone)]
struct Arg {
    text: Option<String>,
    start: usize,
    end: usize,
}

struct Walker {
    builtins: &'static [&'static str],
    options: ParserOptions,
    /// The whole script text.
    top: String,
    /// Uses, with the commands that run them (`sudo x`: `["sudo"]`).
    raw: Vec<Use>,
    /// Commands running the command being walked.
    via: Vec<String>,
    /// The last simple command walked, for [`Commands::sequences`].
    last_simple: Option<Simple>,
    /// The next command runs after `&&`.
    after_and: bool,
    /// Inside a string run as a script (`sh -c '...'`, `eval`): another
    /// dialect maybe; commands aren't recorded for idioms.
    in_string: usize,
    functions: HashSet<String>,
    aliases: HashSet<String>,
    out: Commands,
}

/// Byte offsets of each char index of `text` (brush-parser positions count
/// chars), plus one past the end.
fn char_bytes(text: &str) -> Vec<usize> {
    let mut v: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    v.push(text.len());
    v
}

struct Source<'a> {
    bytes: &'a [usize],
    base: usize,
    depth: usize,
    /// Positions in a nested script whose text differs from its source
    /// (`eval $'...'`): every use is reported at this range.
    clamp: Option<(usize, usize)>,
}

impl Source<'_> {
    fn byte(&self, char_index: usize) -> usize {
        if let Some((start, _)) = self.clamp {
            return start;
        }
        self.base
            + self
                .bytes
                .get(char_index)
                .copied()
                .unwrap_or(*self.bytes.last().unwrap_or(&0))
    }
}

/// End a sequence of commands.
fn flush(run: &mut Vec<Simple>, out: &mut Commands) {
    if run.len() > 1 {
        out.sequences.push(std::mem::take(run));
    }
    run.clear();
}

impl Walker {
    /// Walk `text` (a script), which starts at byte `base` of the outer script.
    fn script(&mut self, text: &str, base: usize, depth: usize, clamp: Option<(usize, usize)>) {
        if depth > MAX_DEPTH {
            return;
        }
        let mut parser = brush_parser::Parser::new(text.as_bytes(), &self.options);
        let Ok(program) = parser.parse_program() else {
            self.out.incomplete = true;
            return;
        };
        let bytes = char_bytes(text);
        let src = Source {
            bytes: &bytes,
            base,
            depth,
            clamp,
        };
        let mut run = Vec::new();
        for list in &program.complete_commands {
            self.list_run(list, &src, &mut run);
        }
        flush(&mut run, &mut self.out);
    }

    fn list(&mut self, list: &CompoundList, src: &Source) {
        let mut run: Vec<Simple> = Vec::new();
        self.list_run(list, src, &mut run);
        flush(&mut run, &mut self.out);
    }

    /// Walk `list`, continuing the sequence `run` (the top level is one
    /// list per line).
    fn list_run(&mut self, list: &CompoundList, src: &Source, run: &mut Vec<Simple>) {
        for item in &list.0 {
            let and_or = &item.0;
            // `a && b` runs like `a; b` as long as they succeed
            let pipelines = std::iter::once((&and_or.first, true)).chain(
                and_or.additional.iter().map(|next| match next {
                    ast::AndOr::And(p) => (p, true),
                    ast::AndOr::Or(p) => (p, false),
                }),
            );
            for (k, (pipeline, sequential)) in pipelines.enumerate() {
                self.last_simple = None;
                self.after_and = k > 0 && sequential;
                self.pipeline(pipeline, src);
                let single = pipeline.seq.len() == 1 && !pipeline.bang;
                match self.last_simple.take() {
                    Some(simple)
                        if single && sequential && simple.redirects.is_empty() && !simple.other =>
                    {
                        run.push(simple);
                    }
                    _ => flush(run, &mut self.out),
                }
            }
            if matches!(item.1, ast::SeparatorOperator::Async) {
                flush(run, &mut self.out);
            }
        }
    }

    fn pipeline(&mut self, pipeline: &ast::Pipeline, src: &Source) {
        let mut stages = Vec::new();
        for command in &pipeline.seq {
            self.last_simple = None;
            self.command(command, src);
            stages.push(self.last_simple.take());
        }
        if stages.len() == 1 {
            self.last_simple = stages.pop().flatten();
        } else if !pipeline.bang
            && let Some(stages) = stages.into_iter().collect::<Option<Vec<_>>>()
        {
            self.out.pipelines.push(stages);
        }
    }

    fn span(&mut self, node: &impl ast::SourceLocation, src: &Source) {
        if let Some(loc) = node.location() {
            self.out
                .spans
                .push((src.byte(loc.start.index), src.byte(loc.end.index)));
        }
    }

    fn command(&mut self, command: &Command, src: &Source) {
        self.span(command, src);
        if !matches!(command, Command::Simple(_)) {
            // compound commands aren't part of a sequence
            self.command_inner(command, src);
            self.last_simple = None;
            return;
        }
        self.command_inner(command, src);
    }

    fn command_inner(&mut self, command: &Command, src: &Source) {
        match command {
            Command::Simple(simple) => self.simple(simple, src),
            Command::Compound(compound, redirects) => {
                self.compound(compound, src);
                self.redirects(redirects.as_ref(), src);
            }
            Command::Function(function) => {
                self.functions.insert(function.fname.value.clone());
                self.compound(&function.body.0, src);
                self.redirects(function.body.1.as_ref(), src);
            }
            Command::ExtendedTest(test, redirects) => {
                self.test_expr(&test.expr, src);
                self.redirects(redirects.as_ref(), src);
            }
        }
    }

    fn compound(&mut self, compound: &CompoundCommand, src: &Source) {
        match compound {
            CompoundCommand::Arithmetic(_) => {}
            CompoundCommand::ArithmeticForClause(clause) => self.list(&clause.body.list, src),
            CompoundCommand::BraceGroup(group) => self.list(&group.list, src),
            CompoundCommand::Subshell(subshell) => self.list(&subshell.list, src),
            CompoundCommand::ForClause(for_clause) => {
                if let Some([value]) = for_clause.values.as_deref()
                    && let Some(loc) = &value.loc
                    && src.clamp.is_none()
                    && (value.value.starts_with("$(ls ") || value.value.starts_with("`ls "))
                {
                    self.out
                        .ls_loops
                        .push((src.byte(loc.start.index), src.byte(loc.end.index)));
                }
                for value in for_clause.values.iter().flatten() {
                    self.word(value, src);
                }
                self.list(&for_clause.body.list, src);
            }
            CompoundCommand::CaseClause(case) => {
                self.word(&case.value, src);
                for item in &case.cases {
                    if let Some(cmd) = &item.cmd {
                        self.list(cmd, src);
                    }
                }
            }
            CompoundCommand::IfClause(if_clause) => {
                self.list(&if_clause.condition, src);
                self.list(&if_clause.then, src);
                for else_clause in if_clause.elses.iter().flatten() {
                    if let Some(condition) = &else_clause.condition {
                        self.list(condition, src);
                    }
                    self.list(&else_clause.body, src);
                }
            }
            CompoundCommand::WhileClause(clause) | CompoundCommand::UntilClause(clause) => {
                self.list(&clause.0, src);
                self.list(&clause.1.list, src);
            }
            CompoundCommand::Coprocess(coproc) => self.command(&coproc.body, src),
        }
    }

    fn test_expr(&mut self, expr: &ExtendedTestExpr, src: &Source) {
        match expr {
            ExtendedTestExpr::And(a, b) | ExtendedTestExpr::Or(a, b) => {
                self.test_expr(a, src);
                self.test_expr(b, src);
            }
            ExtendedTestExpr::Not(a) | ExtendedTestExpr::Parenthesized(a) => self.test_expr(a, src),
            ExtendedTestExpr::UnaryTest(_, w) => {
                // `[[ -f ./run.sh ]]` guards the path
                if let Some(path) = self.word(w, src).text
                    && path.contains('/')
                {
                    self.out.probed.insert(path);
                }
            }
            ExtendedTestExpr::BinaryTest(_, a, b) => {
                self.word(a, src);
                self.word(b, src);
            }
        }
    }

    fn redirects(&mut self, redirects: Option<&ast::RedirectList>, src: &Source) {
        for redirect in redirects.iter().flat_map(|r| &r.0) {
            self.redirect(redirect, src);
        }
    }

    fn redirect(&mut self, redirect: &IoRedirect, src: &Source) {
        match redirect {
            IoRedirect::File(_, _, target) => match target {
                IoFileRedirectTarget::Filename(w) | IoFileRedirectTarget::Duplicate(w) => {
                    self.word(w, src);
                }
                IoFileRedirectTarget::ProcessSubstitution(_, subshell) => {
                    self.list(&subshell.list, src);
                }
                IoFileRedirectTarget::Fd(_) => {}
            },
            // Only command substitutions of unquoted here-docs run.
            IoRedirect::HereDocument(_, doc) => {
                if let Some(loc) = &doc.doc.loc {
                    self.out
                        .data
                        .push((src.byte(loc.start.index), src.byte(loc.end.index)));
                }
                if doc.requires_expansion {
                    self.word(&doc.doc, src);
                }
            }
            IoRedirect::HereString(_, w) | IoRedirect::OutputAndError(w, _) => {
                self.word(w, src);
            }
        }
    }

    /// Commands in `$(...)`, backticks and `<(...)` inside a word; returns the
    /// word's static text when it has no expansions.
    fn word(&mut self, w: &ast::Word, src: &Source) -> Arg {
        let (start, end) = w.loc.as_ref().map_or((src.base, src.base), |loc| {
            (src.byte(loc.start.index), src.byte(loc.end.index))
        });
        if w.value.contains('\n') {
            self.out.data.push((start, end));
        }
        let text = match word::parse(&w.value, &self.options) {
            Ok(pieces) => self.pieces(&pieces, start, src, &w.value),
            Err(_) => None,
        };
        Arg { text, start, end }
    }

    /// Static text of `pieces` (at byte `start`), walking substitutions.
    fn pieces(
        &mut self,
        pieces: &[word::WordPieceWithSource],
        start: usize,
        src: &Source,
        raw: &str,
    ) -> Option<String> {
        let mut text = Some(String::new());
        let add = |text: &mut Option<String>, s: &str| {
            if let Some(t) = text.as_mut() {
                t.push_str(s);
            }
        };
        for piece in pieces {
            let at = start + piece.start_index;
            match &piece.piece {
                WordPiece::Text(s) | WordPiece::SingleQuotedText(s) => {
                    add(&mut text, s);
                }
                // escapes aren't decoded: `$'j\x71'` is unknown
                WordPiece::AnsiCQuotedText(s) if s.contains('\\') => text = None,
                WordPiece::AnsiCQuotedText(s) => add(&mut text, s),
                WordPiece::EscapeSequence(s) => add(&mut text, s.strip_prefix('\\').unwrap_or(s)),
                WordPiece::DoubleQuotedSequence(inner)
                | WordPiece::GettextDoubleQuotedSequence(inner) => {
                    // inner pieces are relative to the word too
                    match self.pieces(inner, start, src, raw) {
                        Some(s) => add(&mut text, &s),
                        None => text = None,
                    }
                }
                WordPiece::CommandSubstitution(script) => {
                    self.script(script, at + 2, src.depth + 1, src.clamp);
                    text = None;
                }
                WordPiece::BackquotedCommandSubstitution(script) => {
                    self.script(script, at + 1, src.depth + 1, src.clamp);
                    text = None;
                }
                WordPiece::ParameterExpansion(_) => {
                    // `${x:-$(jq .)}`: commands in the operand
                    if let Some(inner) = raw
                        .get(piece.start_index..piece.end_index)
                        .and_then(|r| r.strip_prefix("${"))
                        .and_then(|r| r.strip_suffix('}'))
                        && let Some(op) = inner.trim_start_matches(['!', '#']).find(|c: char| {
                            !(c.is_ascii_alphanumeric() || c == '_' || c == '@' || c == '*')
                        })
                    {
                        let skip = inner.len() - inner.trim_start_matches(['!', '#']).len() + op;
                        let operand_at = skip + inner[skip..].len()
                            - inner[skip..]
                                .trim_start_matches([
                                    ':', '-', '=', '+', '?', '#', '%', '/', '^', ',',
                                ])
                                .len();
                        let operand = &inner[operand_at..];
                        if (operand.contains("$(") || operand.contains('`'))
                            && let Ok(inner_pieces) = word::parse(operand, &self.options)
                        {
                            self.pieces(&inner_pieces, at + 2 + operand_at, src, operand);
                        }
                    }
                    text = None;
                }
                WordPiece::TildeExpansion(_) | WordPiece::ArithmeticExpression(_) => text = None,
            }
        }
        text
    }

    /// `cat > x.json <<'EOF'` / `echo '{}' > x.json`: text known before the
    /// script runs, written to a file.
    fn static_write(&mut self, simple: &ast::SimpleCommand, args: &[Arg], src: &Source) {
        let items = || {
            simple
                .prefix
                .iter()
                .flat_map(|p| &p.0)
                .chain(simple.suffix.iter().flat_map(|s| &s.0))
        };
        let mut file = None;
        let mut heredoc = None;
        let mut expanding_at = None;
        for item in items() {
            if let CommandPrefixOrSuffixItem::IoRedirect(redirect) = item {
                match redirect {
                    IoRedirect::File(
                        fd,
                        ast::IoFileRedirectKind::Write | ast::IoFileRedirectKind::Clobber,
                        IoFileRedirectTarget::Filename(w),
                    ) if fd.is_none_or(|fd| fd == 1) => file = Some(w.value.clone()),
                    IoRedirect::HereDocument(fd, doc) if fd.is_none_or(|fd| fd == 0) => {
                        let expands = doc.requires_expansion
                            && (doc.doc.value.contains('$') || doc.doc.value.contains('`'));
                        heredoc = Some((!expands).then(|| doc.doc.value.clone()));
                        if doc.requires_expansion {
                            expanding_at =
                                doc.doc.loc.as_ref().map(|loc| src.byte(loc.start.index));
                        }
                    }
                    _ => {}
                }
            }
        }
        // `$out/config.json` is fine: only the text must be known
        let Some(file) = file.map(|f| f.replace(['"', '\''], "")) else {
            return;
        };
        let Some(first) = args.first() else { return };
        let text = match (first.text.as_deref(), heredoc) {
            (Some("cat"), Some(Some(body))) if args.len() == 1 => body,
            (Some("echo" | "printf"), None) if args.len() > 1 => {
                let words: Option<Vec<&str>> =
                    args[1..].iter().map(|a| a.text.as_deref()).collect();
                let Some(words) = words else { return };
                // echo -n / printf 'fmt': still literal
                words.join(" ")
            }
            _ => return,
        };
        self.out.writes.push(Write {
            file,
            text,
            start: first.start,
            end: first.end,
            expanding_at,
        });
    }

    fn simple(&mut self, simple: &ast::SimpleCommand, src: &Source) {
        for item in simple.prefix.iter().flat_map(|p| &p.0) {
            self.prefix_or_suffix(item, src);
        }
        let mut args = Vec::new();
        if let Some(name) = &simple.word_or_name {
            let mut arg = self.word(name, src);
            // `\cmd` skips aliases; keep the marker the quoting removed
            if name.value.starts_with('\\') {
                arg.text = arg.text.map(|t| format!("\\{t}"));
            }
            args.push(arg);
        }
        for item in simple.suffix.iter().flat_map(|s| &s.0) {
            if let Some(arg) = self.prefix_or_suffix(item, src) {
                args.push(arg);
            }
        }
        if src.clamp.is_none() {
            self.static_write(simple, &args, src);
        }
        self.invocation(&args, Lookup::Any, src);
        // `$(...)` nested commands have exact positions; `eval`/`bash -c`
        // strings don't
        if src.clamp.is_some() || self.in_string > 0 || args.is_empty() {
            return;
        }
        let mut redirects = Vec::new();
        let mut other = false;
        let items = simple
            .prefix
            .iter()
            .flat_map(|p| &p.0)
            .chain(simple.suffix.iter().flat_map(|s| &s.0));
        for item in items {
            match item {
                CommandPrefixOrSuffixItem::Word(_) => {}
                CommandPrefixOrSuffixItem::IoRedirect(IoRedirect::File(
                    fd,
                    kind,
                    IoFileRedirectTarget::Filename(w),
                )) => {
                    let kind = match kind {
                        ast::IoFileRedirectKind::Write | ast::IoFileRedirectKind::Clobber => {
                            RedirectKind::Write
                        }
                        ast::IoFileRedirectKind::Append => RedirectKind::Append,
                        ast::IoFileRedirectKind::Read => RedirectKind::Read,
                        _ => RedirectKind::Other,
                    };
                    let target = w.loc.as_ref().map_or((None, 0, 0), |loc| {
                        (
                            Some(w.value.clone()),
                            src.byte(loc.start.index),
                            src.byte(loc.end.index),
                        )
                    });
                    redirects.push(Redirect {
                        fd: fd.and_then(|f| u32::try_from(f).ok()),
                        kind,
                        target,
                    });
                }
                _ => other = true,
            }
        }
        // `> out cmd`: the span would miss the operator
        if redirects
            .iter()
            .any(|r| args.first().is_some_and(|a| r.target.1 < a.start))
        {
            other = true;
        }
        let start = redirects
            .iter()
            .map(|r| r.target.1)
            .chain(args.first().map(|a| a.start))
            .min()
            .unwrap_or(0);
        let end = redirects
            .iter()
            .map(|r| r.target.2)
            .chain(args.last().map(|a| a.end))
            .max()
            .unwrap_or(start);
        let simple = Simple {
            words: args
                .iter()
                .map(|a| (a.text.clone(), a.start, a.end))
                .collect(),
            redirects,
            other,
            span: (start, end),
            after_and: std::mem::take(&mut self.after_and),
        };
        self.out.simples.push(simple.clone());
        self.last_simple = Some(simple);
    }

    fn prefix_or_suffix(&mut self, item: &CommandPrefixOrSuffixItem, src: &Source) -> Option<Arg> {
        match item {
            CommandPrefixOrSuffixItem::Word(w) => Some(self.word(w, src)),
            CommandPrefixOrSuffixItem::IoRedirect(r) => {
                self.redirect(r, src);
                None
            }
            CommandPrefixOrSuffixItem::ProcessSubstitution(_, subshell) => {
                self.list(&subshell.list, src);
                None
            }
            CommandPrefixOrSuffixItem::AssignmentWord(_, w) => {
                // `env A=1 cmd`, `declare x=1`: arguments too
                Some(self.word(w, src))
            }
        }
    }

    /// `text` (a static string argument) as a nested script.
    fn nested(&mut self, arg: &Arg, src: &Source) {
        if let Some(text) = &arg.text {
            // Exact positions when the text is in the script as is (`'...'`,
            // `"..."` without escapes), else the whole argument.
            let offset = arg.start + usize::from(arg.end > arg.start + text.len());
            let exact = src.clamp.is_none()
                && self.top.get(offset..offset + text.len()) == Some(text.as_str());
            let clamp = if exact {
                None
            } else {
                Some(src.clamp.unwrap_or((arg.start, arg.end)))
            };
            self.in_string += 1;
            self.script(text, offset, src.depth + 1, clamp);
            self.in_string -= 1;
        }
    }

    fn record(&mut self, arg: &Arg, args: &[Arg], lookup: Lookup, src: &Source) {
        if let Some(name) = &arg.text
            && !name.is_empty()
        {
            let (start, end) = src.clamp.unwrap_or((arg.start, arg.end));
            self.raw.push(Use {
                name: name.clone(),
                start,
                end,
                lookup,
                args: args.iter().map(|a| a.text.clone()).collect(),
                via: self.via.clone(),
            });
        }
    }

    fn probe(&mut self, args: &[Arg]) {
        for arg in args {
            if let Some(name) = &arg.text
                && !name.starts_with('-')
            {
                self.out.probed.insert(name.clone());
            }
        }
    }

    /// Handle a command line: the command itself and commands it runs.
    /// `[ -f path ]`: the paths are probed.
    fn file_tests(&mut self, args: &[Arg]) {
        for pair in args.windows(2) {
            if let (Some("-e" | "-f" | "-x" | "-s" | "-r"), Some(path)) =
                (pair[0].text.as_deref(), pair[1].text.as_deref())
            {
                self.out.probed.insert(path.to_string());
            }
        }
    }

    fn invocation(&mut self, args: &[Arg], lookup: Lookup, src: &Source) {
        let Some(first) = args.first() else { return };
        let Some(name) = first.text.as_deref() else {
            return; // dynamic command word ($CMD, "$@", $(...))
        };
        let rest = &args[1..];
        let bare = name.strip_prefix('\\').unwrap_or(name);
        let program = bare.rsplit('/').next().unwrap_or(bare);

        // builtins / keywords that take commands
        if lookup != Lookup::External && !self.functions.contains(bare) {
            match bare {
                "cd" | "pushd" | "popd" => self.out.changes_dir = true,
                "source" | "." => self.out.sources = true,
                "command" => {
                    let (flags, cmd) = split_flags(rest);
                    if flags.contains('v') || flags.contains('V') {
                        self.probe(cmd);
                    } else {
                        self.invocation(cmd, Lookup::NoFunction, src);
                    }
                    return;
                }
                "builtin" => {
                    // `builtin X` only runs builtin X, which may run commands
                    // itself (`builtin command foo`)
                    let is_builtin = rest
                        .first()
                        .and_then(|a| a.text.as_deref())
                        .is_some_and(|b| self.builtins.contains(&b));
                    if is_builtin {
                        self.invocation(rest, Lookup::NoFunction, src);
                    }
                    return;
                }
                "exec" => {
                    let cmd = skip_options(rest, &['a'], &[]);
                    if let Some(cmd) = cmd {
                        self.invocation(cmd, Lookup::External, src);
                    }
                    return;
                }
                "type" | "hash" => {
                    self.probe(rest);
                    return;
                }
                // `[ -x ./run.sh ] && ./run.sh`: file tests guard paths
                "[" | "test" => {
                    self.file_tests(rest);
                    return;
                }
                "eval" => {
                    let text: Option<Vec<&str>> = rest.iter().map(|a| a.text.as_deref()).collect();
                    if let (Some(text), Some(arg)) = (text, rest.first()) {
                        let joined = Arg {
                            text: Some(text.join(" ")),
                            start: arg.start,
                            end: rest.last().map_or(arg.end, |a| a.end),
                        };
                        self.nested(&joined, src);
                    }
                    return;
                }
                "trap" => {
                    // trap 'handler' SIGNALS...
                    if let Some(handler) = rest.first()
                        && rest.len() > 1
                        && handler
                            .text
                            .as_deref()
                            .is_some_and(|t| t != "-" && !t.starts_with('-'))
                    {
                        self.nested(handler, src);
                    }
                    return;
                }
                "alias" => {
                    for arg in rest {
                        if let Some((alias, _)) =
                            arg.text.as_deref().and_then(|t| t.split_once('='))
                        {
                            self.aliases.insert(alias.to_string());
                        }
                    }
                    return;
                }
                _ => {}
            }
        }

        self.record(first, rest, lookup, src);
        // `command sudo x`, `exec sudo x`: never a function, so no name that
        // could be one
        self.via.push(if lookup == Lookup::Any {
            bare.to_string()
        } else {
            String::new()
        });
        self.runs(program, rest, src);
        self.via.pop();
    }

    /// Commands `program` runs with arguments `rest`.
    fn runs(&mut self, program: &str, rest: &[Arg], src: &Source) {
        if program == "which" {
            self.probe(rest);
            return;
        }
        if SHELLS.contains(&program) {
            // sh -c 'script' [name args...]
            if let Some(i) = rest.iter().position(|a| a.text.as_deref() == Some("-c"))
                && let Some(script) = rest.get(i + 1)
            {
                self.nested(script, src);
            }
            return;
        }
        if program == "find" {
            self.find(rest, src);
            return;
        }
        if let Some(cmd) = runs_command(program, rest, src.depth, self, src) {
            self.invocation(cmd, Lookup::External, src);
        }
    }

    /// `find ... -exec cmd args ;` / `+`
    fn find(&mut self, args: &[Arg], src: &Source) {
        let mut i = 0;
        while i < args.len() {
            let action = args[i].text.as_deref();
            if matches!(action, Some("-exec" | "-execdir" | "-ok" | "-okdir")) {
                let plus_ends = matches!(action, Some("-exec" | "-execdir"));
                let start = i + 1;
                let mut end = start;
                while end < args.len() {
                    match args[end].text.as_deref() {
                        Some(";") => break,
                        Some("+") if plus_ends => break,
                        _ => end += 1,
                    }
                }
                self.invocation(&args[start..end], Lookup::External, src);
                i = end;
            }
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests;
