use std::borrow::Cow;

use crate::LintMap;

use rnix::TextRange;

mod all;
use all::all_with;

mod single;
use single::single;

type Source<'a> = Cow<'a, str>;

pub struct FixResult<'a> {
    pub src: Source<'a>,
    pub fixed: Vec<Fixed>,
    pub lints: &'a LintMap,
}

#[derive(Debug, Clone)]
pub struct Fixed {
    pub at: TextRange,
    pub code: u32,
}

impl<'a> FixResult<'a> {
    fn empty(src: Source<'a>, lints: &'a LintMap) -> Self {
        Self {
            src,
            fixed: Vec::new(),
            lints,
        }
    }
}

pub mod main {
    use std::borrow::Cow;

    use crate::{
        config::{
            FixOut, Single as SingleConfig, {ConfFile, Fix as FixConfig},
        },
        err::{FixErr, StatixErr},
    };

    use similar::TextDiff;

    fn print_diff(path: &std::path::Path, old: &str, new: &str) {
        let text_diff = TextDiff::from_lines(old, new);
        let old_file = format!("{}", path.display());
        let new_file = format!("{} [fixed]", path.display());
        println!(
            "{}",
            text_diff
                .unified_diff()
                .context_radius(4)
                .header(&old_file, &new_file)
        );
    }

    pub fn all(fix_config: &FixConfig) -> Result<(), StatixErr> {
        let conf_file = ConfFile::discover(&fix_config.conf_path)?;
        let vfs = fix_config.vfs(conf_file.ignore.as_slice())?;

        let lints = conf_file.lints();
        let fix_scripts = lints.values().flatten().any(|l| l.name() == "script_file");
        let mut fixed_scripts = std::collections::HashSet::new();

        for entry in vfs.iter() {
            let fix_result = lib::with_current_file(Some(entry.file_path), || {
                super::all_with(entry.contents, &lints)
            });
            let src = fix_result
                .as_ref()
                .map_or(Cow::Borrowed(entry.contents), |r| r.src.clone());
            match (fix_config.out(), &fix_result) {
                (FixOut::Diff, _) => print_diff(entry.file_path, entry.contents, &src),
                (FixOut::Stream, _) => println!("{src}"),
                (FixOut::Write, Some(fix_result)) => {
                    std::fs::write(entry.file_path, &*fix_result.src)
                        .map_err(FixErr::InvalidPath)?;
                }
                (FixOut::Write, None) => (),
            }

            // Script files the Nix code refers to
            if !fix_scripts || matches!(fix_config.out(), FixOut::Stream) {
                continue;
            }
            for (path, lang, kind) in lib::referenced_files(&src, entry.file_path) {
                if !fixed_scripts.insert(path.clone()) {
                    continue;
                }
                let Ok(old) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Some(new) = lib::fix_text(lang, kind, &old, Some(&path)) else {
                    continue;
                };
                if matches!(fix_config.out(), FixOut::Diff) {
                    print_diff(&path, &old, &new);
                } else {
                    std::fs::write(&path, new).map_err(FixErr::InvalidPath)?;
                }
            }
        }
        Ok(())
    }

    pub fn single(single_config: &SingleConfig) -> Result<(), StatixErr> {
        let vfs = single_config.vfs()?;
        let entry = vfs.iter().next().unwrap();
        let path = entry.file_path.display().to_string();
        let original_src = entry.contents;

        match (
            single_config.out(),
            lib::with_current_file(Some(entry.file_path), || {
                super::single(single_config.position, original_src)
            }),
        ) {
            (FixOut::Diff, single_result) => {
                let fixed_src = single_result
                    .map(|r| r.src)
                    .unwrap_or(Cow::Borrowed(original_src));
                let text_diff = TextDiff::from_lines(original_src, &fixed_src);
                let old_file = &path;
                let new_file = format!("{path} [fixed]");
                println!(
                    "{}",
                    text_diff
                        .unified_diff()
                        .context_radius(4)
                        .header(old_file, &new_file)
                );
            }
            (FixOut::Stream, single_result) => {
                let src = single_result
                    .map(|r| r.src)
                    .unwrap_or(Cow::Borrowed(original_src));
                println!("{src}");
            }
            (FixOut::Write, Ok(single_result)) => {
                let path = entry.file_path;
                std::fs::write(path, &*single_result.src).map_err(FixErr::InvalidPath)?;
            }
            (_, Err(e)) => return Err(e.into()),
        }
        Ok(())
    }
}
