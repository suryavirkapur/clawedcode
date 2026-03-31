use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Cargo,
    Npm,
    LocalBuild,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePlan {
    pub method: InstallMethod,
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOutcome {
    pub method: InstallMethod,
    pub command: String,
}

pub fn detect_install_method() -> InstallMethod {
    match std::env::var("CLAWEDCODE_INSTALL_METHOD").ok().as_deref() {
        Some("npm") => InstallMethod::Npm,
        Some("cargo") => InstallMethod::Cargo,
        Some("local") => InstallMethod::LocalBuild,
        _ => std::env::current_exe()
            .ok()
            .as_deref()
            .map(detect_install_method_from_path)
            .unwrap_or(InstallMethod::Unknown),
    }
}

pub fn detect_install_method_from_path(path: &Path) -> InstallMethod {
    let path_str = path.to_string_lossy();
    if path_str.contains("/target/debug/")
        || path_str.contains("/target/release/")
        || path_str.contains("\\target\\debug\\")
        || path_str.contains("\\target\\release\\")
    {
        return InstallMethod::LocalBuild;
    }

    let parent = path.parent().and_then(|p| p.to_str()).unwrap_or_default();
    if parent.ends_with("/.cargo/bin")
        || parent.ends_with("\\.cargo\\bin")
        || parent.contains("/.cargo/bin/")
        || parent.contains("\\.cargo\\bin\\")
    {
        return InstallMethod::Cargo;
    }

    if path_str.contains("node_modules")
        || parent.contains("npm")
        || parent.contains("pnpm")
        || parent.contains("yarn")
    {
        return InstallMethod::Npm;
    }

    InstallMethod::Unknown
}

pub fn plan_self_update() -> Result<UpdatePlan> {
    match detect_install_method() {
        InstallMethod::Cargo => Ok(UpdatePlan {
            method: InstallMethod::Cargo,
            program: "cargo".to_string(),
            args: vec![
                "install".to_string(),
                "clawedcode".to_string(),
                "--force".to_string(),
            ],
        }),
        InstallMethod::Npm => Ok(UpdatePlan {
            method: InstallMethod::Npm,
            program: npm_program_name(),
            args: vec![
                "install".to_string(),
                "-g".to_string(),
                "clawedcode@latest".to_string(),
            ],
        }),
        InstallMethod::LocalBuild => bail!(
            "self-update is not supported for a local build; rebuild locally or install via cargo or npm"
        ),
        InstallMethod::Unknown => bail!(
            "could not determine install method; set CLAWEDCODE_INSTALL_METHOD to cargo or npm"
        ),
    }
}

pub fn run_self_update() -> Result<UpdateOutcome> {
    let plan = plan_self_update()?;
    let status = Command::new(&plan.program)
        .args(&plan.args)
        .status()
        .with_context(|| format!("failed to run {}", plan.program))?;

    if !status.success() {
        bail!(
            "update command exited with status {}",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string())
        );
    }

    Ok(UpdateOutcome {
        method: plan.method,
        command: format!("{} {}", plan.program, plan.args.join(" ")),
    })
}

fn npm_program_name() -> String {
    if cfg!(windows) {
        "npm.cmd".to_string()
    } else {
        "npm".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detect_install_method_marks_local_debug_build() {
        let path = PathBuf::from("/tmp/clawedcode/target/debug/clawedcode");
        assert_eq!(detect_install_method_from_path(&path), InstallMethod::LocalBuild);
    }

    #[test]
    fn detect_install_method_marks_cargo_bin() {
        let path = PathBuf::from("/home/sk/.cargo/bin/clawedcode");
        assert_eq!(detect_install_method_from_path(&path), InstallMethod::Cargo);
    }

    #[test]
    fn detect_install_method_marks_npm_wrapper() {
        let path = PathBuf::from("/usr/lib/node_modules/clawedcode/bin/clawedcode");
        assert_eq!(detect_install_method_from_path(&path), InstallMethod::Npm);
    }
}
