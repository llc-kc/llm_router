//! Mooncake HTTP Client - Command line tool for querying key existence
//!
//! Usage:
//!   mooncake-http-client --url <URL> --key <KEY>    # Check single key
//!   mooncake-http-client --url <URL> --keys k1,k2   # Check multiple keys
//!   mooncake-http-client --url <URL> --all-keys     # List all keys
//!   mooncake-http-client --url <URL> --segments     # List all segments
//!   mooncake-http-client --url <URL> --health       # Health check

use clap::{Parser, Subcommand};

use llm_cahr::modes::mooncake_client::{MooncakeClient, Result};

#[derive(Parser)]
#[command(name = "mooncake-http-client")]
#[command(about = "HTTP client for querying key existence in Mooncake Store")]
#[command(version)]
struct Cli {
    /// Mooncake server URL
    #[arg(short, long, default_value = "http://localhost:9003")]
    url: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check if a single key exists
    Check {
        /// Key to check
        #[arg(short, long)]
        key: String,
    },
    /// Check multiple keys (comma-separated)
    CheckBatch {
        /// Keys to check (comma-separated)
        #[arg(short, long)]
        keys: String,
    },
    /// List all keys in the store
    ListKeys,
    /// List all segments
    ListSegments,
    /// Check server health
    Health,
    /// Get Prometheus metrics
    Metrics,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let cli = Cli::parse();

    let client = MooncakeClient::new(&cli.url)?;
    println!("Connecting to Mooncake Store at: {}", cli.url);

    match cli.command {
        Commands::Check { key } => {
            println!("\n=== Single Key Query ===");
            println!("Checking if key '{}' exists...", key);
            match client.key_exists(&key).await {
                Ok(exists) => {
                    if exists {
                        println!("Key exists: true");
                    } else {
                        println!("Key exists: false");
                    }
                },
                Err(e) => println!("Query failed: {}", e),
            }
        },
        Commands::CheckBatch { keys } => {
            println!("\n=== Batch Key Query ===");
            let key_list: Vec<&str> = keys.split(',').map(|s| s.trim()).collect();
            println!("Checking keys: {:?}", key_list);
            match client.batch_keys_exist(&key_list).await {
                Ok(results) => {
                    for (key, result) in results {
                        println!("  {}: {}", key, result);
                    }
                },
                Err(e) => println!("Batch query failed: {}", e),
            }
        },
        Commands::ListKeys => {
            println!("\n=== Get All Keys ===");
            match client.get_all_keys().await {
                Ok(all_keys) => {
                    if all_keys.is_empty() {
                        println!("No keys found in the store");
                    } else {
                        println!("Total keys: {}", all_keys.len());
                        for key in all_keys {
                            println!("  - {}", key);
                        }
                    }
                },
                Err(e) => println!("Failed to get all keys: {}", e),
            }
        },
        Commands::ListSegments => {
            println!("\n=== Get All Segments ===");
            match client.get_all_segments().await {
                Ok(segments) => {
                    if segments.is_empty() {
                        println!("No segments found");
                    } else {
                        println!("Segments:");
                        for segment in segments {
                            println!("  - {}", segment);
                        }
                    }
                },
                Err(e) => println!("Failed to get all segments: {}", e),
            }
        },
        Commands::Health => {
            println!("\n=== Health Check ===");
            match client.health_check().await {
                Ok(health) => {
                    println!("Healthy: {}", health.healthy);
                    println!("Message: {}", health.message);
                },
                Err(e) => println!("Health check failed: {}", e),
            }
        },
        Commands::Metrics => {
            println!("\n=== Metrics ===");
            match client.get_metrics().await {
                Ok(metrics) => {
                    println!("{}", metrics);
                },
                Err(e) => println!("Failed to get metrics: {}", e),
            }
        },
    }

    Ok(())
}
