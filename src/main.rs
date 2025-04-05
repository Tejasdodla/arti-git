use std::path::{Path, PathBuf};
use std::process;
use std::env;

mod core;
mod repository;
mod transport;
mod commands;
mod crypto;
mod protocol;
mod utils;
mod service;
mod ipfs;

use clap::{Parser, Subcommand, Args};
use tokio::signal;
use crate::core::{ArtiGitClient, ArtiGitConfig, OnionServiceConfig, GitError, Result};
use crate::service::GitOnionService;

#[derive(Parser)]
#[command(name = "arti-git")]
#[command(about = "Git over Tor using Arti", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    
    /// Path to config file
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Clone a repository
    Clone(CloneArgs),
    /// Pull updates from a remote
    Pull(PullArgs),
    /// Push changes to a remote
    Push(PushArgs),
    /// Initialize a repository
    Init(InitArgs),
    /// Show status of the repository
    Status(StatusArgs),
    /// Add files to the index
    Add(AddArgs),
    /// Commit changes to the repository
    Commit(CommitArgs),
    /// Start an onion service for hosting repositories
    Serve(ServeArgs),
    /// IPFS related commands
    Ipfs(IpfsArgs),
}

#[derive(Args)]
struct CloneArgs {
    /// Repository URL to clone
    url: String,
    /// Destination path
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Use Tor for anonymous cloning
    #[arg(short, long)]
    anonymous: bool,
}

#[derive(Args)]
struct PullArgs {
    /// Repository path
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Remote name
    #[arg(short, long, default_value = "origin")]
    remote: String,
    /// Use Tor for anonymous pulling
    #[arg(short, long)]
    anonymous: bool,
}

#[derive(Args)]
struct PushArgs {
    /// Repository path
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Remote name
    #[arg(short, long, default_value = "origin")]
    remote: String,
    /// Use Tor for anonymous pushing
    #[arg(short, long)]
    anonymous: bool,
}

#[derive(Args)]
struct InitArgs {
    /// Repository path
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Create a bare repository
    #[arg(long)]
    bare: bool,
    /// Initial branch name
    #[arg(long)]
    initial_branch: Option<String>,
}

#[derive(Args)]
struct StatusArgs {
    /// Repository path
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Show short status
    #[arg(short, long)]
    short: bool,
}

#[derive(Args)]
struct AddArgs {
    /// Repository path
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Files to add
    files: Vec<PathBuf>,
    /// Add all changes
    #[arg(short = 'A', long)]
    all: bool,
}

#[derive(Args)]
struct CommitArgs {
    /// Repository path
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Commit message
    #[arg(short, long)]
    message: String,
    /// Sign commit with Ed25519 key
    #[arg(short, long)]
    sign: bool,
}

#[derive(Args)]
struct ServeArgs {
    /// Repository directory to serve
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Port for the onion service
    #[arg(short, long, default_value = "9418")]
    port: u16,
}

#[derive(Args)]
struct IpfsArgs {
    /// IPFS subcommand
    #[command(subcommand)]
    command: IpfsCommands,
}

#[derive(Subcommand)]
enum IpfsCommands {
    /// Store a file in IPFS
    Add {
        /// Path to the file to store
        path: PathBuf,
    },
    /// Retrieve a file from IPFS
    Get {
        /// IPFS content ID
        cid: String,
        /// Output file path
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Show information about the IPFS node
    Info,
    /// Store a Git object in IPFS
    StoreObject {
        /// Repository path
        #[arg(default_value = ".")]
        repo_path: PathBuf,
        /// Object ID
        object_id: String,
    },
    /// Get a Git object from IPFS
    GetObject {
        /// Repository path
        #[arg(default_value = ".")]
        repo_path: PathBuf,
        /// Object ID
        object_id: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse the command line arguments
    let cli = Cli::parse();

    // --- Initialize Transports ---
    // Register custom transports (like Tor) with gitoxide
    if let Err(e) = transport::register_transports().await {
        eprintln!("Failed to register custom transports: {}", e);
        // Decide if this is a fatal error
        // process::exit(1);
    }
    // --- End Initialize Transports ---
    
    // Load config
    let config_path = cli.config
        .unwrap_or_else(|| ArtiGitConfig::default_location());
    
    let config = if config_path.exists() {
        ArtiGitConfig::from_file(&config_path)?
    } else {
        ArtiGitConfig::default()
    };
    
    // Initialize ArtiGit client
    let client = match ArtiGitClient::new(config).await {
        Ok(client) => client,
        Err(e) => {
            eprintln!("Failed to initialize ArtiGit client: {}", e);
            process::exit(1);
        }
    };
    
    // Execute command
    match cli.command {
        Commands::Clone(args) => {
            println!("Cloning {} to {}", args.url, args.path.display());
            
            // If anonymous flag is set, ensure Tor is enabled in the config
            if args.anonymous {
                // Check if Tor is enabled
                if !client.config().tor.use_tor {
                    eprintln!("Anonymous clone requested but Tor is not enabled in the configuration");
                    process::exit(1);
                }
            }
            
            match client.clone(&args.url, &args.path).await {
                Ok(_) => println!("Clone completed successfully"),
                Err(e) => {
                    eprintln!("Clone failed: {}", e);
                    process::exit(1);
                }
            }
        },
        Commands::Pull(args) => {
            println!("Pulling from remote {} in {}", args.remote, args.path.display());
            
            // Open the repository
            let mut repo = match client.open(&args.path) {
                Ok(repo) => repo,
                Err(e) => {
                    eprintln!("Failed to open repository: {}", e);
                    process::exit(1);
                }
            };
            
            match client.pull(&mut repo).await {
                Ok(_) => println!("Pull completed successfully"),
                Err(e) => {
                    eprintln!("Pull failed: {}", e);
                    process::exit(1);
                }
            }
        },
        Commands::Push(args) => {
            println!("Pushing to remote {} from {}", args.remote, args.path.display());
            
            // Open the repository
            let repo = match client.open(&args.path) {
                Ok(repo) => repo,
                Err(e) => {
                    eprintln!("Failed to open repository: {}", e);
                    process::exit(1);
                }
            };
            
            match client.push(&repo, Some(&args.remote), None).await {
                Ok(_) => println!("Push completed successfully"),
                Err(e) => {
                    eprintln!("Push failed: {}", e);
                    process::exit(1);
                }
            }
        },
        Commands::Init(args) => {
            println!("Initializing repository at {}", args.path.display());
            
            // This uses gitoxide's repository creation directly
            let config = gix::init::CreateOptions {
                initial_head: args.initial_branch,
                bare: args.bare,
                ..Default::default()
            };
            
            match gix::init::Options {
                target_directory: args.path,
                create_options: config,
                ..Default::default()
            }.execute() {
                Ok(_) => println!("Repository initialized successfully"),
                Err(e) => {
                    eprintln!("Initialization failed: {}", e);
                    process::exit(1);
                }
            }
        },
        Commands::Status(args) => {
            println!("Checking status of {}", args.path.display());
            
            // Open the repository
            let repo = match client.open(&args.path) {
                Ok(repo) => repo,
                Err(e) => {
                    eprintln!("Failed to open repository: {}", e);
                    process::exit(1);
                }
            };
            
            // Get repository status using gitoxide
            let statuses = match repo.status() {
                Ok(statuses) => statuses,
                Err(e) => {
                    eprintln!("Failed to get repository status: {}", e);
                    process::exit(1);
                }
            };
            
            // Display status based on format preference
            if args.short {
                for status in statuses {
                    println!("{:?}", status);
                }
            } else {
                println!("Repository status:");
                for status in statuses {
                    println!("{:?}", status);
                }
            }
        },
        Commands::Add(args) => {
            println!("Adding files in {}", args.path.display());
            
            // Open the repository
            let repo = match client.open(&args.path) {
                Ok(repo) => repo,
                Err(e) => {
                    eprintln!("Failed to open repository: {}", e);
                    process::exit(1);
                }
            };
            
            if args.all {
                // Add all changes
                let workdir = match repo.work_dir() {
                    Ok(dir) => dir,
                    Err(e) => {
                        eprintln!("Failed to get work directory: {}", e);
                        process::exit(1);
                    }
                };
                
                match client.add(&repo, &[PathBuf::from("*")]).await {
                    Ok(_) => println!("Added all changes to index"),
                    Err(e) => {
                        eprintln!("Failed to add changes: {}", e);
                        process::exit(1);
                    }
                }
            } else if !args.files.is_empty() {
                // Add specific files
                match client.add(&repo, &args.files).await {
                    Ok(_) => println!("Added files to index"),
                    Err(e) => {
                        eprintln!("Failed to add files: {}", e);
                        process::exit(1);
                    }
                }
            } else {
                eprintln!("No files specified");
                process::exit(1);
            }
        },
        Commands::Commit(args) => {
            println!("Committing changes in {}", args.path.display());
            
            // Open the repository
            let repo = match client.open(&args.path) {
                Ok(repo) => repo,
                Err(e) => {
                    eprintln!("Failed to open repository: {}", e);
                    process::exit(1);
                }
            };
            
            // Commit changes
            match client.commit(&repo, &args.message, args.sign).await {
                Ok(commit_id) => println!("Created commit: {}", commit_id),
                Err(e) => {
                    eprintln!("Failed to commit: {}", e);
                    process::exit(1);
                }
            }
        },
        Commands::Serve(args) => {
            println!("Starting Git onion service for {}", args.path.display());
            
            // Ensure Tor is enabled
            if !client.config().tor.use_tor {
                eprintln!("Cannot create onion service: Tor is not enabled in configuration");
                process::exit(1);
            }
            
            // Get tor client from our ArtiGit client
            let tor_client = match client.tor_client() {
                Some(client) => client,
                None => {
                    eprintln!("Tor client not available");
                    process::exit(1);
                }
            };
            
            // Create service configuration
            let mut onion_config = match client.config().tor.onion_service.clone() {
                Some(config) => config,
                None => {
                    // Create a default configuration
                    let mut cfg = OnionServiceConfig::default();
                    cfg.port = args.port;
                    cfg
                }
            };
            
            // Override port if specified in the command line
            if args.port != 9418 {
                onion_config.port = args.port;
            }
            
            // Create and start the onion service
            let runtime = tokio::runtime::Handle::current();
            let mut service = GitOnionService::new(
                tor_client.clone(),
                &args.path,
                onion_config,
                runtime.clone(),
            )?;
            
            // Start the service and get the onion address
            let onion_address = match service.start().await {
                Ok(addr) => addr,
                Err(e) => {
                    eprintln!("Failed to start onion service: {}", e);
                    process::exit(1);
                }
            };