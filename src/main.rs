use std::path::{Path, PathBuf};
use std::process;
use std::env;

mod core;
mod repository;
mod transport;
mod commands;
mod crypto;
mod protocol;

use crate::core::{GitError, Result};
use crate::commands::{
    CommitCommand,
    CloneCommand,
    PushCommand,
    PullCommand,
    AddCommand,
    StatusCommand,
    InitCommand,
};

fn main() {
    // Get the current directory as the default repository path
    let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    
    // Parse arguments and execute the command
    let args: Vec<String> = env::args().collect();
    
    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }
    
    let command = &args[1];
    
    let result = match command.as_str() {
        "init" => handle_init(&args[2..], &current_dir),
        "clone" => handle_clone(&args[2..], &current_dir),
        "add" => handle_add(&args[2..], &current_dir),
        "commit" => handle_commit(&args[2..], &current_dir),
        "push" => handle_push(&args[2..], &current_dir),
        "pull" => handle_pull(&args[2..], &current_dir),
        "status" => handle_status(&args[2..], &current_dir),
        "help" => {
            print_usage();
            Ok(())
        },
        _ => {
            eprintln!("Unknown command: {}", command);
            print_usage();
            process::exit(1);
        }
    };
    
    // Handle command result
    if let Err(err) = result {
        eprintln!("Error: {}", err);
        process::exit(1);
    }
}

/// Print usage information
fn print_usage() {
    println!("ArtiGit - Git over Tor");
    println!("Usage: arti-git <command> [options]");
    println!("");
    println!("Commands:");
    println!("  init [--bare] [--initial-branch=<name>] [--gitignore]");
    println!("    Initialize a new Git repository");
    println!("  clone [-a|--anonymous] [--depth=<n>] <repository> [<directory>]");
    println!("    Clone a repository via HTTP or over Tor (with --anonymous)");
    println!("  add <pathspec>...");
    println!("    Add file contents to the index");
    println!("  commit [-s|--sign] [-a|--all] [-m <message>]");
    println!("    Record changes to the repository, with anonymous signing support");
    println!("  push [-a|--anonymous] [<remote>] [<refspec>]");
    println!("    Update remote refs and associated objects, optionally over Tor");
    println!("  pull [-a|--anonymous] [<remote>] [<refspec>]");
    println!("    Fetch from and integrate with another repository, optionally over Tor");
    println!("  status");
    println!("    Show the working tree status");
    println!("  help");
    println!("    Show help information");
}

/// Parse options for the init command
fn handle_init(args: &[String], current_dir: &Path) -> Result<()> {
    let mut path = current_dir.to_path_buf();
    let mut bare = false;
    let mut initial_branch = None;
    let mut init_gitignore = false;
    
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bare" => bare = true,
            s if s.starts_with("--initial-branch=") => {
                let branch = &s["--initial-branch=".len()..];
                initial_branch = Some(branch.to_string());
            },
            "--gitignore" => init_gitignore = true,
            arg => {
                if !arg.starts_with("--") && path == current_dir {
                    path = PathBuf::from(arg);
                } else {
                    return Err(GitError::InvalidArgument(format!("Invalid argument: {}", arg)));
                }
            }
        }
        i += 1;
    }
    
    // Create and execute the init command
    let cmd = InitCommand::new(&path, bare, initial_branch.as_deref(), init_gitignore);
    cmd.execute()
}

/// Parse options for the clone command
fn handle_clone(args: &[String], current_dir: &Path) -> Result<()> {
    if args.is_empty() {
        return Err(GitError::InvalidArgument("No repository specified".to_string()));
    }
    
    let mut url = None;
    let mut target = None;
    let mut anonymous = false;
    let mut depth = None;
    
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-a" | "--anonymous" => anonymous = true,
            s if s.starts_with("--depth=") => {
                let depth_str = &s["--depth=".len()..];
                depth = Some(depth_str.parse::<usize>()
                    .map_err(|_| GitError::InvalidArgument(format!("Invalid depth: {}", depth_str)))?);
            },
            arg => {
                if url.is_none() {
                    url = Some(arg.to_string());
                } else if target.is_none() {
                    target = Some(PathBuf::from(arg));
                } else {
                    return Err(GitError::InvalidArgument(format!("Unexpected argument: {}", arg)));
                }
            }
        }
        i += 1;
    }
    
    let url = url.ok_or_else(|| GitError::InvalidArgument("No repository specified".to_string()))?;
    
    // Determine target directory
    let target = match target {
        Some(path) => path,
        None => {
            // Use the last part of the URL as the directory name
            let name = url.split('/').last()
                .unwrap_or("repo")
                .trim_end_matches(".git");
            current_dir.join(name)
        }
    };
    
    // Create and execute the clone command
    let cmd = CloneCommand::new(&url, &target, depth, anonymous);
    cmd.execute()
}

/// Parse options for the add command
fn handle_add(args: &[String], current_dir: &Path) -> Result<()> {
    let mut paths = Vec::new();
    let mut all = false;
    
    for arg in args {
        match arg.as_str() {
            "-A" | "--all" => all = true,
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    
    // Create and execute the add command
    let cmd = AddCommand::new(paths, current_dir, all);
    cmd.execute()
}

/// Parse options for the commit command
fn handle_commit(args: &[String], current_dir: &Path) -> Result<()> {
    let mut message = None;
    let mut sign = false;
    let mut onion_address = None;
    let mut i = 0;
    
    while i < args.len() {
        match args[i].as_str() {
            "-m" => {
                i += 1;
                if i < args.len() {
                    message = Some(args[i].clone());
                } else {
                    return Err(GitError::InvalidArgument("Missing commit message".to_string()));
                }
            },
            "-s" | "--sign" => sign = true,
            s if s.starts_with("--onion=") => {
                onion_address = Some(&s["--onion=".len()..]);
            },
            _ => return Err(GitError::InvalidArgument(format!("Invalid argument: {}", args[i]))),
        }
        i += 1;
    }
    
    let message = message.ok_or_else(|| GitError::InvalidArgument("No commit message specified".to_string()))?;
    
    // Create and execute the commit command
    let cmd = CommitCommand::new(&message, sign, onion_address, current_dir);
    cmd.execute()?;
    
    Ok(())
}

/// Parse options for the push command
fn handle_push(args: &[String], current_dir: &Path) -> Result<()> {
    let mut remote = "origin".to_string();
    let mut refspec = None;
    let mut anonymous = false;
    
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-a" | "--anonymous" => anonymous = true,
            arg => {
                if i == 0 && !arg.starts_with("-") {
                    remote = arg.to_string();
                } else if i == 1 && !arg.starts_with("-") {
                    refspec = Some(arg.to_string());
                } else {
                    return Err(GitError::InvalidArgument(format!("Unexpected argument: {}", arg)));
                }
            }
        }
        i += 1;
    }
    
    // Create and execute the push command
    let cmd = PushCommand::new(&remote, refspec.as_deref(), current_dir, anonymous);
    cmd.execute()
}

/// Parse options for the pull command
fn handle_pull(args: &[String], current_dir: &Path) -> Result<()> {
    let mut remote = "origin".to_string();
    let mut refspec = None;
    let mut anonymous = false;
    
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-a" | "--anonymous" => anonymous = true,
            arg => {
                if i == 0 && !arg.starts_with("-") {
                    remote = arg.to_string();
                } else if i == 1 && !arg.starts_with("-") {
                    refspec = Some(arg.to_string());
                } else {
                    return Err(GitError::InvalidArgument(format!("Unexpected argument: {}", arg)));
                }
            }
        }
        i += 1;
    }
    
    // Create and execute the pull command
    let cmd = PullCommand::new(&remote, refspec.as_deref(), current_dir, anonymous);
    cmd.execute()
}

/// Parse options for the status command
fn handle_status(args: &[String], current_dir: &Path) -> Result<()> {
    let mut short = false;
    
    for arg in args {
        match arg.as_str() {
            "-s" | "--short" => short = true,
            _ => return Err(GitError::InvalidArgument(format!("Invalid argument: {}", arg))),
        }
    }
    
    // Create and execute the status command
    let cmd = StatusCommand::new(current_dir, short);
    cmd.execute()
}
