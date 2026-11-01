//! `relay` 命令行入口。

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use shio_relay::db::Db;
use shio_relay::error::RelayError;
use shio_relay::{server, store, util, Config};

#[derive(Debug, Parser)]
#[command(name = "relay", version, about = "shio content-agnostic relay")]
struct Cli {
    /// `relay.toml` 路径。
    #[arg(long, global = true, value_name = "PATH", default_value = "relay.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 启动中继服务器。
    Serve,
    /// channel 运维。
    Channel {
        #[command(subcommand)]
        command: ChannelCommand,
    },
    /// device 运维。
    Device {
        #[command(subcommand)]
        command: DeviceCommand,
    },
    /// token 运维。
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    /// membership 运维。
    Member {
        #[command(subcommand)]
        command: MemberCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ChannelCommand {
    /// 新建 channel。
    New {
        /// 指定 32 位 hex；缺省随机生成。
        #[arg(long)]
        id: Option<String>,
        /// 配额字节数；缺省用配置。
        #[arg(long)]
        quota_bytes: Option<u64>,
        /// 帧 TTL 秒数；缺省用配置。
        #[arg(long)]
        ttl_secs: Option<u64>,
    },
    /// 列出 channel。
    List,
}

#[derive(Debug, Subcommand)]
enum DeviceCommand {
    /// 新建 device 并输出 token（只显示一次）。
    New {
        /// 指定 32 位 hex；缺省随机生成。
        #[arg(long)]
        device_id: Option<String>,
    },
    /// 全局撤销 device。
    Revoke {
        /// device ID。
        #[arg(long)]
        device: String,
    },
}

#[derive(Debug, Subcommand)]
enum TokenCommand {
    /// 轮换 device token；旧 token 立即失效。
    Rotate {
        /// device ID。
        #[arg(long)]
        device: String,
    },
}

#[derive(Debug, Subcommand)]
enum MemberCommand {
    /// 添加 membership。
    Add {
        /// channel ID。
        #[arg(long)]
        channel: String,
        /// device ID。
        #[arg(long)]
        device: String,
    },
    /// 移除 membership。
    Remove {
        /// channel ID。
        #[arg(long)]
        channel: String,
        /// device ID。
        #[arg(long)]
        device: String,
    },
}

#[tokio::main]
async fn main() {
    init_tracing();
    if let Err(err) = run().await {
        tracing::error!(%err, "relay failed");
        std::process::exit(1);
    }
}

async fn run() -> shio_relay::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve => {
            let config = Config::load(&cli.config)?;
            server::serve(config).await
        }
        Command::Channel { command } => {
            let config = Config::load(&cli.config)?;
            let db = Db::open(&config.data)?;
            match command {
                ChannelCommand::New {
                    id,
                    quota_bytes,
                    ttl_secs,
                } => channel_new(&config, &db, id, quota_bytes, ttl_secs),
                ChannelCommand::List => channel_list(&db),
            }
        }
        Command::Device { command } => {
            let config = Config::load(&cli.config)?;
            let db = Db::open(&config.data)?;
            match command {
                DeviceCommand::New { device_id } => device_new(&db, device_id),
                DeviceCommand::Revoke { device } => device_revoke(&db, &device),
            }
        }
        Command::Token { command } => {
            let config = Config::load(&cli.config)?;
            let db = Db::open(&config.data)?;
            match command {
                TokenCommand::Rotate { device } => token_rotate(&db, &device),
            }
        }
        Command::Member { command } => {
            let config = Config::load(&cli.config)?;
            let db = Db::open(&config.data)?;
            match command {
                MemberCommand::Add { channel, device } => member_add(&db, &channel, &device),
                MemberCommand::Remove { channel, device } => member_remove(&db, &channel, &device),
            }
        }
    }
}

fn channel_new(
    config: &Config,
    db: &Db,
    id: Option<String>,
    quota_bytes: Option<u64>,
    ttl_secs: Option<u64>,
) -> shio_relay::Result<()> {
    let channel_id = match id {
        Some(value) => normalize_id(&value, "channel id must be 32 hex characters")?,
        None => util::random_hex_id()?,
    };
    let quota = i64::try_from(quota_bytes.unwrap_or(config.channel_quota_bytes))
        .map_err(|_| RelayError::InvalidArgument("quota out of range"))?;
    let ttl = i64::try_from(ttl_secs.unwrap_or(config.frame_ttl_secs))
        .map_err(|_| RelayError::InvalidArgument("ttl out of range"))?;
    let max_payload = i64::try_from(config.max_payload_bytes)
        .map_err(|_| RelayError::InvalidArgument("max payload out of range"))?;
    db.with_conn(|conn| {
        store::insert_channel(
            conn,
            &channel_id,
            quota,
            Some(ttl),
            Some(max_payload),
            util::now_secs(),
        )
    })?;
    println!("{channel_id}");
    Ok(())
}

fn channel_list(db: &Db) -> shio_relay::Result<()> {
    let channels = db.with_conn(store::list_channels)?;
    for channel in channels {
        println!(
            "{}\tnext={}\tretained={}\tbytes={}\tquota={}",
            channel.channel_id,
            channel.next_seq,
            channel.retained_from,
            channel.bytes,
            channel.quota_bytes
        );
    }
    Ok(())
}

fn device_new(db: &Db, device_id: Option<String>) -> shio_relay::Result<()> {
    let device_id = match device_id {
        Some(value) => normalize_id(&value, "device id must be 32 hex characters")?,
        None => util::random_hex_id()?,
    };
    let token = util::random_secret()?;
    let token_hash = util::sha256_hex(token.as_bytes());
    db.with_conn(|conn| store::insert_device(conn, &device_id, &token_hash, util::now_secs()))?;
    println!("device_id: {device_id}");
    println!("token: {token}");
    Ok(())
}

fn device_revoke(db: &Db, device: &str) -> shio_relay::Result<()> {
    let device_id = normalize_id(device, "device id must be 32 hex characters")?;
    let revoked = db.with_conn(|conn| store::revoke_device(conn, &device_id, util::now_secs()))?;
    println!("revoked={revoked}");
    Ok(())
}

fn token_rotate(db: &Db, device: &str) -> shio_relay::Result<()> {
    let device_id = normalize_id(device, "device id must be 32 hex characters")?;
    let token = util::random_secret()?;
    let token_hash = util::sha256_hex(token.as_bytes());
    let rotated = db.with_conn(|conn| store::rotate_token(conn, &device_id, &token_hash))?;
    if !rotated {
        return Err(RelayError::NotFound);
    }
    println!("token: {token}");
    Ok(())
}

fn member_add(db: &Db, channel: &str, device: &str) -> shio_relay::Result<()> {
    let channel_id = normalize_id(channel, "channel id must be 32 hex characters")?;
    let device_id = normalize_id(device, "device id must be 32 hex characters")?;
    db.with_conn(|conn| store::upsert_membership(conn, &channel_id, &device_id, util::now_secs()))?;
    println!("added channel={channel_id} device={device_id}");
    Ok(())
}

fn member_remove(db: &Db, channel: &str, device: &str) -> shio_relay::Result<()> {
    let channel_id = normalize_id(channel, "channel id must be 32 hex characters")?;
    let device_id = normalize_id(device, "device id must be 32 hex characters")?;
    let removed = db.with_conn(|conn| {
        store::revoke_membership(conn, &channel_id, &device_id, util::now_secs())
    })?;
    println!("removed={removed}");
    Ok(())
}

fn normalize_id(value: &str, message: &'static str) -> shio_relay::Result<String> {
    let value = value.to_ascii_lowercase();
    if !util::is_hex_id(&value) {
        return Err(RelayError::InvalidArgument(message));
    }
    Ok(value)
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
