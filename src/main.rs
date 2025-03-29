use clap::{Parser, Subcommand};
use std::path::Path;

mod core;
mod repository;
mod transport;
mod utils;

use repository::Repository;

#[derive(Parser)]
#[command(name = "arti-git")]
#[command(author = "ArtiGit Team")]
#[command(version = "0.1.0")]
#[command(about = "Decentralized, anonymous Git infrastructure using Arti", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new Git repository
    Init {
        /// Path where to create the repository
        #[arg(default_value = ".")]
        path: String,
        
        /// Create a bare repository
        #[arg(long, default_value_t = false)]
        bare: bool,
    },
    
    /// Clone a repository
    Clone {
        /// URL to clone from
        url: String,
        
        /// Directory to clone into
        #[arg(default_value = "")]
        target_dir: String,
    },
    
    /// Add file contents to the index
    Add {
        /// Files to add
        files: Vec<String>,
    },
    
    /// Record changes to the repository
    Commit {
        /// Commit message
        #[arg(short, long)]
        message: String,
    },
    
    /// Show the working tree status
    Status,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Init { path, bare } => {
            println!("Initializing repository at {} (bare: {})", path, bare);
            match Repository::init(Path::new(path), *bare) {
                Ok(_) => println!("Repository created successfully!"),
                Err(e) => eprintln!("Failed to create repository: {}", e),
            }
        }
        Commands::Clone { url, target_dir } => {
            let dir = if target_dir.is_empty() {
                // Extract repository name from URL
                url.split('/').last().unwrap_or("repository")
            } else {
                target_dir
            };
            println!("Cloning {} into {}", url, dir);
            // TODO: Implement repository cloning
        }
        Commands::Add { files } => {
            println!("Adding files: {:?}", files);
            // TODO: Implement add command
        }
        Commands::Commit { message } => {
            println!("Committing changes: {}", message);
            // TODO: Implement commit command
        }
        Commands::Status => {
            println!("Showing repository status");
            // TODO: Implement status command
        }
    }
    
    Ok(())
}
