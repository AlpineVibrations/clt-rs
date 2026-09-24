use std::{
    fs,
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

pub(crate) const CLT_TASK_MANAGEMENT_SKILL_NAME: &str = "clt-task-management";
pub(crate) const GIT_COMMIT_SKILL_NAME: &str = "git-commit";
pub(crate) const EMBEDDED_CLT_TASK_MANAGEMENT_SKILL: &str =
    include_str!("../skills/clt-task-management/SKILL.md");
pub(crate) const EMBEDDED_GIT_COMMIT_SKILL: &str = include_str!("../skills/git-commit/SKILL.md");

const BUNDLED_SKILLS: [(&str, &str); 2] = [
    (
        CLT_TASK_MANAGEMENT_SKILL_NAME,
        EMBEDDED_CLT_TASK_MANAGEMENT_SKILL,
    ),
    (GIT_COMMIT_SKILL_NAME, EMBEDDED_GIT_COMMIT_SKILL),
];

pub(crate) fn install_skills(force: bool) -> Result<()> {
    let home = user_home()?;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = io::stdout().lock();
    install_skills_at(
        &home,
        force,
        stdin.is_terminal() && io::stdout().is_terminal(),
        &mut input,
        &mut output,
    )
}

#[cfg(not(windows))]
fn user_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .context("Cannot find the user home directory (HOME is unset)")
}

#[cfg(windows)]
fn user_home() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("USERPROFILE").filter(|home| !home.is_empty()) {
        return Ok(PathBuf::from(home));
    }
    match (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH")) {
        (Some(drive), Some(path)) if !drive.is_empty() && !path.is_empty() => {
            let mut home = drive;
            home.push(path);
            Ok(PathBuf::from(home))
        }
        _ => bail!("Cannot find the user home directory (USERPROFILE is unset)"),
    }
}

fn install_skills_at(
    home: &Path,
    force: bool,
    interactive: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<()> {
    let skills_root = home.join(".agents").join("skills");

    // Check every destination before writing, so a non-interactive conflict does
    // not leave only the first skill installed.
    for (name, contents) in BUNDLED_SKILLS {
        let directory = skills_root.join(name);
        if let Some(metadata) = destination_metadata(&directory)?
            && (!metadata.is_dir() || metadata.file_type().is_symlink())
        {
            bail!(
                "Skill destination is not a directory: {}",
                directory.display()
            );
        }
        let file = directory.join("SKILL.md");
        if let Some(metadata) = destination_metadata(&file)? {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                bail!(
                    "Skill destination is not a regular file: {}",
                    file.display()
                );
            }
            if !force && !interactive && fs::read(&file)? != contents.as_bytes() {
                bail!(
                    "{} differs from the bundled skill; rerun in a terminal to choose whether to overwrite it, or use --force",
                    file.display()
                );
            }
        }
    }

    for (name, contents) in BUNDLED_SKILLS {
        let directory = skills_root.join(name);
        let file = directory.join("SKILL.md");
        let existing = if file.exists() {
            Some(fs::read(&file).with_context(|| format!("Failed to read {}", file.display()))?)
        } else {
            None
        };
        if existing.as_deref() == Some(contents.as_bytes()) {
            writeln!(output, "Already up to date: {}", file.display())?;
            continue;
        }
        if existing.is_some() && !force {
            write!(output, "Overwrite {}? [y/N] ", file.display())?;
            output.flush()?;
            let mut answer = String::new();
            input.read_line(&mut answer)?;
            if !answer.trim().eq_ignore_ascii_case("y") {
                writeln!(output, "Skipped: {}", file.display())?;
                continue;
            }
        }
        fs::create_dir_all(&directory)
            .with_context(|| format!("Failed to create {}", directory.display()))?;
        fs::write(&file, contents)
            .with_context(|| format!("Failed to write {}", file.display()))?;
        writeln!(
            output,
            "{}: {}",
            if existing.is_some() {
                "Updated"
            } else {
                "Installed"
            },
            file.display()
        )?;
    }
    Ok(())
}

fn destination_metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Failed to inspect {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io};

    use super::{BUNDLED_SKILLS, EMBEDDED_CLT_TASK_MANAGEMENT_SKILL, install_skills_at};

    #[test]
    fn installs_bundled_skills_and_skips_identical_files() {
        let home = tempfile::tempdir().unwrap();
        let mut output = Vec::new();
        install_skills_at(home.path(), false, false, &mut io::empty(), &mut output).unwrap();
        for (name, contents) in BUNDLED_SKILLS {
            assert_eq!(
                fs::read_to_string(
                    home.path()
                        .join(".agents/skills")
                        .join(name)
                        .join("SKILL.md")
                )
                .unwrap(),
                contents
            );
        }
        output.clear();
        install_skills_at(home.path(), false, false, &mut io::empty(), &mut output).unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("Already up to date")
        );
    }

    #[test]
    fn changed_skill_requires_confirmation_or_force() {
        let home = tempfile::tempdir().unwrap();
        let file = home
            .path()
            .join(".agents/skills/clt-task-management/SKILL.md");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "user edition").unwrap();
        let mut output = Vec::new();

        let error = install_skills_at(home.path(), false, false, &mut io::empty(), &mut output)
            .unwrap_err();
        assert!(error.to_string().contains("--force"));
        assert!(!home.path().join(".agents/skills/git-commit").exists());
        assert_eq!(fs::read_to_string(&file).unwrap(), "user edition");

        install_skills_at(
            home.path(),
            false,
            true,
            &mut io::Cursor::new(b"n\n"),
            &mut output,
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "user edition");
        assert!(
            home.path()
                .join(".agents/skills/git-commit/SKILL.md")
                .exists()
        );

        install_skills_at(home.path(), true, false, &mut io::empty(), &mut output).unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            EMBEDDED_CLT_TASK_MANAGEMENT_SKILL
        );
    }

    #[test]
    fn confirmation_overwrites_only_the_accepted_skill() {
        let home = tempfile::tempdir().unwrap();
        for (name, _) in BUNDLED_SKILLS {
            let file = home
                .path()
                .join(".agents/skills")
                .join(name)
                .join("SKILL.md");
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, "old").unwrap();
        }
        let mut output = Vec::new();
        install_skills_at(
            home.path(),
            false,
            true,
            &mut io::Cursor::new(b"y\nn\n"),
            &mut output,
        )
        .unwrap();
        let root = home.path().join(".agents/skills");
        assert_eq!(
            fs::read_to_string(root.join("clt-task-management/SKILL.md")).unwrap(),
            EMBEDDED_CLT_TASK_MANAGEMENT_SKILL
        );
        assert_eq!(
            fs::read_to_string(root.join("git-commit/SKILL.md")).unwrap(),
            "old"
        );
    }
}
