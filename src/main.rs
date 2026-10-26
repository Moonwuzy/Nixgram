use anyhow::{Context, Result, bail};
use clap::Parser;
use dialoguer::{Confirm, FuzzySelect, Input, Select};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use walkdir::WalkDir;

const SYSTEM_PACKAGES: &str = "environment.systemPackages = with pkgs; [";
const HOME_PACKAGES: &str = "home-manager.users.moonwuzy.home.packages = with pkgs; [";
const MAX_SEARCH_RESULTS: usize = 10;

#[derive(Debug, Parser)]
#[command(
    name = "nxg",
    version,
    about = "An interactive package manager for NixOS flakes"
)]
struct Cli {
    #[arg(short = 'i', value_name = "PACKAGE")]
    install: Option<String>,

    #[arg(short = 's', value_name = "QUERY")]
    search: Option<String>,

    #[arg(short = 'u', value_name = "PACKAGE")]
    uninstall: Option<String>,

    #[arg(long = "flake-update")]
    flake_update: bool,

    #[arg(short = 'c')]
    clean: bool,

    #[arg(long = "list")]
    list: bool,

    #[arg(long = "settings")]
    settings: bool,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct Settings {
    flake: Option<String>,
    module: Option<String>,
    host: Option<String>,
}

#[derive(Clone, Copy)]
enum PackageTarget {
    System,
    HomeManager,
}

impl PackageTarget {
    fn marker(self) -> &'static str {
        match self {
            Self::System => SYSTEM_PACKAGES,
            Self::HomeManager => HOME_PACKAGES,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::System => "System packages",
            Self::HomeManager => "Home Manager packages",
        }
    }
}

fn settings_path() -> Result<PathBuf> {
    let base = dirs::config_dir().context("Could not find your user configuration directory")?;
    Ok(base.join("nixgram").join("config.toml"))
}

fn load_settings() -> Settings {
    let Ok(path) = settings_path() else {
        return Settings::default();
    };

    fs::read_to_string(path)
        .ok()
        .and_then(|contents| toml::from_str(&contents).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &Settings) -> Result<()> {
    let path = settings_path()?;
    fs::create_dir_all(path.parent().context("Invalid settings path")?)?;

    let contents = toml::to_string_pretty(settings)?;
    fs::write(&path, contents).with_context(|| format!("Could not write {}", path.display()))?;

    println!("Settings saved to {}", path.display());
    Ok(())
}

fn discover_flakes() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(current) = std::env::current_dir() {
        candidates.push(current.join("flake.nix"));
    }

    candidates.push(PathBuf::from("/etc/nixos/flake.nix"));

    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("nixos-new/nixos/flake.nix"));
        candidates.push(home.join("Code/Nixgram/flake.nix"));
    }

    candidates.sort();
    candidates.dedup();
    candidates.retain(|path| path.is_file());
    candidates
}

fn discover_modules(flake: &Path) -> Vec<PathBuf> {
    let Some(root) = flake.parent() else {
        return Vec::new();
    };

    let mut modules = Vec::new();

    for entry in WalkDir::new(root)
        .follow_links(false)
        .max_depth(6)
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.file_type().is_file()
            && entry
                .path()
                .file_name()
                .is_some_and(|name| name == "packages.nix")
        {
            modules.push(entry.path().to_path_buf());
        }
    }

    modules.sort();
    modules
}

fn select_path(prompt: &str, candidates: &[PathBuf], current: Option<&str>) -> Result<PathBuf> {
    if !candidates.is_empty() {
        let mut options: Vec<String> = candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect();

        options.push("Enter another path...".to_string());

        let default = current
            .and_then(|saved| candidates.iter().position(|path| path == Path::new(saved)))
            .unwrap_or(0);

        let selected = Select::new()
            .with_prompt(prompt)
            .items(&options)
            .default(default)
            .interact()?;

        if selected < candidates.len() {
            return Ok(candidates[selected].clone());
        }
    }

    let entered: String = Input::new()
        .with_prompt(format!("{prompt} (absolute or relative path)"))
        .with_initial_text(current.unwrap_or(""))
        .interact_text()?;

    let path = PathBuf::from(entered);

    if !path.is_file() {
        bail!("File does not exist: {}", path.display());
    }

    Ok(path.canonicalize()?)
}

fn configure() -> Result<()> {
    let mut settings = load_settings();

    println!("Nixgram settings\n");

    let flakes = discover_flakes();
    let flake = select_path("Select your flake.nix", &flakes, settings.flake.as_deref())?;

    if flake.file_name().is_none_or(|name| name != "flake.nix") {
        bail!("Please select a file named flake.nix");
    }

    let modules = discover_modules(&flake);
    let module = select_path(
        "Select your packages.nix module",
        &modules,
        settings.module.as_deref(),
    )?;

    if module.file_name().is_none_or(|name| name != "packages.nix") {
        bail!("Please select a file named packages.nix");
    }

    let host: String = Input::new()
        .with_prompt("Flake host output name")
        .default(
            settings
                .host
                .clone()
                .unwrap_or_else(|| "desktop".to_string()),
        )
        .interact_text()?;

    settings.flake = Some(flake.display().to_string());
    settings.module = Some(module.display().to_string());
    settings.host = Some(host);

    save_settings(&settings)?;

    println!("Flake:  {}", flake.display());
    println!("Module: {}", module.display());
    println!("Host:   {}", settings.host.as_deref().unwrap_or("desktop"));

    Ok(())
}

fn nix_search(query: &str) -> Result<serde_json::Map<String, serde_json::Value>> {
    let output = Command::new("nix")
        .args(["search", "nixpkgs", query, "--json"])
        .output()
        .context("Could not run Nix. Is Nix installed and available in PATH?")?;

    if !output.status.success() {
        bail!(
            "Nix search failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let results: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("Could not parse Nix search results")?;

    Ok(results
        .as_object()
        .context("Unexpected JSON format from Nix search")?
        .clone())
}

fn normalize_attribute(attribute: &str) -> &str {
    attribute
        .strip_prefix("legacyPackages.x86_64-linux.")
        .or_else(|| attribute.strip_prefix("legacyPackages.aarch64-linux."))
        .unwrap_or(attribute)
}

fn package_label(attribute: &str, details: &serde_json::Value) -> String {
    let name = details["pname"].as_str().unwrap_or(attribute);
    let version = details["version"].as_str().unwrap_or("unknown");
    let description = details["description"]
        .as_str()
        .unwrap_or("No description available")
        .replace(['\n', '\r'], " ");

    format!("{name} ({version}) — {attribute}: {description}")
}

fn package_candidates(
    packages: serde_json::Map<String, serde_json::Value>,
    query: &str,
) -> Vec<(String, String)> {
    let mut candidates: Vec<(String, String)> = packages
        .into_iter()
        .map(|(raw_attribute, details)| {
            let attribute = normalize_attribute(&raw_attribute).to_string();
            let label = package_label(&attribute, &details);
            (attribute, label)
        })
        .collect();

    candidates.sort_by(|a, b| {
        let a_exact = a.0.eq_ignore_ascii_case(query);
        let b_exact = b.0.eq_ignore_ascii_case(query);

        b_exact.cmp(&a_exact).then_with(|| a.0.cmp(&b.0))
    });

    candidates.dedup_by(|a, b| a.0 == b.0);
    candidates.truncate(MAX_SEARCH_RESULTS);
    candidates
}

fn search_packages(query: &str) -> Result<()> {
    println!("Searching Nixpkgs for '{query}'...\n");

    let packages = nix_search(query)?;
    let candidates = package_candidates(packages, query);

    if candidates.is_empty() {
        println!("No packages found for '{query}'.");
        return Ok(());
    }

    println!("Showing up to {} matching packages:\n", MAX_SEARCH_RESULTS);

    for (attribute, label) in candidates {
        println!("{label}");
        println!("  Attribute: {attribute}\n");
    }

    Ok(())
}

fn select_target() -> Result<PackageTarget> {
    let choices = ["System packages", "Home Manager packages"];

    let selected = Select::new()
        .with_prompt("Which package list should Nixgram edit?")
        .items(choices)
        .default(0)
        .interact()?;

    Ok(if selected == 0 {
        PackageTarget::System
    } else {
        PackageTarget::HomeManager
    })
}

// Locate a package list while ignoring ordinary quoted strings and Nix comments.
fn find_list_bounds(source: &str, marker: &str) -> Result<(usize, usize)> {
    let mut marker_matches = source.match_indices(marker);

    let (marker_start, _) = marker_matches
        .next()
        .with_context(|| format!("Could not find `{marker}` in the package module"))?;

    if marker_matches.next().is_some() {
        bail!("Multiple occurrences of `{marker}` found; refusing an ambiguous edit");
    }

    let open = marker_start + marker.len() - 1;
    let bytes = source.as_bytes();

    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut i = open;

    while i < bytes.len() {
        let ch = bytes[i];
        let next = bytes.get(i + 1).copied();

        if in_line_comment {
            if ch == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }

        if in_block_comment {
            if ch == b'*' && next == Some(b'/') {
                in_block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        if in_string {
            if escaped {
                escaped = false;
            } else if ch == b'\\' {
                escaped = true;
            } else if ch == b'"' {
                in_string = false;
            }

            i += 1;
            continue;
        }

        if ch == b'#' {
            in_line_comment = true;
            i += 1;
            continue;
        }

        if ch == b'/' && next == Some(b'*') {
            in_block_comment = true;
            i += 2;
            continue;
        }

        if ch == b'"' {
            in_string = true;
            i += 1;
            continue;
        }

        if ch == b'[' {
            depth += 1;
        } else if ch == b']' {
            depth = depth.saturating_sub(1);

            if depth == 0 {
                return Ok((open + 1, i));
            }
        }

        i += 1;
    }

    bail!("Could not find the closing bracket for the package list")
}

fn backup_file(path: &Path) -> Result<()> {
    let backup = path.with_extension("nixgram.bak");

    fs::copy(path, &backup)
        .with_context(|| format!("Could not create or refresh backup {}", backup.display()))?;

    println!("Backup refreshed: {}", backup.display());
    Ok(())
}

fn validate_nix_file(path: &Path) -> Result<()> {
    let output = Command::new("nix-instantiate")
        .arg("--parse")
        .arg(path)
        .output()
        .context("Could not run nix-instantiate to validate Nix syntax")?;

    if !output.status.success() {
        bail!(
            "Nix syntax validation failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

fn edit_file_and_validate(path: &Path, original: &str, updated: &str) -> Result<()> {
    backup_file(path)?;

    fs::write(path, updated).with_context(|| format!("Could not write {}", path.display()))?;

    match validate_nix_file(path) {
        Ok(()) => {
            println!("Nix syntax validation passed.");
            Ok(())
        }
        Err(error) => {
            fs::write(path, original)
                .with_context(|| format!("Could not restore {}", path.display()))?;

            bail!("{error:#}\nThe file was restored to its original contents.");
        }
    }
}

fn install_package(query: &str) -> Result<()> {
    let settings = load_settings();

    let module = settings
        .module
        .as_deref()
        .context("No package module selected. Run `nxg -settings` first.")?;

    let module_path = Path::new(module);

    if !module_path.is_file() {
        bail!("Package module does not exist: {module}");
    }

    println!("Searching Nixpkgs for '{query}'...\n");

    let packages = nix_search(query)?;
    let candidates = package_candidates(packages, query);

    if candidates.is_empty() {
        println!("No packages found for '{query}'.");
        return Ok(());
    }

    let labels: Vec<String> = candidates.iter().map(|(_, label)| label.clone()).collect();

    let selected = FuzzySelect::new()
        .with_prompt("Filter packages and select one")
        .items(&labels)
        .default(0)
        .interact()?;

    let attribute = &candidates[selected].0;
    let target = select_target()?;

    let original = fs::read_to_string(module_path)
        .with_context(|| format!("Could not read {}", module_path.display()))?;

    let (list_start, list_end) = find_list_bounds(&original, target.marker())?;
    let existing_list = &original[list_start..list_end];

    if existing_list.lines().any(|line| {
        let entry = line.split('#').next().unwrap_or("").trim();
        entry == attribute
    }) {
        println!(
            "{attribute} already appears in the {} list.",
            target.label()
        );
        return Ok(());
    }

    println!("\nPackage: {attribute}");
    println!("Target:  {}", target.label());
    println!("File:    {}", module_path.display());

    if !Confirm::new()
        .with_prompt(format!("Add {attribute} to {}?", target.label()))
        .default(false)
        .interact()?
    {
        println!("Cancelled; no changes made.");
        return Ok(());
    }

    let line_start = original[..list_end]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);

    let mut updated = original.clone();
    updated.insert_str(line_start, &format!("    {attribute}\n"));

    edit_file_and_validate(module_path, &original, &updated)?;

    println!("Added {attribute} to {}.", target.label());
    println!("Review the diff before rebuilding your system.");

    Ok(())
}

fn uninstall_package(query: &str) -> Result<()> {
    let settings = load_settings();

    let module = settings
        .module
        .as_deref()
        .context("No package module selected. Run `nxg -settings` first.")?;

    let module_path = Path::new(module);

    if !module_path.is_file() {
        bail!("Package module does not exist: {module}");
    }

    let target = select_target()?;

    let original = fs::read_to_string(module_path)
        .with_context(|| format!("Could not read {}", module_path.display()))?;

    let (list_start, list_end) = find_list_bounds(&original, target.marker())?;
    let list = &original[list_start..list_end];

    let lines: Vec<&str> = list.split_inclusive('\n').collect();

    let matches: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let entry = line.split('#').next().unwrap_or("").trim();

            if entry == query || entry.ends_with(&format!(".{query}")) {
                Some(index)
            } else {
                None
            }
        })
        .collect();

    if matches.is_empty() {
        println!(
            "No direct package entry matching '{query}' was found in {}.",
            target.label()
        );
        return Ok(());
    }

    let selected_line = if matches.len() == 1 {
        matches[0]
    } else {
        let options: Vec<String> = matches
            .iter()
            .map(|index| lines[*index].trim().to_string())
            .collect();

        let selected = FuzzySelect::new()
            .with_prompt("Choose the package declaration to remove")
            .items(&options)
            .default(0)
            .interact()?;

        matches[selected]
    };

    let declaration = lines[selected_line].trim().to_string();

    println!("Proposed removal: {declaration}");

    if !Confirm::new()
        .with_prompt("Remove this declaration?")
        .default(false)
        .interact()?
    {
        println!("Cancelled; no changes made.");
        return Ok(());
    }

    let mut new_list = String::new();

    for (index, line) in lines.iter().enumerate() {
        if index != selected_line {
            new_list.push_str(line);
        }
    }

    let mut updated = original.clone();
    updated.replace_range(list_start..list_end, &new_list);

    edit_file_and_validate(module_path, &original, &updated)?;

    println!("Removed the declaration from {}.", target.label());
    Ok(())
}

// List declarations in the configured System and Home Manager package lists.
fn list_installed_packages() -> Result<()> {
    let settings = load_settings();

    let module = settings
        .module
        .as_deref()
        .context("No package module selected. Run `nxg -settings` first.")?;

    let module_path = Path::new(module);

    if !module_path.is_file() {
        bail!("Package module does not exist: {module}");
    }

    let source = fs::read_to_string(module_path)
        .with_context(|| format!("Could not read {}", module_path.display()))?;

    println!("Nixgram package lists\n");
    println!("Source: {}\n", module_path.display());

    for target in [PackageTarget::System, PackageTarget::HomeManager] {
        println!("{}:", target.label());

        match find_list_bounds(&source, target.marker()) {
            Ok((start, end)) => {
                let packages: Vec<String> = source[start..end]
                    .lines()
                    .filter_map(|line| {
                        let entry = line.split('#').next().unwrap_or("").trim();

                        if entry.is_empty() {
                            None
                        } else {
                            Some(entry.to_string())
                        }
                    })
                    .collect();

                if packages.is_empty() {
                    println!("  No packages listed.");
                } else {
                    for (index, package) in packages.iter().enumerate() {
                        println!("  {}. {}", index + 1, package);
                    }
                    println!("  Total: {}", packages.len());
                }
            }
            Err(_) => {
                println!("  Package list not found in the configured module.");
            }
        }

        println!();
    }

    Ok(())
}

fn update_flake() -> Result<()> {
    let settings = load_settings();

    let flake = settings
        .flake
        .as_deref()
        .context("No flake selected. Run `nxg -settings` first.")?;

    let root = Path::new(flake).parent().context("Invalid flake path")?;

    if !Confirm::new()
        .with_prompt(format!("Run `nix flake update` in {}?", root.display()))
        .default(false)
        .interact()?
    {
        println!("Cancelled.");
        return Ok(());
    }

    let status = Command::new("nix")
        .args(["flake", "update"])
        .current_dir(root)
        .status()
        .context("Could not run Nix. Is it installed?")?;

    if !status.success() {
        bail!("Flake update failed");
    }

    println!("Flake inputs updated.");
    Ok(())
}

fn clean_store() -> Result<()> {
    println!("Nix garbage collection removes unused store paths.");

    if !Confirm::new()
        .with_prompt("Run `nix-collect-garbage`?")
        .default(false)
        .interact()?
    {
        println!("Cancelled.");
        return Ok(());
    }

    let status = Command::new("nix-collect-garbage")
        .status()
        .context("Could not run nix-collect-garbage")?;

    if !status.success() {
        bail!("Nix garbage collection failed");
    }

    println!("Garbage collection finished.");
    Ok(())
}

fn print_help() {
    println!(
        "Nixgram\n\
         -i PACKAGE  Search and interactively install a package\n\
         -s QUERY    Search Nixpkgs (up to 10 results)\n\
         -u PACKAGE  Remove a package declaration\n\
         -fu         Update flake inputs\n\
         -c          Clean unused Nix store paths\n\
         -list       List System and Home Manager packages\n\
         -settings   Select flake and package module"
    );
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args()
        .map(|arg| match arg.as_str() {
            "-settings" => "--settings".to_string(),
            "-fu" => "--flake-update".to_string(),
            "-list" => "--list".to_string(),
            other => other.to_string(),
        })
        .collect();

    let cli = Cli::parse_from(args);

    if cli.settings {
        configure()?;
    } else if let Some(query) = cli.search {
        search_packages(&query)?;
    } else if cli.list {
        list_installed_packages()?;
    } else if let Some(package) = cli.install {
        install_package(&package)?;
    } else if let Some(package) = cli.uninstall {
        uninstall_package(&package)?;
    } else if cli.flake_update {
        update_flake()?;
    } else if cli.clean {
        clean_store()?;
    } else {
        print_help();
    }

    Ok(())
}
