#![forbid(unsafe_code)]

use std::{
    collections::BTreeSet,
    fs,
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use bip0039::Mnemonic;
use clap::Parser;
use log::{LevelFilter, error, info, warn};
use log4rs::{
    append::{console::ConsoleAppender, file::FileAppender},
    config::{Appender, Config as LogConfig, Root},
    encode::pattern::PatternEncoder,
};
use pepper_sync::{
    activation::{set_orchard_activation_height, set_sapling_activation_height},
    config::{PerformanceLevel, SyncConfig, TransparentAddressDiscovery},
    keys::transparent::{self, TransparentAddressId, TransparentScope},
};
use rand::Rng;
use reqwest::blocking::Client;
use rpassword::prompt_password;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::runtime::Runtime;
use zcash_primitives::legacy::keys::NonHardenedChildIndex;
use zcash_protocol::consensus::BlockHeight;
use zingolib::{
    config::{self, ChainType, chain_from_str, construct_lightwalletd_uri},
    lightclient::{LightClient, error::LightClientError},
    wallet::{
        LightWallet, WalletBase, WalletSettings, restrictions::TransparentAddressRestriction,
    },
};
use zip32::AccountId;

#[derive(Parser, Debug)]
struct Cli {
    /// Path to the auto-shield configuration file (TOML)
    #[arg(long, default_value = "auto-shield-config.toml")]
    config: PathBuf,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e:?}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let cfg = AutoShieldConfig::from_file(&cli.config)?;
    init_logging(&cfg.log_file)?;

    if let Err(e) = rustls::crypto::ring::default_provider().install_default() {
        warn!("Error installing crypto provider: {e:?}");
    }

    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let signal_flag = shutdown_flag.clone();
    ctrlc::set_handler(move || {
        signal_flag.store(true, Ordering::SeqCst);
    })
    .context("installing Ctrl+C handler")?;

    let mut runner = AutoShieldRunner::initialize(cfg, shutdown_flag.clone())?;
    info!(
        "Auto-shield daemon started; polling every {:?}.",
        runner.config.poll_interval
    );

    while !shutdown_flag.load(Ordering::SeqCst) {
        if let Err(e) = runner.process_blocks() {
            error!("Processing pass failed: {e:?}");
        }

        if shutdown_flag.load(Ordering::SeqCst) {
            break;
        }

        thread::sleep(runner.config.poll_interval);
    }

    info!("Shutting down auto-shield daemon.");
    runner.shutdown()?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    mnemonic: Option<String>,
    birthday: u32,
    chain: String,
    lightwalletd_server: String,
    rpc_url: String,
    start_height: u32,
    confirmations: u32,
    state_dir: Option<PathBuf>,
    processed_log: Option<PathBuf>,
    rewards_log: Option<PathBuf>,
    queue_log: Option<PathBuf>,
    log_file: Option<PathBuf>,
    sapling_activation_height: Option<u32>,
    orchard_activation_height: Option<u32>,
    delay_min_minutes: Option<u32>,
    delay_max_minutes: Option<u32>,
    reschedule_grace_minutes: Option<u32>,
    poll_interval_seconds: Option<u64>,
}

#[derive(Debug)]
struct AutoShieldConfig {
    mnemonic: Option<String>,
    birthday: u32,
    chain: ChainType,
    lwd_uri: http::Uri,
    rpc_url: String,
    start_height: u32,
    confirmations: u32,
    wallet_dir: PathBuf,
    processed_log: PathBuf,
    rewards_log: PathBuf,
    queue_log: PathBuf,
    log_file: PathBuf,
    wallet_settings: WalletSettings,
    delay_min_minutes: u32,
    delay_max_minutes: u32,
    reschedule_grace_secs: u64,
    poll_interval: Duration,
}

impl AutoShieldConfig {
    fn from_file(path: &Path) -> Result<Self> {
        let config_text = fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let file_cfg: FileConfig = toml::from_str(&config_text)
            .with_context(|| format!("parsing config file {}", path.display()))?;

        if let Some(height) = file_cfg.sapling_activation_height {
            set_sapling_activation_height(BlockHeight::from_u32(height));
        }
        if let Some(height) = file_cfg.orchard_activation_height {
            set_orchard_activation_height(BlockHeight::from_u32(height));
        }

        let state_dir = file_cfg.state_dir.unwrap_or(default_state_dir());
        fs::create_dir_all(&state_dir)
            .with_context(|| format!("creating state directory {}", state_dir.display()))?;

        let processed_log = resolve_path(
            &state_dir,
            file_cfg
                .processed_log
                .unwrap_or_else(|| "processed_blocks.json".into()),
        );
        let rewards_log = resolve_path(
            &state_dir,
            file_cfg
                .rewards_log
                .unwrap_or_else(|| "reward_blocks.json".into()),
        );
        let queue_log = resolve_path(
            &state_dir,
            file_cfg
                .queue_log
                .unwrap_or_else(|| "shield_queue.json".into()),
        );
        let log_file = resolve_path(
            &state_dir,
            file_cfg
                .log_file
                .unwrap_or_else(|| "auto-shield.log".into()),
        );
        let wallet_dir = state_dir.join("wallet");

        let delay_min = file_cfg.delay_min_minutes.unwrap_or(200);
        let delay_max = file_cfg.delay_max_minutes.unwrap_or(500);
        if delay_min == 0 {
            return Err(anyhow!("delay_min_minutes must be > 0"));
        }
        if delay_max < delay_min {
            return Err(anyhow!(
                "delay_max_minutes ({delay_max}) must be >= delay_min_minutes ({delay_min})"
            ));
        }
        let grace_minutes = file_cfg.reschedule_grace_minutes.unwrap_or(5);
        let poll_interval_secs = file_cfg.poll_interval_seconds.unwrap_or(60).max(1);

        let chain = chain_from_str(&file_cfg.chain)
            .map_err(|e| anyhow!("invalid chain '{}': {}", file_cfg.chain, e))?;
        let lwd_uri = construct_lightwalletd_uri(Some(file_cfg.lightwalletd_server));

        let wallet_settings = WalletSettings {
            sync_config: SyncConfig {
                transparent_address_discovery: TransparentAddressDiscovery::minimal(),
                performance_level: PerformanceLevel::High,
            },
            min_confirmations: NonZeroU32::try_from(3).expect("non-zero"),
        };

        Ok(Self {
            mnemonic: file_cfg.mnemonic.filter(|m| !m.trim().is_empty()),
            birthday: file_cfg.birthday,
            chain,
            lwd_uri,
            rpc_url: file_cfg.rpc_url,
            start_height: file_cfg.start_height,
            confirmations: file_cfg.confirmations.max(1),
            wallet_dir,
            processed_log,
            rewards_log,
            queue_log,
            log_file,
            wallet_settings,
            delay_min_minutes: delay_min,
            delay_max_minutes: delay_max,
            reschedule_grace_secs: grace_minutes as u64 * 60,
            poll_interval: Duration::from_secs(poll_interval_secs),
        })
    }

    fn random_delay_secs(&self) -> u64 {
        let mut rng = rand::thread_rng();
        let minutes = rng.gen_range(self.delay_min_minutes..=self.delay_max_minutes) as u64;
        minutes * 60
    }
}

fn default_state_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".zcash")
        .join("zingo-auto-shield")
}

fn resolve_path(base: &Path, candidate: PathBuf) -> PathBuf {
    if candidate.is_absolute() {
        candidate
    } else {
        base.join(candidate)
    }
}

fn init_logging(log_path: &Path) -> Result<()> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating log directory {}", parent.display()))?;
    }

    let file_appender = FileAppender::builder()
        .encoder(Box::new(PatternEncoder::new(
            "{d(%Y-%m-%d %H:%M:%S)} {l:<5} {m}{n}",
        )))
        .build(log_path)
        .with_context(|| format!("opening log file {}", log_path.display()))?;

    let console_appender = ConsoleAppender::builder().build();

    let config = LogConfig::builder()
        .appender(Appender::builder().build("stdout", Box::new(console_appender)))
        .appender(Appender::builder().build("logfile", Box::new(file_appender)))
        .build(
            Root::builder()
                .appender("stdout")
                .appender("logfile")
                .build(LevelFilter::Info),
        )
        .context("building logger config")?;

    log4rs::init_config(config).context("initializing logger")?;
    info!("Writing logs to {}", log_path.display());
    Ok(())
}

struct AutoShieldRunner {
    config: AutoShieldConfig,
    runtime: Runtime,
    lightclient: LightClient,
    rpc: NodeRpcClient,
    processed: ProcessedBlocks,
    rewards: RewardLog,
    queue: ShieldQueue,
    needs_reward_rescan: bool,
    shutdown_flag: Arc<AtomicBool>,
}

impl AutoShieldRunner {
    fn initialize(mut config: AutoShieldConfig, shutdown_flag: Arc<AtomicBool>) -> Result<Self> {
        let runtime = Runtime::new().context("creating tokio runtime")?;

        fs::create_dir_all(&config.wallet_dir)
            .with_context(|| format!("creating wallet dir {}", config.wallet_dir.display()))?;

        let lw_config = config::load_clientconfig(
            config.lwd_uri.clone(),
            Some(config.wallet_dir.clone()),
            config.chain,
            config.wallet_settings.clone(),
            NonZeroU32::try_from(1).expect("non-zero"),
        )
        .context("loading client config")?;

        let mut lightclient = if lw_config.wallet_path_exists() {
            LightClient::create_from_wallet_path(lw_config.clone())
                .context("loading existing wallet")?
        } else {
            let phrase = config
                .mnemonic
                .take()
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .map(Ok)
                .unwrap_or_else(|| prompt_for_mnemonic())?;
            let mnemonic = Mnemonic::from_phrase(phrase).context("parsing mnemonic")?;
            let wallet = LightWallet::new(
                config.chain,
                WalletBase::Mnemonic {
                    mnemonic,
                    no_of_accounts: NonZeroU32::try_from(1).expect("non-zero"),
                },
                config.birthday.into(),
                config.wallet_settings.clone(),
            )
            .context("creating wallet from mnemonic")?;
            LightClient::create_from_wallet(wallet, lw_config.clone(), false)
                .context("creating lightclient from wallet")?
        };

        runtime.block_on(lightclient.save_task());
        run_wallet_sync(
            &runtime,
            &mut lightclient,
            WalletSyncOp::Sync,
            "initial sync",
        )?;

        let processed = ProcessedBlocks::load(&config.processed_log)?;
        let rewards = RewardLog::load(&config.rewards_log)?;
        let queue = ShieldQueue::load(&config.queue_log)?;

        let mut runner = Self {
            rpc: NodeRpcClient::new(&config.rpc_url)?,
            config,
            runtime,
            lightclient,
            processed,
            rewards,
            queue,
            needs_reward_rescan: false,
            shutdown_flag,
        };

        runner.ensure_tracked_indices_for_queue()?;

        if !runner.queue.pending.is_empty() {
            info!(
                "Queued shields detected on startup; running rescan to backfill transparent rewards."
            );
            run_wallet_sync(
                &runner.runtime,
                &mut runner.lightclient,
                WalletSyncOp::Rescan,
                "startup rescan for existing shield queue",
            )?;
        }
        Ok(runner)
    }

    fn should_shutdown(&self) -> bool {
        self.shutdown_flag.load(Ordering::SeqCst)
    }

    fn shutdown(&mut self) -> Result<()> {
        self.runtime
            .block_on(self.lightclient.shutdown_save_task())
            .context("shutting down wallet")?;
        Ok(())
    }

    fn rescan_tracked_rewards_if_needed(&mut self) -> Result<()> {
        if !self.needs_reward_rescan {
            return Ok(());
        }
        info!(
            "New transparent reward addresses detected; rescanning wallet to import historical UTXOs."
        );
        run_wallet_sync(
            &self.runtime,
            &mut self.lightclient,
            WalletSyncOp::Rescan,
            "rescanning after tracking new reward addresses",
        )?;
        self.needs_reward_rescan = false;
        Ok(())
    }

    fn process_blocks(&mut self) -> Result<()> {
        if self.should_shutdown() {
            return Ok(());
        }

        let tip_height = self.rpc.block_count().context("fetching block count")?;
        if tip_height < self.config.start_height {
            info!(
                "Tip height {tip_height} is below configured start height {}; nothing to do.",
                self.config.start_height
            );
            return Ok(());
        }

        let confirmation_gap = self.config.confirmations.saturating_sub(1);
        if tip_height <= confirmation_gap {
            info!(
                "Not enough blocks yet for confirmation requirement {}",
                self.config.confirmations
            );
            return Ok(());
        }

        let target_height = tip_height - confirmation_gap;

        let mut next_height = self.next_height();
        if next_height < self.config.start_height {
            next_height = self.config.start_height;
        }
        if next_height > target_height {
            info!(
                "No finalized blocks to examine (next_height={next_height}, target={target_height})."
            );
        } else {
            info!(
                "Scanning blocks [{}, {}] ({} confirmations).",
                next_height, target_height, self.config.confirmations
            );

            for height in next_height..=target_height {
                if self.should_shutdown() {
                    break;
                }
                if let Err(e) = self.process_height(height) {
                    error!("Failed processing block {height}: {e:?}");
                }
            }
        }

        if !self.should_shutdown() {
            self.rescan_tracked_rewards_if_needed()?;
        }
        if !self.should_shutdown() {
            self.process_queue()?;
        }
        Ok(())
    }

    fn next_height(&self) -> u32 {
        self.processed
            .blocks
            .iter()
            .copied()
            .max()
            .map(|h| h.saturating_add(1))
            .unwrap_or(self.config.start_height)
    }

    fn process_height(&mut self, height: u32) -> Result<()> {
        if self.should_shutdown() {
            return Ok(());
        }

        let block_hash = self
            .rpc
            .block_hash(height)
            .with_context(|| format!("fetching block hash for height {height}"))?;
        let block = self
            .rpc
            .block(&block_hash)
            .with_context(|| format!("fetching block data for height {height}"))?;

        let derived_address = self.derive_transparent_address(height)?;
        let coinbase = block.coinbase_addresses();

        let matched_outputs: Vec<_> = coinbase
            .iter()
            .filter(|output| output.addresses.contains(&derived_address))
            .collect();

        if !matched_outputs.is_empty() {
            info!("Height {height}: detected reward for derived address {derived_address}");
            if self.rewards.contains_height(height) {
                info!("Height {height} already recorded as shielded; skipping schedule.");
            } else if self.queue.contains_height(height) {
                info!("Height {height} already queued for shielding.");
            } else {
                let tracked_new_index = self.track_transparent_index(height)?;
                if tracked_new_index {
                    self.needs_reward_rescan = true;
                }
                let total_value: i64 = matched_outputs.iter().map(|o| o.value_zat).sum();
                self.schedule_shield(height, &block_hash, &derived_address, total_value)?;
            }
        }

        self.processed.blocks.push(height);
        self.processed.save(&self.config.processed_log)?;
        Ok(())
    }

    fn schedule_shield(
        &mut self,
        height: u32,
        block_hash: &str,
        address: &str,
        value_zat: i64,
    ) -> Result<()> {
        let scheduled_at = unix_timestamp();
        let execute_after = scheduled_at + self.config.random_delay_secs();
        let delay_minutes = (execute_after - scheduled_at) / 60;

        self.queue.pending.push(QueuedShield {
            height,
            block_hash: block_hash.to_string(),
            address: address.to_string(),
            value_zat,
            scheduled_at,
            execute_after,
        });
        self.queue.save(&self.config.queue_log)?;

        info!(
            "Queued height {height} for shielding in ~{delay_minutes} minutes (exec at {}).",
            execute_after
        );
        Ok(())
    }

    fn process_queue(&mut self) -> Result<()> {
        if self.queue.pending.is_empty() || self.should_shutdown() {
            return Ok(());
        }

        let now = unix_timestamp();
        let mut retained = Vec::new();
        let mut due = Vec::new();

        for entry in self.queue.pending.drain(..) {
            if entry.execute_after > now {
                retained.push(entry);
            } else {
                due.push(entry);
            }
        }

        self.queue.pending = retained;

        let mut due_iter = due.into_iter();
        while let Some(mut entry) = due_iter.next() {
            if self.should_shutdown() {
                self.queue.pending.push(entry);
                self.queue.pending.extend(due_iter);
                self.queue.save(&self.config.queue_log)?;
                return Ok(());
            }

            if self.rewards.contains_height(entry.height) {
                info!(
                    "Queue entry for height {} already satisfied; skipping.",
                    entry.height
                );
                continue;
            }

            let overdue_secs = now.saturating_sub(entry.execute_after);
            if overdue_secs > self.config.reschedule_grace_secs {
                entry.execute_after = now + self.config.random_delay_secs();
                info!(
                    "Queue entry for height {} overdue by {}s; re-randomizing execution to {}.",
                    entry.height, overdue_secs, entry.execute_after
                );
                self.queue.pending.push(entry);
                continue;
            }

            match self.trigger_shield(entry.height) {
                Ok(txids) => {
                    let executed_at = unix_timestamp();
                    self.rewards.entries.push(RewardEntry {
                        height: entry.height,
                        block_hash: entry.block_hash.clone(),
                        address: entry.address.clone(),
                        value_zat: entry.value_zat,
                        txids,
                        scheduled_at: Some(entry.scheduled_at),
                        executed_at: Some(executed_at),
                    });
                    self.rewards.save(&self.config.rewards_log)?;
                    info!(
                        "Shielded queued reward at height {} (scheduled {}, executed {}).",
                        entry.height, entry.scheduled_at, executed_at
                    );
                }
                Err(e) => {
                    error!(
                        "Shielding queued reward at height {} failed: {e:?}. Rescheduling.",
                        entry.height
                    );
                    let err_txt = e.to_string();
                    let delay_secs = if err_txt.contains("InsufficientFunds") {
                        let extra_minutes =
                            self.config.delay_max_minutes.saturating_add(150) as u64;
                        info!(
                            "Height {} lacked mature funds; delaying {} additional minutes.",
                            entry.height, extra_minutes
                        );
                        extra_minutes * 60
                    } else if is_missing_backend_transaction(&err_txt) {
                        let retry_minutes = self.config.delay_min_minutes.max(30) as u64;
                        if let Some(txid) = extract_txid_from_error(&err_txt) {
                            warn!(
                                "Backend could not supply transaction {txid} for height {}. Backend reindex or txindex=1 may be required; retrying in {} minutes.",
                                entry.height, retry_minutes
                            );
                        } else {
                            warn!(
                                "Backend missing transaction data for height {}; retrying in {} minutes.",
                                entry.height, retry_minutes
                            );
                        }
                        retry_minutes * 60
                    } else {
                        self.config.random_delay_secs()
                    };
                    entry.execute_after = unix_timestamp() + delay_secs;
                    self.queue.pending.push(entry);
                }
            }
        }

        self.queue.save(&self.config.queue_log)?;
        Ok(())
    }

    fn derive_transparent_address(&self, index: u32) -> Result<String> {
        let wallet = self.runtime.block_on(self.lightclient.wallet.read());
        let nh_index = NonHardenedChildIndex::from_index(index)
            .ok_or_else(|| anyhow!("index {index} exceeds NonHardenedChildIndex bounds"))?;
        let key_store = wallet
            .unified_key_store
            .get(&AccountId::ZERO)
            .ok_or_else(|| anyhow!("account 0 key store missing"))?;
        let raw_address = key_store
            .generate_transparent_address(nh_index, TransparentScope::External)
            .context("deriving transparent address")?;
        Ok(transparent::encode_address(&wallet.network, raw_address))
    }

    fn track_transparent_index(&mut self, index: u32) -> Result<bool> {
        let nh_index = NonHardenedChildIndex::from_index(index)
            .ok_or_else(|| anyhow!("index {index} exceeds NonHardenedChildIndex bounds"))?;

        let already_known = self.runtime.block_on(async {
            let wallet = self.lightclient.wallet.read().await;
            let id =
                TransparentAddressId::new(AccountId::ZERO, TransparentScope::External, nh_index);
            wallet.transparent_addresses().contains_key(&id)
        });

        if already_known {
            return Ok(false);
        }

        self.runtime
            .block_on(self.lightclient.generate_transparent_address(
                AccountId::ZERO,
                false,
                Some(index),
            ))
            .with_context(|| format!("tracking transparent index {index}"))?;
        Ok(true)
    }

    fn ensure_tracked_indices_for_queue(&mut self) -> Result<()> {
        let mut seen = BTreeSet::new();
        let mut derived_new = false;
        let heights: Vec<u32> = self
            .queue
            .pending
            .iter()
            .map(|entry| entry.height)
            .collect();
        for height in heights {
            if seen.insert(height) && self.track_transparent_index(height)? {
                derived_new = true;
            }
        }

        if derived_new {
            info!(
                "Derived new transparent indices for queued rewards; running rescan to backfill UTXOs."
            );
            run_wallet_sync(
                &self.runtime,
                &mut self.lightclient,
                WalletSyncOp::Rescan,
                "rescanning after deriving transparent backlog",
            )?;
        }

        Ok(())
    }

    fn trigger_shield(&mut self, index: u32) -> Result<Vec<String>> {
        info!("Triggering quick_shield for index {index}.");
        run_wallet_sync(
            &self.runtime,
            &mut self.lightclient,
            WalletSyncOp::Sync,
            "sync before shielding",
        )?;

        let restriction = TransparentAddressRestriction::new(
            AccountId::ZERO,
            TransparentScope::External,
            index,
            Some(index),
        );

        let txids = self
            .runtime
            .block_on(
                self.lightclient
                    .quick_shield(AccountId::ZERO, Some(restriction)),
            )
            .map_err(|e| anyhow!("quick_shield failed: {e:?}"))?;

        run_wallet_sync(
            &self.runtime,
            &mut self.lightclient,
            WalletSyncOp::Sync,
            "sync after shielding",
        )?;

        Ok(txids.into_iter().map(|txid| txid.to_string()).collect())
    }
}

enum WalletSyncOp {
    Sync,
    Rescan,
}

fn run_wallet_sync(
    runtime: &Runtime,
    lightclient: &mut LightClient,
    op: WalletSyncOp,
    context: &'static str,
) -> Result<()> {
    run_wallet_sync_inner(runtime, lightclient, op, context, true)
}

fn run_wallet_sync_inner(
    runtime: &Runtime,
    lightclient: &mut LightClient,
    op: WalletSyncOp,
    context: &'static str,
    allow_note_recovery: bool,
) -> Result<()> {
    let result = match op {
        WalletSyncOp::Sync => runtime.block_on(lightclient.sync_and_await()),
        WalletSyncOp::Rescan => runtime.block_on(lightclient.rescan_and_await()),
    };

    match result {
        Ok(_) => Ok(()),
        Err(err) => {
            handle_wallet_sync_error(runtime, lightclient, context, err, allow_note_recovery)
        }
    }
}

fn handle_wallet_sync_error(
    runtime: &Runtime,
    lightclient: &mut LightClient,
    context: &'static str,
    err: LightClientError,
    allow_note_recovery: bool,
) -> Result<()> {
    let err_text = err.to_string();
    if is_missing_backend_transaction(&err_text) {
        log_missing_backend_transaction(context, &err_text);
        Ok(())
    } else if allow_note_recovery && is_missing_note_metadata(&err_text) {
        warn!(
            "{context} hit missing decrypted note metadata ({err_text}); running full rescan and continuing."
        );
        run_wallet_sync_inner(
            runtime,
            lightclient,
            WalletSyncOp::Rescan,
            "recovery rescan after missing note metadata",
            false,
        )
        .with_context(|| format!("{context} recovery rescan"))?;
        Ok(())
    } else {
        Err::<(), LightClientError>(err).context(context)
    }
}

fn log_missing_backend_transaction(context: &str, err_text: &str) {
    if let Some(txid) = extract_txid_from_error(err_text) {
        warn!(
            "{context} hit missing backend transaction {txid}; backend reindex or txindex=1 may be required. Continuing."
        );
    } else {
        warn!(
            "{context} hit missing backend transaction data; backend reindex or txindex=1 may be required. Continuing. ({err_text})"
        );
    }
}

fn is_missing_backend_transaction(err_text: &str) -> bool {
    err_text.contains("No such mempool or main chain transaction")
}

fn is_missing_note_metadata(err_text: &str) -> bool {
    err_text.contains("decrypted note nullifier and position data not found")
}

fn extract_txid_from_error(err_text: &str) -> Option<String> {
    err_text
        .split(|c: char| c == ' ' || c == '"' || c == '\n' || c == ':' || c == ',')
        .find(|token| token.len() == 64 && token.chars().all(|ch| ch.is_ascii_hexdigit()))
        .map(ToOwned::to_owned)
}

#[derive(Default, Serialize, Deserialize)]
struct ProcessedBlocks {
    blocks: Vec<u32>,
}

impl ProcessedBlocks {
    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_slice(&data).with_context(|| format!("parsing {}", path.display()))
    }

    fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        fs::write(path, json).with_context(|| format!("writing {}", path.display()))
    }
}

#[derive(Default, Serialize, Deserialize)]
struct RewardLog {
    entries: Vec<RewardEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct RewardEntry {
    height: u32,
    block_hash: String,
    address: String,
    value_zat: i64,
    txids: Vec<String>,
    #[serde(default)]
    scheduled_at: Option<u64>,
    #[serde(default)]
    executed_at: Option<u64>,
}

impl RewardLog {
    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_slice(&data).with_context(|| format!("parsing {}", path.display()))
    }

    fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        fs::write(path, json).with_context(|| format!("writing {}", path.display()))
    }

    fn contains_height(&self, height: u32) -> bool {
        self.entries.iter().any(|entry| entry.height == height)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct ShieldQueue {
    pending: Vec<QueuedShield>,
}

impl ShieldQueue {
    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_slice(&data).with_context(|| format!("parsing {}", path.display()))
    }

    fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        fs::write(path, json).with_context(|| format!("writing {}", path.display()))
    }

    fn contains_height(&self, height: u32) -> bool {
        self.pending.iter().any(|entry| entry.height == height)
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct QueuedShield {
    height: u32,
    block_hash: String,
    address: String,
    value_zat: i64,
    scheduled_at: u64,
    execute_after: u64,
}

struct NodeRpcClient {
    client: Client,
    url: String,
}

impl NodeRpcClient {
    fn new(url: &str) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .context("constructing RPC client")?;
        Ok(Self {
            client,
            url: url.to_string(),
        })
    }

    fn block_count(&self) -> Result<u32> {
        self.call_method::<u32>("getblockcount", Value::Array(vec![]))
    }

    fn block_hash(&self, height: u32) -> Result<String> {
        let params = Value::Array(vec![Value::from(height)]);
        self.call_method("getblockhash", params)
    }

    fn block(&self, hash: &str) -> Result<BlockResult> {
        let params = Value::Array(vec![Value::from(hash), Value::from(2)]);
        self.call_method("getblock", params)
    }

    fn call_method<T: for<'a> Deserialize<'a>>(&self, method: &str, params: Value) -> Result<T> {
        let request = RpcRequest {
            jsonrpc: "2.0",
            id: "zingo-auto-shield",
            method,
            params,
        };
        let response = self
            .client
            .post(&self.url)
            .json(&request)
            .send()
            .with_context(|| format!("calling RPC method {method}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!("RPC {method} failed: HTTP {status}"));
        }
        let rpc_response: RpcResponse<T> = response.json().context("parsing RPC response")?;
        if let Some(err) = rpc_response.error {
            return Err(anyhow!("RPC error {}: {}", err.code, err.message));
        }
        rpc_response
            .result
            .ok_or_else(|| anyhow!("RPC {method} returned no result"))
    }
}

#[derive(Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'a str,
    id: &'a str,
    method: &'a str,
    params: Value,
}

#[derive(Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<RpcError>,
    #[allow(dead_code)]
    id: Value,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Deserialize)]
struct BlockResult {
    hash: String,
    height: u32,
    tx: Vec<BlockTx>,
}

impl BlockResult {
    fn coinbase_addresses(&self) -> Vec<CoinbaseOutput> {
        if let Some(tx) = self.tx.first() {
            tx.vout
                .iter()
                .map(|vout| CoinbaseOutput {
                    value_zat: vout.value_zat,
                    addresses: vout.script_pub_key.addresses.clone().unwrap_or_default(),
                })
                .collect()
        } else {
            vec![]
        }
    }
}

#[derive(Deserialize)]
struct BlockTx {
    vout: Vec<BlockVout>,
}

#[derive(Deserialize)]
struct BlockVout {
    #[serde(rename = "valueZat")]
    value_zat: i64,
    #[serde(rename = "scriptPubKey")]
    script_pub_key: ScriptPubKey,
}

#[derive(Deserialize)]
struct ScriptPubKey {
    addresses: Option<Vec<String>>,
}

struct CoinbaseOutput {
    value_zat: i64,
    addresses: Vec<String>,
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
}

fn prompt_for_mnemonic() -> Result<String> {
    eprintln!("Enter wallet mnemonic (input hidden, not stored):");
    let phrase = prompt_password("mnemonic> ").context("reading mnemonic from stdin")?;
    let trimmed = phrase.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("mnemonic input cannot be empty"));
    }
    Ok(trimmed.to_string())
}
